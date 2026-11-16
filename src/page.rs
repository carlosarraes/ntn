//! Load a page's block tree and render it as Markdown.

use crate::client::Client;
use crate::client::get;
use crate::error::{AppError, Result};
use crate::ids::url_for;
use crate::records::{Store, collection_id, markdown, ms_to_iso, plain, refs};
use serde_json::{Map, Value, json};

const MAX_CHUNKS: u32 = 50;
const MAX_FETCH_ROUNDS: usize = 20;

#[derive(Clone, Copy, Default)]
pub struct Opts {
    /// Include meeting transcripts (can be thousands of lines)
    pub transcript: bool,
}

fn is_page_like(t: &str) -> bool {
    matches!(t, "page" | "collection_view_page" | "meeting_note_page")
}

fn children(b: &Value) -> Vec<String> {
    b["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn pointer(b: &Value, key: &str) -> Option<String> {
    b["format"][key]["id"].as_str().map(str::to_string)
}

fn fmt_id(b: &Value, key: &str) -> Option<String> {
    b["format"][key].as_str().map(str::to_string)
}

/// Blocks never walked: the transcript of a meeting note unless asked for.
fn skipped(store: &Store, page_id: &str, opts: Opts) -> Vec<String> {
    let mut skip = Vec::new();
    if !opts.transcript
        && let Some(t) = store
            .block(page_id)
            .and_then(|b| fmt_id(b, "transcription_transcript_id"))
    {
        skip.push(t);
    }
    skip
}

/// Pulls the page via loadPageChunk, then backfills any descendants,
/// synced-block sources, mentioned users/pages and child databases.
pub fn load(client: &Client, store: &mut Store, page_id: &str, opts: Opts) -> Result<()> {
    let mut cursor = json!({ "stack": [] });
    for chunk in 0..MAX_CHUNKS {
        let r = client.call(
            "loadPageChunk",
            json!({
                "pageId": page_id, "limit": 100, "cursor": cursor,
                "chunkNumber": chunk, "verticalColumns": false
            }),
        )?;
        store.merge(&r["recordMap"]);
        cursor = r["cursor"].clone();
        if cursor["stack"].as_array().is_none_or(Vec::is_empty) {
            break;
        }
    }
    if store.block(page_id).is_none() {
        store.fetch_missing(client, "block", &[page_id.to_string()])?;
    }
    let Some(root) = store.block(page_id).cloned() else {
        return Err(AppError::not_found(format!(
            "page {page_id} not found or not shared with you"
        )));
    };
    if root["parent_table"] == "collection"
        && let Some(cid) = root["parent_id"].as_str()
    {
        store.fetch_missing(client, "collection", &[cid.to_string()])?;
    }
    let skip = skipped(store, page_id, opts);
    for _ in 0..MAX_FETCH_ROUNDS {
        let (mut blocks, mut users, mut collections) = (vec![], vec![], vec![]);
        walk(store, page_id, true, &skip, &mut |b| {
            let p = &b["properties"];
            for v in p.as_object().into_iter().flat_map(|o| o.values()) {
                refs(v, &mut users, &mut blocks);
            }
            if !is_page_like(b["type"].as_str().unwrap_or_default()) || b["id"] == page_id {
                blocks.extend(children(b));
            }
            blocks.extend(pointer(b, "transclusion_reference_pointer"));
            blocks.extend(pointer(b, "alias_pointer"));
            collections.extend(collection_id(b));
            users.extend(b["last_edited_by_id"].as_str().map(str::to_string));
        });
        blocks.retain(|id| !skip.contains(id));
        let before = store.map.to_string().len();
        store.fetch_missing(client, "block", &blocks)?;
        store.fetch_missing(client, "notion_user", &users)?;
        store.fetch_missing(client, "collection", &collections)?;
        if store.map.to_string().len() == before {
            break;
        }
    }
    Ok(())
}

/// Visits loaded blocks under `id` in document order without descending into
/// child pages or skipped subtrees.
fn walk(store: &Store, id: &str, root: bool, skip: &[String], f: &mut dyn FnMut(&Value)) {
    if skip.iter().any(|s| s == id) {
        return;
    }
    let Some(b) = store.block(id) else { return };
    f(b);
    let t = b["type"].as_str().unwrap_or_default();
    if is_page_like(t) && !root {
        return;
    }
    let mut ids = children(b);
    ids.extend(pointer(b, "transclusion_reference_pointer"));
    for c in ids {
        walk(store, &c, false, skip, f);
    }
}

/// A page's own properties (database columns), excluding the title.
pub fn properties(store: &Store, page_id: &str) -> Map<String, Value> {
    let Some(b) = store.block(page_id) else {
        return Map::new();
    };
    let schema = (b["parent_table"] == "collection")
        .then(|| b["parent_id"].as_str())
        .flatten()
        .and_then(|cid| get(&store.map, "collection", cid))
        .map(|c| c["schema"].clone())
        .filter(Value::is_object);
    if let Some(schema) = schema {
        let mut row = match crate::db::row(store, &schema, b) {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let title = schema
            .as_object()
            .and_then(|s| s.values().find(|d| d["type"] == "title"))
            .and_then(|d| d["name"].as_str())
            .unwrap_or("Name")
            .to_string();
        for k in ["id", "url", title.as_str()] {
            row.remove(k);
        }
        return row;
    }
    // Meeting notes live in a hidden collection; infer columns from their values.
    let mut out = Map::new();
    for (key, raw) in b["properties"].as_object().into_iter().flatten() {
        if key == "title" {
            continue;
        }
        let (mut users, mut pages) = (vec![], vec![]);
        refs(raw, &mut users, &mut pages);
        let has_date = raw.to_string().contains("\"start_date\"");
        if !users.is_empty() {
            out.insert(
                "attendees".into(),
                json!(users.iter().map(|u| store.user_name(u)).collect::<Vec<_>>()),
            );
        } else if has_date {
            out.insert("date".into(), json!(plain(raw, store)));
        } else {
            out.insert(key.clone(), json!(plain(raw, store)));
        }
    }
    out
}

fn props_markdown(props: &Map<String, Value>) -> String {
    let mut s = String::new();
    for (k, v) in props {
        let shown = match v {
            Value::String(x) => x.clone(),
            Value::Array(a) => a
                .iter()
                .map(|x| x.as_str().map(str::to_string).unwrap_or(x.to_string()))
                .collect::<Vec<_>>()
                .join(", "),
            Value::Null => continue,
            other => other.to_string(),
        };
        if !shown.is_empty() {
            s.push_str(&format!("**{k}:** {shown}  \n"));
        }
    }
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

pub fn is_meeting(store: &Store, page_id: &str) -> bool {
    store
        .block(page_id)
        .is_some_and(|b| b["type"] == "meeting_note_page")
}

/// Top-level blocks that make up the readable body. Meeting notes wrap their
/// summary and notes in container blocks, so their children are flattened.
fn body(store: &Store, page_id: &str) -> Vec<String> {
    let Some(b) = store.block(page_id) else {
        return vec![];
    };
    if b["type"] != "meeting_note_page" {
        return children(b);
    }
    ["transcription_summary_id", "transcription_notes_id"]
        .iter()
        .filter_map(|k| fmt_id(b, k))
        .filter_map(|id| store.block(&id).map(children))
        .flatten()
        .collect()
}

pub fn render(store: &Store, page_id: &str, opts: Opts) -> String {
    let mut out = format!("# {}\n\n", store.title(page_id));
    out.push_str(&props_markdown(&properties(store, page_id)));
    let Some(b) = store.block(page_id) else {
        return tidy(&out);
    };
    if b["type"] == "collection_view_page" {
        out.push_str(&db_line(store, b));
    }
    if b["type"] == "meeting_note_page" {
        let m = meeting(store, page_id, opts);
        out.push_str(&format!(
            "## Summary\n\n{}\n",
            m["summary"].as_str().unwrap_or_default()
        ));
        if let Some(n) = m["notes"].as_str().filter(|n| !n.trim().is_empty()) {
            out.push_str(&format!("## Notes\n\n{n}\n"));
        }
        if let Some(lines) = m["transcript"].as_array() {
            out.push_str("## Transcript\n\n");
            for l in lines {
                out.push_str(&format!(
                    "[{}] {}: {}\n",
                    l["at"].as_str().unwrap_or_default(),
                    l["speaker"].as_str().unwrap_or_default(),
                    l["text"].as_str().unwrap_or_default()
                ));
            }
        } else if m["has_transcript"] == true {
            out.push_str("_Transcript available: add `--transcript`._\n");
        }
    } else {
        render_children(store, &children(b), 0, &mut out);
    }
    tidy(&out)
}

fn section_level(t: &str) -> Option<u8> {
    match t {
        "header" => Some(1),
        "sub_header" => Some(2),
        "sub_sub_header" => Some(3),
        _ => None,
    }
}

/// Renders the first heading whose text contains `heading` (case-insensitive)
/// plus everything after it up to the next heading of the same or higher level.
pub fn section(store: &Store, page_id: &str, heading: &str) -> Result<String> {
    let ids = body(store, page_id);
    let wanted = heading.to_lowercase();
    let headings: Vec<(usize, u8, String)> = ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let b = store.block(id)?;
            let lvl = section_level(b["type"].as_str().unwrap_or_default())?;
            Some((i, lvl, plain(&b["properties"]["title"], store)))
        })
        .collect();
    let Some((start, level, _)) = headings
        .iter()
        .find(|(_, _, t)| t.to_lowercase().contains(&wanted))
        .cloned()
    else {
        let available: Vec<&str> = headings.iter().map(|(_, _, t)| t.as_str()).collect();
        return Err(
            AppError::not_found(format!("no heading matching '{heading}'"))
                .with_details(json!({ "headings": available })),
        );
    };
    let end = headings
        .iter()
        .find(|(i, l, _)| *i > start && *l <= level)
        .map_or(ids.len(), |(i, _, _)| *i);
    let mut out = String::new();
    render_children(store, &ids[start..end], 0, &mut out);
    Ok(tidy(&out))
}

/// Blocks created or edited at or after `since_ms`, in document order, each
/// tagged with the heading it sits under.
pub fn changes(store: &Store, page_id: &str, since_ms: i64, opts: Opts) -> Vec<Value> {
    let skip = skipped(store, page_id, opts);
    let mut heading = String::new();
    let mut out = Vec::new();
    walk(store, page_id, true, &skip, &mut |b| {
        let t = b["type"].as_str().unwrap_or_default();
        if b["id"] == page_id || b["alive"] == false {
            return;
        }
        if section_level(t).is_some() {
            heading = plain(&b["properties"]["title"], store);
        }
        let edited = b["last_edited_time"].as_i64().unwrap_or(0);
        if edited < since_ms {
            return;
        }
        let mut md = String::new();
        let mut single = b.clone();
        single["content"] = json!([]);
        render_block(store, &single, 0, 1, &mut md);
        let id = b["id"].as_str().unwrap_or_default();
        out.push(json!({
            "id": id,
            "type": t,
            "change": if b["created_time"].as_i64().unwrap_or(0) >= since_ms { "added" } else { "edited" },
            "edited": ms_to_iso(&b["last_edited_time"]),
            "edited_by": b["last_edited_by_id"].as_str().map(|u| store.user_name(u)),
            "under": (!heading.is_empty()).then(|| heading.clone()),
            "markdown": md.trim().to_string(),
            "url": format!("{}#{}", url_for(&store.base, page_id), id.replace('-', "")),
        }));
    });
    out
}

/// Meeting note parts: summary/notes Markdown, action items and (optionally) transcript.
pub fn meeting(store: &Store, page_id: &str, opts: Opts) -> Value {
    let b = store.block(page_id).cloned().unwrap_or_default();
    let part = |key: &str| {
        let mut out = String::new();
        if let Some(c) = fmt_id(&b, key).and_then(|id| store.block(&id).map(children)) {
            render_children(store, &c, 0, &mut out);
        }
        tidy(&out).trim().to_string()
    };
    let attendees: Vec<String> = properties(store, page_id)
        .get("attendees")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut actions = Vec::new();
    let skip = skipped(store, page_id, Opts { transcript: false });
    walk(store, page_id, true, &skip, &mut |blk| {
        if blk["type"] != "to_do" {
            return;
        }
        let title = &blk["properties"]["title"];
        let (mut users, mut pages) = (vec![], vec![]);
        refs(title, &mut users, &mut pages);
        let due = title
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|seg| seg[1].as_array().cloned().unwrap_or_default())
            .find(|d| d[0] == "d")
            .map(|d| d[1]["start_date"].clone());
        let text = plain(title, store).trim().to_string();
        let mut owners: Vec<String> = users.iter().map(|u| store.user_name(u)).collect();
        // AI summaries name owners in plain text; fall back to attendees named in the item.
        if owners.is_empty() {
            owners = attendees
                .iter()
                .filter(|a| text.contains(a.as_str()))
                .cloned()
                .collect();
        }
        actions.push(json!({
            "text": text,
            "done": plain(&blk["properties"]["checked"], store) == "Yes",
            "owners": owners,
            "due": due,
        }));
    });
    let transcript_id = fmt_id(&b, "transcription_transcript_id");
    let transcript = transcript_id
        .as_ref()
        .filter(|_| opts.transcript)
        .and_then(|id| store.block(id))
        .map(|t| {
            let lines: Vec<&Value> = children(t)
                .iter()
                .filter_map(|id| store.block(id))
                .collect();
            // Offsets come from wall-clock time; per-session `start_ms` resets between recordings.
            let first = lines
                .iter()
                .filter_map(|l| l["format"]["transcript_metadata"]["start_epoch_ms"].as_i64())
                .min()
                .unwrap_or(0);
            lines
                .iter()
                .map(|l| {
                    let meta = &l["format"]["transcript_metadata"];
                    let secs = meta["start_epoch_ms"]
                        .as_i64()
                        .map(|e| (e - first).max(0) as u64 / 1000)
                        .or_else(|| meta["start_ms"].as_u64().map(|m| m / 1000))
                        .unwrap_or(0);
                    json!({
                        "at": format!("{:02}:{:02}", secs / 60, secs % 60),
                        "speaker": meta["speaker_name"].as_str().unwrap_or("speaker"),
                        "text": plain(&l["properties"]["title"], store),
                    })
                })
                .collect::<Vec<_>>()
        });
    json!({
        "summary": part("transcription_summary_id"),
        "notes": part("transcription_notes_id"),
        "action_items": actions,
        "has_transcript": transcript_id.is_some(),
        "transcript": transcript,
    })
}

