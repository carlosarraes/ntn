//! Load a page's block tree and render it as Markdown.

use crate::client::Client;
use crate::error::{AppError, Result};
use crate::ids::url_for;
use crate::records::{Store, collection_id, markdown, plain, refs};
use serde_json::{Value, json};

const MAX_CHUNKS: u32 = 50;
const MAX_FETCH_ROUNDS: usize = 20;

fn is_page_like(t: &str) -> bool {
    matches!(t, "page" | "collection_view_page")
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

/// Pulls the page via loadPageChunk, then backfills any descendants,
/// synced-block sources, mentioned users/pages and child databases.
pub fn load(client: &Client, store: &mut Store, page_id: &str) -> Result<()> {
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
    if store.block(page_id).is_none() {
        return Err(AppError::not_found(format!(
            "page {page_id} not found or not shared with you"
        )));
    }
    for _ in 0..MAX_FETCH_ROUNDS {
        let (mut blocks, mut users, mut collections) = (vec![], vec![], vec![]);
        walk(store, page_id, true, &mut |b| {
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
        });
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

/// Visits loaded blocks under `id` without descending into child pages.
fn walk(store: &Store, id: &str, root: bool, f: &mut dyn FnMut(&Value)) {
    let Some(b) = store.block(id) else { return };
    f(b);
    let t = b["type"].as_str().unwrap_or_default();
    if is_page_like(t) && !root {
        return;
    }
    let mut ids = children(b);
    ids.extend(pointer(b, "transclusion_reference_pointer"));
    for c in ids {
        walk(store, &c, false, f);
    }
}

pub fn render(store: &Store, page_id: &str) -> String {
    let mut out = format!("# {}\n\n", store.title(page_id));
    if let Some(b) = store.block(page_id) {
        if b["type"] == "collection_view_page" {
            out.push_str(&db_line(store, b));
        }
        render_children(store, &children(b), 0, &mut out);
    }
    tidy(&out)
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
        "text" => {
            line(out, &text);
            out.push('\n');
        }
        "header" => line(out, &format!("## {text}\n")),
        "sub_header" => line(out, &format!("### {text}\n")),
        "sub_sub_header" => line(out, &format!("#### {text}\n")),
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
        "divider" => line(out, "---\n"),
        "page" => {
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
    out.trim_end().to_string() + "\n"
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
        let md = render(&store, "root");
        assert_eq!(
            md,
            "# Plan\n\n## Goals\n\n- first\n  - nested\n1. one\n2. two\n- [x] ship\n📄 [Sub page](https://n.so/child)\n\n```rust\nfn x()\n```\n"
        );
    }
}
