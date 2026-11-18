use crate::cli::{Ctx, read_limit};
use crate::client::{Client, record_value};
use crate::db;
use crate::error::{AppError, Result};
use crate::ids::{self, url_for};
use crate::page::{self, Opts};
use crate::records::{Store, ms_to_iso};
use crate::time::{ms_to_date, parse_since};
use clap::{Args, Subcommand, ValueEnum};
use regex::Regex;
use serde_json::{Value, json};

const DAY_MS: i64 = 86_400_000;
const MAX_MEETING_DAYS: i64 = 180;
/// Most hits one Notion search call returns; paging walks within this window.
const SEARCH_WINDOW: usize = 100;

#[derive(Clone, Copy, ValueEnum)]
pub enum Sort {
    Relevance,
    LastEdited,
}

#[derive(Args)]
pub struct SearchArgs {
    query: String,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Skip this many hits; pass the `next` value of the previous page
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, value_enum, default_value = "relevance")]
    sort: Sort,
    /// Only results under this page (URL or id)
    #[arg(long = "in")]
    within: Option<String>,
    /// Only pages edited since (7d, 2h, 2026-11-01)
    #[arg(long)]
    since: Option<String>,
    /// Only pages edited by this person (me, @Name, email or user id)
    #[arg(long)]
    edited_by: Option<String>,
}

#[derive(Subcommand)]
pub enum PageCmd {
    /// Render one or more pages (URL or id) as Markdown
    Read {
        #[arg(required = true)]
        pages: Vec<String>,
        /// Only this heading and its content (case-insensitive substring)
        #[arg(long)]
        section: Option<String>,
        /// Include meeting transcripts
        #[arg(long)]
        transcript: bool,
    },
    /// List comment threads with quoted text, author and time
    Comments {
        page: String,
        /// Include resolved threads
        #[arg(long)]
        all: bool,
    },
    /// Blocks added or edited since a time (2d, 6h, 2026-11-01)
    Changes {
        page: String,
        #[arg(long, default_value = "1d")]
        since: String,
    },
}

#[derive(Subcommand)]
pub enum DbCmd {
    /// List rows of a database (URL with optional ?v=<view>, or id)
    Query {
        database: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Full-text filter applied by Notion
        #[arg(long)]
        search: Option<String>,
        /// Column filter, repeatable: 'Status=Done', 'Owner=@Ana', 'Points>3', 'Name~api', 'Tag!=x'
        #[arg(long = "where")]
        filters: Vec<String>,
        /// Sort by a column: 'Date desc' or 'Name'
        #[arg(long)]
        sort: Option<String>,
        /// Rows fetched before local --where/--sort are applied
        #[arg(long, default_value_t = 1000)]
        scan: usize,
    },
}