/// Loads every discussion on the page's blocks with its comments and authors.
pub fn comments(client: &Client, store: &mut Store, page_id: &str) -> Result<Vec<Value>> {
    let skip = skipped(store, page_id, Opts::default());
    let mut ids = Vec::new();
    walk(store, page_id, true, &skip, &mut |b| {
        for d in b["discussions"].as_array().into_iter().flatten() {
            ids.extend(d.as_str().map(str::to_string));
        }
    });
    // A block can be reached twice (e.g. through a synced block); keep first sighting.
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    store.fetch_missing(client, "discussion", &ids)?;
    let discussions: Vec<Value> = ids
        .iter()
        .filter_map(|id| get(&store.map, "discussion", id).cloned())
        .collect();
    let comment_ids: Vec<String> = discussions
        .iter()
        .flat_map(|d| d["comments"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect();
    store.fetch_missing(client, "comment", &comment_ids)?;
    let users: Vec<String> = comment_ids
        .iter()
        .filter_map(|c| get(&store.map, "comment", c))
        .filter_map(|c| c["created_by_id"].as_str().map(str::to_string))
        .collect();
    store.fetch_missing(client, "notion_user", &users)?;
    Ok(discussions
        .iter()
        .map(|d| {
            let block_id = d["parent_id"].as_str().unwrap_or_default();
            let block_text = store
                .block(block_id)
                .map(|b| plain(&b["properties"]["title"], store))
                .unwrap_or_default();
            let comments: Vec<Value> = d["comments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| get(&store.map, "comment", c.as_str()?))
                .filter(|c| c["alive"] != false)
                .map(|c| {
                    json!({
                        "author": c["created_by_id"].as_str().map(|u| store.user_name(u)),
                        "created": ms_to_iso(&c["created_time"]),
                        "text": plain(&c["text"], store).trim(),
                    })
                })
                .collect();
            json!({
                "id": d["id"],
                "resolved": d["resolved"] == true,
                "quote": plain(&d["context"], store),
                "block_id": block_id,
                "block_text": truncate(&block_text, 200),
                "comments": comments,
            })
        })
        .filter(|d| d["comments"].as_array().is_some_and(|c| !c.is_empty()))
        .collect())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max).collect::<String>())
}

fn db_line(store: &Store, b: &Value) -> String {
    let id = b["id"].as_str().unwrap_or_default();
    format!(
        "🗃 Database [{}]({}) — rows: `ntn db query {}`\n\n",
        store.title(id),
        url_for(&store.base, id),
        id
    )
}

fn render_children(store: &Store, ids: &[String], depth: usize, out: &mut String) {
    let mut number = 0;
    for id in ids {
        let Some(b) = store.block(id) else {
            continue;
        };
        if b["alive"] == false {
            continue;
        }
        number = if b["type"] == "numbered_list" {
            number + 1
        } else {
            0
        };
        render_block(store, b, depth, number, out);
    }
}

fn render_block(store: &Store, b: &Value, depth: usize, number: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    let t = b["type"].as_str().unwrap_or_default();
    let props = &b["properties"];
    let text = markdown(&props["title"], store);
    let id = b["id"].as_str().unwrap_or_default();
    let kids = children(b);
    let line = |out: &mut String, s: &str| {
        for (i, l) in s.split('\n').enumerate() {
            out.push_str(&pad);
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(l);
            out.push('\n');
        }
    };
    // Lists and toggles nest children under themselves; everything else flows at the same depth.
    let mut nest = true;
    match t {
        // Empty text blocks used as containers (e.g. meeting summaries) shouldn't indent.
        "text" if text.trim().is_empty() && !kids.is_empty() => nest = false,
        "text" => {
            line(out, &text);
            out.push('\n');
        }
        "header" => {
            out.push('\n');
            line(out, &format!("## {text}\n"))
        }
        "sub_header" => {
            out.push('\n');
            line(out, &format!("### {text}\n"))
        }
        "sub_sub_header" => {
            out.push('\n');
            line(out, &format!("#### {text}\n"))
        }
        "bulleted_list" | "toggle" => line(out, &format!("- {text}")),
        "numbered_list" => line(out, &format!("{number}. {text}")),
        "to_do" => {
            let done = plain(&props["checked"], store) == "Yes";
            line(out, &format!("- [{}] {text}", if done { "x" } else { " " }));
        }
        "quote" => line(out, &format!("> {}\n", text.replace('\n', "\n> "))),
        "callout" => {
            let icon = b["format"]["page_icon"]
                .as_str()
                .filter(|i| !i.starts_with("http"))
                .unwrap_or("💡");
            line(out, &format!("> {icon} {}\n", text.replace('\n', "\n> ")));
        }
        "code" => {
            let lang = plain(&props["language"], store).to_lowercase();
            let code = plain(&props["title"], store);
            out.push_str(&format!("{pad}```{lang}\n{code}\n{pad}```\n\n"));
        }
        "equation" => line(out, &format!("$$\n{}\n$$\n", plain(&props["title"], store))),
        "divider" => {
            out.push('\n');
            line(out, "---\n")
        }
        "page" | "meeting_note_page" => {
            line(
                out,
                &format!("📄 [{}]({})\n", store.title(id), url_for(&store.base, id)),
            );
            return;
        }
        "alias" => {
            if let Some(target) = pointer(b, "alias_pointer") {
                line(
                    out,
                    &format!(
                        "↗ [{}]({})\n",
                        store.title(&target),
                        url_for(&store.base, &target)
                    ),
                );
            }
            return;
        }
        "collection_view" | "collection_view_page" => {
            line(out, &db_line(store, b));
            return;
        }
        "image" | "video" | "audio" | "file" | "pdf" => {
            let src = plain(&props["source"], store);
            let src = if src.is_empty() {
                b["format"]["display_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            } else {
                src
            };
            let caption = plain(&props["caption"], store);
            let label = if caption.is_empty() {
                plain(&props["title"], store)
            } else {
                caption
            };
            let bang = if t == "image" { "!" } else { "" };
            line(out, &format!("{bang}[{t}: {label}]({src})\n"));
        }
        "bookmark"
        | "embed"
        | "figma"
        | "tweet"
        | "gist"
        | "codepen"
        | "maps"
        | "drive"
        | "external_object_instance" => {
            let link = plain(&props["link"], store);
            let link = if link.is_empty() {
                plain(&props["source"], store)
            } else {
                link
            };
            let title = if text.is_empty() {
                t.to_string()
            } else {
                plain(&props["title"], store)
            };
            line(out, &format!("🔗 [{title}]({link})\n"));
        }
        "table" => {
            render_table(store, b, &kids, &pad, out);
            return;
        }
        "transclusion_reference" => {
            if let Some(src) = pointer(b, "transclusion_reference_pointer")
                && let Some(sb) = store.block(&src)
            {
                render_children(store, &children(sb), depth, out);
            }
            return;
        }
        "column_list" | "column" | "transclusion_container" => nest = false,
        "table_of_contents" | "breadcrumb" => return,
        _ if !text.is_empty() => {
            line(out, &text);
            out.push('\n');
        }
        _ => line(out, &format!("[{t} block]\n")),
    }
    let child_depth = if nest
        && matches!(
            t,
            "bulleted_list" | "numbered_list" | "to_do" | "toggle" | "text" | "quote" | "callout"
        ) {
        depth + 1
    } else {
        depth
    };
    render_children(store, &kids, child_depth, out);
}

fn render_table(store: &Store, b: &Value, rows: &[String], pad: &str, out: &mut String) {
    let cols: Vec<String> = b["format"]["table_block_column_order"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect();
    let mut first = true;
    for rid in rows {
        let Some(r) = store.block(rid) else { continue };
        let cells: Vec<String> = cols
            .iter()
            .map(|c| {
                markdown(&r["properties"][c], store)
                    .replace('|', "\\|")
                    .replace('\n', " ")
            })
            .collect();
        out.push_str(&format!("{pad}| {} |\n", cells.join(" | ")));
        if first {
            out.push_str(&format!("{pad}|{}\n", " --- |".repeat(cols.len())));
            first = false;
        }
    }
    out.push('\n');
}

/// Collapses runs of blank lines.
fn tidy(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for l in s.lines() {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(l.trim_end());
        out.push('\n');
    }
    out.trim_start_matches('\n').trim_end().to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(id: &str, t: &str, title: &str, content: &[&str]) -> (String, Value) {
        (
            id.to_string(),
            json!({ "value": { "id": id, "type": t, "alive": true,
                "properties": { "title": [[title]] }, "content": content } }),
        )
    }

    #[test]
    fn renders_tree() {
        let mut blocks = serde_json::Map::new();
        for (k, v) in [
            blk(
                "root",
                "page",
                "Plan",
                &["h", "b1", "n1", "n2", "t", "child", "code"],
            ),
            blk("h", "header", "Goals", &[]),
            blk("b1", "bulleted_list", "first", &["b2"]),
            blk("b2", "bulleted_list", "nested", &[]),
            blk("n1", "numbered_list", "one", &[]),
            blk("n2", "numbered_list", "two", &[]),
            blk("child", "page", "Sub page", &["hidden"]),
            blk("hidden", "text", "should not render", &[]),
        ] {
            blocks.insert(k, v);
        }
        blocks.insert("t".into(), json!({"value": {"id": "t", "type": "to_do", "properties": {"title": [["ship"]], "checked": [["Yes"]]}}}));
        blocks.insert("code".into(), json!({"value": {"id": "code", "type": "code", "properties": {"title": [["fn x()"]], "language": [["Rust"]]}}}));
        let mut store = Store::new("https://n.so");
        store.merge(&json!({ "block": blocks }));
        let md = render(&store, "root", Opts::default());
        assert_eq!(
            md,
            "# Plan\n\n## Goals\n\n- first\n  - nested\n1. one\n2. two\n- [x] ship\n📄 [Sub page](https://n.so/child)\n\n```rust\nfn x()\n```\n"
        );
    }

    fn store_of(blocks: Value, extra: Value) -> Store {
        let mut store = Store::new("https://n.so");
        store.merge(&json!({ "block": blocks }));
        store.merge(&extra);
        store
    }

    fn b(id: &str, t: &str, title: &str, content: &[&str], edited: i64) -> Value {
        json!({ "value": { "id": id, "type": t, "alive": true, "last_edited_time": edited,
            "created_time": 0, "properties": { "title": [[title]] }, "content": content } })
    }

    #[test]
    fn section_stops_at_same_level_heading() {
        let store = store_of(
            json!({
                "root": b("root", "page", "Doc", &["h1", "p1", "h2", "p2", "h3", "p3"], 0),
                "h1": b("h1", "header", "Intro", &[], 0),
                "p1": b("p1", "text", "hello", &[], 0),
                "h2": b("h2", "sub_header", "Details", &[], 0),
                "p2": b("p2", "text", "deep", &[], 0),
                "h3": b("h3", "header", "Next", &[], 0),
                "p3": b("p3", "text", "later", &[], 0),
            }),
            json!({}),
        );
        assert_eq!(
            section(&store, "root", "intro").unwrap(),
            "## Intro\n\nhello\n\n### Details\n\ndeep\n"
        );
        assert_eq!(
            section(&store, "root", "detail").unwrap(),
            "### Details\n\ndeep\n"
        );
        let err = section(&store, "root", "missing").unwrap_err();
        assert_eq!(err.details["headings"], json!(["Intro", "Details", "Next"]));
    }

    #[test]
    fn changes_keep_heading_context() {
        let store = store_of(
            json!({
                "root": b("root", "page", "Doc", &["h1", "old", "new"], 0),
                "h1": b("h1", "header", "Plan", &[], 10),
                "old": b("old", "text", "stale", &[], 10),
                "new": b("new", "bulleted_list", "fresh", &[], 500),
            }),
            json!({}),
        );
        let c = changes(&store, "root", 100, Opts::default());
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0]["markdown"].as_str(), c[0]["under"].as_str()),
            (Some("- fresh"), Some("Plan"))
        );
        assert_eq!(c[0]["change"], "edited");
    }

    #[test]
    fn meeting_parts_and_owners() {
        let mut root = b("m", "meeting_note_page", "Sync", &["sum", "tr"], 0);
        root["value"]["format"] =
            json!({ "transcription_summary_id": "sum", "transcription_transcript_id": "tr" });
        root["value"]["properties"]["att"] = json!([["‣", [["u", "u1"]]]]);
        let mut todo = b("t1", "to_do", "Ana Lima to send deck", &[], 0);
        todo["value"]["properties"]["checked"] = json!([["Yes"]]);
        let mut l1 = b("l1", "text", "hi", &[], 0);
        l1["value"]["format"] = json!({ "transcript_metadata": { "speaker_name": "speaker0", "start_epoch_ms": 1000 } });
        let mut l2 = b("l2", "text", "bye", &[], 0);
        l2["value"]["format"] = json!({ "transcript_metadata": { "speaker_name": "speaker1", "start_epoch_ms": 66000 } });
        let store = store_of(
            json!({ "m": root, "sum": b("sum", "text", "", &["t1"], 0), "t1": todo,
                    "tr": b("tr", "text", "", &["l1", "l2"], 0), "l1": l1, "l2": l2 }),
            json!({ "notion_user": { "u1": { "value": { "id": "u1", "name": "Ana Lima" } } } }),
        );
        let m = meeting(&store, "m", Opts { transcript: true });
        assert_eq!(
            m["action_items"],
            json!([{ "text": "Ana Lima to send deck", "done": true, "owners": ["Ana Lima"], "due": null }])
        );
        assert_eq!(
            m["transcript"][1],
            json!({ "at": "01:05", "speaker": "speaker1", "text": "bye" })
        );
        assert_eq!(m["summary"], "- [x] Ana Lima to send deck");
        let without = meeting(&store, "m", Opts::default());
        assert!(without["transcript"].is_null() && without["has_transcript"] == true);
    }
}