#[derive(Subcommand)]
pub enum MeetingsCmd {
    /// Meeting notes edited in a time window, newest first
    List {
        #[arg(long, default_value = "7d")]
        since: String,
        /// Only meetings whose content matches this text
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Summary, action items, notes and (with --transcript) the transcript
    Read {
        meeting: String,
        #[arg(long)]
        transcript: bool,
    },
}

fn space_id(client: &Client) -> Result<String> {
    if let Some(s) = client.space_id() {
        return Ok(s.to_string());
    }
    first_spaces(client)?
        .first()
        .and_then(|s| s["id"].as_str().map(str::to_string))
        .ok_or_else(|| AppError::not_found("no workspace found for this account"))
}

fn first_spaces(client: &Client) -> Result<Vec<Value>> {
    let r = client.call("getSpaces", json!({}))?;
    let mut out = Vec::new();
    for user in r.as_object().into_iter().flat_map(|o| o.values()) {
        for s in user["space"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
        {
            if let Some(v) = record_value(s) {
                out.push(json!({ "id": v["id"], "name": v["name"], "domain": v["domain"], "plan_type": v["plan_type"] }));
            }
        }
    }
    Ok(out)
}

struct SearchQuery<'a> {
    query: &'a str,
    limit: usize,
    sort: Value,
    ancestors: Vec<String>,
    edited_by: Vec<String>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
}

fn run_search(client: &Client, space: &str, q: &SearchQuery) -> Result<Value> {
    let mut edited = json!({});
    if let Some(s) = q.since_ms {
        edited["starting"] = json!({ "type": "date", "start_date": ms_to_date(s) });
    }
    if let Some(u) = q.until_ms {
        edited["ending"] = json!({ "type": "date", "start_date": ms_to_date(u) });
    }
    client.call(
        "search",
        json!({
            "type": "BlocksInSpace",
            "query": q.query,
            "spaceId": space,
            "limit": q.limit,
            "filters": {
                "isDeletedOnly": false, "excludeTemplates": true,
                "navigableBlockContentOnly": true, "requireEditPermissions": false,
                "includePublicPagesWithoutExplicitAccess": false,
                "ancestors": q.ancestors, "createdBy": [], "editedBy": q.edited_by,
                "lastEditedTime": edited, "createdTime": {}, "inTeams": [],
            },
            "sort": q.sort,
            "source": "quick_find",
        }),
    )
}

/// "me", a user id, or a name/email of someone who recently edited the workspace.
fn resolve_user(client: &Client, space: &str, input: &str) -> Result<String> {
    let input = input.trim_start_matches('@');
    if input.eq_ignore_ascii_case("me") {
        return Ok(client.user_id().to_string());
    }
    if let Ok(t) = ids::parse(input) {
        return Ok(t.id);
    }
    let recent = run_search(
        client,
        space,
        &SearchQuery {
            query: "",
            limit: 100,
            sort: json!({ "field": "lastEdited", "direction": "desc" }),
            ancestors: vec![],
            edited_by: vec![],
            since_ms: None,
            until_ms: None,
        },
    )?;
    let mut store = Store::new(client.base_url());
    store.merge(&recent["recordMap"]);
    let mut ids: Vec<String> = store.map["block"]
        .as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .filter_map(record_value)
        .flat_map(|b| [b["created_by_id"].as_str(), b["last_edited_by_id"].as_str()])
        .flatten()
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    store.fetch_missing(client, "notion_user", &ids)?;
    let wanted = input.to_lowercase();
    let hits: Vec<&String> = ids
        .iter()
        .filter(|id| {
            let u = crate::client::get(&store.map, "notion_user", id);
            let name = store.user_name(id).to_lowercase();
            let email = u
                .and_then(|u| u["email"].as_str())
                .unwrap_or_default()
                .to_lowercase();
            name == wanted || email == wanted || name.split(' ').next() == Some(wanted.as_str())
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(AppError::not_found(format!(
            "no recent editor named '{input}'; pass their user id"
        ))),
        many => Err(AppError::ambiguous(
            format!("'{input}' matches several people"),
            many.iter()
                .map(|id| json!({ "id": id, "name": store.user_name(id) }))
                .collect::<Vec<_>>()
                .into(),
        )),
    }
}

/// Highlight text of a search hit, markers stripped. Notion has sent it as
/// `highlight.text` and as `highlights.textHighlights` (strings or `{text}`);
/// both are read, and a hit without any text yields `None`.
fn snippet(hit: &Value, marker: &Regex) -> Option<String> {
    let parts: Vec<&str> = hit["highlights"]["textHighlights"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h.as_str().or(h["text"].as_str()))
        .collect();
    let raw = if parts.is_empty() {
        hit["highlight"]["text"].as_str()?.to_string()
    } else {
        parts.join(" … ")
    };
    Some(marker.replace_all(&raw, "").trim().to_string()).filter(|s| !s.is_empty())
}

pub fn search(ctx: &Ctx, args: SearchArgs) -> Result<()> {
    let limit = read_limit(args.limit, SEARCH_WINDOW)?;
    let offset = args.offset;
    if offset >= SEARCH_WINDOW {
        return Err(AppError::input(format!(
            "--offset must be below {SEARCH_WINDOW}: Notion search returns at most {SEARCH_WINDOW} hits; narrow with --since, --in or a better query"
        )));
    }
    let client = ctx.client()?;
    let space = space_id(&client)?;
    let ancestors: Vec<String> = args
        .within
        .map(|w| ids::parse(&w).map(|t| t.id))
        .transpose()?
        .into_iter()
        .collect();
    let since_ms = args.since.as_deref().map(parse_since).transpose()?;
    let edited_by = args
        .edited_by
        .as_deref()
        .map(|u| resolve_user(&client, &space, u))
        .transpose()?
        .into_iter()
        .collect();
    let sort = match args.sort {
        Sort::Relevance => json!({ "field": "relevance" }),
        Sort::LastEdited => json!({ "field": "lastEdited", "direction": "desc" }),
    };
    let r = run_search(
        &client,
        &space,
        &SearchQuery {
            query: &args.query,
            // Notion has no offset, so fetch through this page plus one hit to
            // learn whether another page exists. The date filter trims hits
            // after the fact, so it needs the whole window.
            limit: if since_ms.is_some() {
                SEARCH_WINDOW
            } else {
                (offset + limit + 1).min(SEARCH_WINDOW)
            },
            sort,
            ancestors,
            edited_by,
            since_ms,
            until_ms: None,
        },
    )?;
    let mut store = Store::new(client.base_url());
    store.merge(&r["recordMap"]);
    let marker = Regex::new(r"</?gzkNfoUU>").unwrap();
    let results: Vec<Value> = r["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            let id = hit["id"].as_str()?;
            let b = store.block(id);
            // The date filter is day-granular; trim to the exact instant.
            if let (Some(s), Some(b)) = (since_ms, b)
                && b["last_edited_time"].as_i64().unwrap_or(i64::MAX) < s
            {
                return None;
            }
            let mut item = json!({
                "id": id,
                "title": store.title(id),
                "type": b.map(|b| b["type"].clone()),
                "path": Some(store.path(id)).filter(|p| !p.is_empty()),
                "last_edited": b.and_then(|b| ms_to_iso(&b["last_edited_time"])),
                "url": url_for(&store.base, id),
            });
            if let Some(text) = snippet(hit, &marker) {
                item["snippet"] = json!(text);
            }
            Some(item)
        })
        .collect();
    let total = if since_ms.is_some() {
        json!(results.len())
    } else {
        r["total"].clone()
    };
    let next = (results.len() > offset + limit).then_some(offset + limit);
    let results: Vec<Value> = results.into_iter().skip(offset).take(limit).collect();
    ctx.out.emit(
        json!({ "total": total, "next": next, "results": results }),
        |v| {
            let mut s = format!("{} results\n\n", v["total"]);
            for hit in v["results"].as_array().into_iter().flatten() {
                let icon = match hit["type"].as_str() {
                    Some("collection_view_page" | "collection_view") => "🗃",
                    Some("meeting_note_page") => "📅",
                    _ => "📄",
                };
                s.push_str(&format!(
                    "{icon} {}\n",
                    hit["title"].as_str().unwrap_or_default()
                ));
                if let Some(p) = hit["path"].as_str() {
                    s.push_str(&format!("   in {p}\n"));
                }
                if let Some(t) = hit["snippet"].as_str() {
                    s.push_str(&format!("   {}\n", t.replace('\n', " ")));
                }
                s.push_str(&format!(
                    "   {}  (edited {})\n\n",
                    hit["url"].as_str().unwrap_or_default(),
                    hit["last_edited"].as_str().unwrap_or("?")
                ));
            }
            if let Some(n) = v["next"].as_u64() {
                s.push_str(&format!("More results: --offset {n}\n"));
            }
            s
        },
    );
    Ok(())
}

fn load_page(client: &Client, input: &str, opts: Opts) -> Result<(Store, String)> {
    let target = ids::parse(input)?;
    let id = target.peek_id.unwrap_or(target.id);
    let mut store = Store::new(client.base_url());
    page::load(client, &mut store, &id, opts)?;
    Ok((store, id))
}

fn page_json(store: &Store, id: &str, markdown: String) -> Value {
    let b = store.block(id).cloned().unwrap_or_default();
    json!({
        "id": id,
        "title": store.title(id),
        "url": url_for(&store.base, id),
        "type": b["type"],
        "last_edited": ms_to_iso(&b["last_edited_time"]),
        "properties": page::properties(store, id),
        "markdown": markdown,
    })
}

pub fn page(ctx: &Ctx, cmd: PageCmd) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        PageCmd::Read {
            pages,
            section,
            transcript,
        } => {
            let opts = Opts { transcript };
            let mut out = Vec::new();
            let mut missing = Vec::new();
            for input in &pages {
                let (store, id) = match load_page(&client, input, opts) {
                    Ok(loaded) => loaded,
                    Err(e) if e.code == "not_found" => {
                        let t = ids::parse(input)?;
                        missing.push(t.peek_id.unwrap_or(t.id));
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let md = match &section {
                    Some(h) => page::section(&store, &id, h)?,
                    None => page::render(&store, &id, opts),
                };
                out.push(page_json(&store, &id, md));
            }
            if !missing.is_empty() {
                return Err(AppError::not_found(format!(
                    "not found or not shared with you: {}",
                    missing.join(", ")
                ))
                .with_details(json!({ "missing": missing })));
            }
            let value = if out.len() == 1 {
                out.remove(0)
            } else {
                Value::Array(out)
            };
            ctx.out.emit(value, |v| match v {
                Value::Array(items) => items
                    .iter()
                    .map(|p| p["markdown"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n"),
                p => p["markdown"].as_str().unwrap_or_default().to_string(),
            });
        }
        PageCmd::Comments { page: input, all } => {
            let (mut store, id) = load_page(&client, &input, Opts::default())?;
            let mut threads = page::comments(&client, &mut store, &id)?;
            if !all {
                threads.retain(|t| t["resolved"] == false);
            }
            let v = json!({ "page": store.title(&id), "url": url_for(&store.base, &id), "threads": threads });
            ctx.out.emit(v, |v| {
                let list = v["threads"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return "No comment threads.\n".into();
                }
                let mut s = String::new();
                for t in &list {
                    let quote = t["quote"]
                        .as_str()
                        .filter(|q| !q.is_empty())
                        .or(t["block_text"].as_str())
                        .unwrap_or_default();
                    s.push_str(&format!(
                        "{} on \"{}\"\n",
                        if t["resolved"] == true {
                            "✓ resolved"
                        } else {
                            "● open"
                        },
                        quote
                    ));
                    for c in t["comments"].as_array().into_iter().flatten() {
                        s.push_str(&format!(
                            "    {} ({}): {}\n",
                            c["author"].as_str().unwrap_or("?"),
                            c["created"].as_str().unwrap_or_default(),
                            c["text"]
                                .as_str()
                                .unwrap_or_default()
                                .replace('\n', "\n      ")
                        ));
                    }
                    s.push('\n');
                }
                s
            });
        }
        PageCmd::Changes { page: input, since } => {
            let since_ms = parse_since(&since)?;
            let (store, id) = load_page(&client, &input, Opts::default())?;
            let changes = page::changes(&store, &id, since_ms, Opts::default());
            let v = json!({
                "page": store.title(&id), "url": url_for(&store.base, &id),
                "since": ms_to_iso(&json!(since_ms)), "changes": changes,
            });
            ctx.out.emit(v, |v| {
                let list = v["changes"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return format!(
                        "No changes since {}.\n",
                        v["since"].as_str().unwrap_or_default()
                    );
                }
                let mut s = String::new();
                for c in &list {
                    s.push_str(&format!(
                        "[{} {} by {}]{}\n{}\n\n",
                        c["change"].as_str().unwrap_or_default(),
                        c["edited"].as_str().unwrap_or_default(),
                        c["edited_by"].as_str().unwrap_or("?"),
                        c["under"]
                            .as_str()
                            .map(|h| format!(" under \"{h}\""))
                            .unwrap_or_default(),
                        c["markdown"].as_str().unwrap_or_default()
                    ));
                }
                s
            });
        }
    }
    Ok(())
}

pub fn db(ctx: &Ctx, cmd: DbCmd) -> Result<()> {
    let DbCmd::Query {
        database,
        limit,
        search,
        filters,
        sort,
        scan,
    } = cmd;
    let limit = read_limit(limit, 10_000)?;
    let filters = filters
        .iter()
        .map(|f| db::Filter::parse(f))
        .collect::<Result<Vec<_>>>()?;
    let sort = sort.as_deref().map(db::SortSpec::parse).transpose()?;
    let fetch = if filters.is_empty() && sort.is_none() {
        limit
    } else {
        read_limit(scan, 10_000)?
    };
    let client = ctx.client()?;
    let target = ids::parse(&database)?;
    let mut store = Store::new(client.base_url());
    let mut rows = db::query(
        &client,
        &mut store,
        &target,
        db::Query {
            limit: fetch,
            search: search.as_deref(),
        },
    )?;
    rows.rows.retain(|r| filters.iter().all(|f| f.matches(r)));
    if let Some(s) = &sort {
        s.apply(&mut rows.rows);
    }
    let has_more = rows.has_more || rows.rows.len() > limit;
    rows.rows.truncate(limit);
    let v = json!({ "database": rows.database, "rows": rows.rows, "has_more": has_more });
    ctx.out.emit(v, |v| {
        let mut s = format!(
            "🗃 {}\n\n",
            v["database"]["title"].as_str().unwrap_or_default()
        );
        let title_col = v["database"]["columns"]
            .as_array()
            .and_then(|c| c.iter().find(|c| c["type"] == "title"))
            .and_then(|c| c["name"].as_str())
            .unwrap_or("Name")
            .to_string();
        for row in v["rows"].as_array().into_iter().flatten() {
            s.push_str(&format!(
                "• {}\n",
                row[&title_col].as_str().unwrap_or("Untitled")
            ));
            for (k, val) in row.as_object().into_iter().flatten() {
                if k == "id" || *k == title_col || val.is_null() || val == "" || val == &json!([]) {
                    continue;
                }
                s.push_str(&format!("    {k}: {}\n", db::display(val)));
            }
        }
        if v["has_more"] == true {
            s.push_str("\n(more rows: raise --limit or --scan)\n");
        }
        s
    });
    Ok(())
}

pub fn meetings(ctx: &Ctx, cmd: MeetingsCmd) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        MeetingsCmd::List {
            since,
            query,
            limit,
        } => {
            let limit = read_limit(limit, 1000)?;
            let since_ms = parse_since(&since)?;
            let now = chrono::Utc::now().timestamp_millis();
            if now - since_ms > MAX_MEETING_DAYS * DAY_MS {
                return Err(AppError::input(format!(
                    "--since is capped at {MAX_MEETING_DAYS}d"
                )));
            }
            let space = space_id(&client)?;
            let mut store = Store::new(client.base_url());
            let mut ids: Vec<String> = Vec::new();
            // Search returns at most 100 hits, so walk the window one day at a time.
            let mut day = now;
            while day >= since_ms - DAY_MS && ids.len() < limit {
                let r = run_search(
                    &client,
                    &space,
                    &SearchQuery {
                        query: query.as_deref().unwrap_or_default(),
                        limit: 100,
                        sort: json!({ "field": "lastEdited", "direction": "desc" }),
                        ancestors: vec![],
                        edited_by: vec![],
                        since_ms: Some(day),
                        until_ms: Some(day),
                    },
                )?;
                store.merge(&r["recordMap"]);
                for hit in r["results"].as_array().into_iter().flatten() {
                    let Some(id) = hit["id"].as_str() else {
                        continue;
                    };
                    let is_meeting = store.block(id).is_some_and(|b| {
                        b["type"] == "meeting_note_page"
                            && b["created_time"].as_i64().unwrap_or(0) >= since_ms
                    });
                    if is_meeting && !ids.iter().any(|x| x == id) {
                        ids.push(id.to_string());
                    }
                }
                day -= DAY_MS;
            }
            let mut users = Vec::new();
            for id in &ids {
                if let Some(b) = store.block(id) {
                    for v in b["properties"]
                        .as_object()
                        .into_iter()
                        .flat_map(|o| o.values())
                    {
                        crate::records::refs(v, &mut users, &mut Vec::new());
                    }
                }
            }
            store.fetch_missing(&client, "notion_user", &users)?;
            ids.sort_by_key(|id| {
                std::cmp::Reverse(
                    store
                        .block(id)
                        .and_then(|b| b["created_time"].as_i64())
                        .unwrap_or(0),
                )
            });
            ids.truncate(limit);
            let meetings: Vec<Value> = ids
                .iter()
                .map(|id| {
                    let props = page::properties(&store, id);
                    json!({
                        "id": id,
                        "title": store.title(id),
                        "date": props.get("date").cloned().unwrap_or_else(|| json!(store.block(id).and_then(|b| ms_to_iso(&b["created_time"])))),
                        "attendees": props.get("attendees").cloned().unwrap_or(json!([])),
                        "url": url_for(&store.base, id),
                    })
                })
                .collect();
            ctx.out.emit(json!({ "meetings": meetings }), |v| {
                let list = v["meetings"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return "No meeting notes in that window.\n".into();
                }
                list.iter()
                    .map(|m| {
                        format!(
                            "📅 {}\n   {}  ·  {}\n   {}\n\n",
                            m["title"].as_str().unwrap_or_default(),
                            m["date"].as_str().unwrap_or_default(),
                            db::display(&m["attendees"]),
                            m["url"].as_str().unwrap_or_default()
                        )
                    })
                    .collect()
            });
        }
        MeetingsCmd::Read {
            meeting,
            transcript,
        } => {
            let opts = Opts { transcript };
            let (store, id) = load_page(&client, &meeting, opts)?;
            if !page::is_meeting(&store, &id) {
                return Err(AppError::input("not a meeting note; use `ntn page read`"));
            }
            let mut v = page::meeting(&store, &id, opts);
            v["id"] = json!(id);
            v["title"] = json!(store.title(&id));
            v["url"] = json!(url_for(&store.base, &id));
            v["properties"] = json!(page::properties(&store, &id));
            if !transcript {
                v.as_object_mut().map(|m| m.remove("transcript"));
            }
            let md = page::render(&store, &id, opts);
            ctx.out.emit(v, |_| md.clone());
        }
    }
    Ok(())
}

pub fn spaces(ctx: &Ctx) -> Result<()> {
    let client = ctx.client()?;
    let spaces = first_spaces(&client)?;
    let current = client.space_id().map(str::to_string);
    ctx.out
        .emit(json!({ "spaces": spaces, "current": current }), |v| {
            v["spaces"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    format!(
                        "{} {} {}\n",
                        if v["current"] == s["id"] { "*" } else { " " },
                        s["id"].as_str().unwrap_or_default(),
                        s["name"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        });
    Ok(())
}
