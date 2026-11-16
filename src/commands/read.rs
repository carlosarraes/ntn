use crate::cli::{Ctx, read_limit};
use crate::client::{Client, record_value};
use crate::db;
use crate::error::{AppError, Result};
use crate::ids::{self, url_for};
use crate::page;
use crate::records::{Store, ms_to_iso};
use clap::{Args, Subcommand, ValueEnum};
use regex::Regex;
use serde_json::{Value, json};

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
    #[arg(long, value_enum, default_value = "relevance")]
    sort: Sort,
    /// Only results under this page (URL or id)
    #[arg(long = "in")]
    within: Option<String>,
}

#[derive(Subcommand)]
pub enum PageCmd {
    /// Render a page (URL or id) as Markdown
    Read { page: String },
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

pub fn search(ctx: &Ctx, args: SearchArgs) -> Result<()> {
    let limit = read_limit(args.limit, 100)?;
    let client = ctx.client()?;
    let ancestors: Vec<String> = args
        .within
        .map(|w| ids::parse(&w).map(|t| t.id))
        .transpose()?
        .into_iter()
        .collect();
    let sort = match args.sort {
        Sort::Relevance => json!({ "field": "relevance" }),
        Sort::LastEdited => json!({ "field": "lastEdited", "direction": "desc" }),
    };
    let r = client.call(
        "search",
        json!({
            "type": "BlocksInSpace",
            "query": args.query,
            "spaceId": space_id(&client)?,
            "limit": limit,
            "filters": {
                "isDeletedOnly": false, "excludeTemplates": true,
                "navigableBlockContentOnly": true, "requireEditPermissions": false,
                "includePublicPagesWithoutExplicitAccess": false,
                "ancestors": ancestors, "createdBy": [], "editedBy": [],
                "lastEditedTime": {}, "createdTime": {}, "inTeams": [],
            },
            "sort": sort,
            "source": "quick_find",
        }),
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
            let strip = |v: &Value| v.as_str().map(|s| marker.replace_all(s, "").to_string());
            Some(json!({
                "id": id,
                "title": store.title(id),
                "type": b.map(|b| b["type"].clone()),
                "path": strip(&hit["highlight"]["pathText"]),
                "snippet": strip(&hit["highlight"]["text"]),
                "last_edited": b.and_then(|b| ms_to_iso(&b["last_edited_time"])),
                "url": url_for(&store.base, id),
            }))
        })
        .collect();
    ctx.out
        .emit(json!({ "total": r["total"], "results": results }), |v| {
            let mut s = format!("{} results\n\n", v["total"]);
            for hit in v["results"].as_array().into_iter().flatten() {
                let icon = if hit["type"] == "collection_view_page" {
                    "🗃"
                } else {
                    "📄"
                };
                s.push_str(&format!(
                    "{icon} {}\n",
                    hit["title"].as_str().unwrap_or_default()
                ));
                if let Some(p) = hit["path"].as_str().filter(|p| !p.is_empty()) {
                    s.push_str(&format!("   in {p}\n"));
                }
                if let Some(t) = hit["snippet"].as_str().filter(|t| !t.is_empty()) {
                    s.push_str(&format!("   {}\n", t.replace('\n', " ")));
                }
                s.push_str(&format!(
                    "   {}\n\n",
                    hit["url"].as_str().unwrap_or_default()
                ));
            }
            s
        });
    Ok(())
}

pub fn page(ctx: &Ctx, cmd: PageCmd) -> Result<()> {
    let PageCmd::Read { page: input } = cmd;
    let client = ctx.client()?;
    let target = ids::parse(&input)?;
    let id = target.peek_id.unwrap_or(target.id);
    let mut store = Store::new(client.base_url());
    page::load(&client, &mut store, &id)?;
    let b = store.block(&id).cloned().unwrap_or_default();
    let v = json!({
        "id": id,
        "title": store.title(&id),
        "url": url_for(&store.base, &id),
        "last_edited": ms_to_iso(&b["last_edited_time"]),
        "markdown": page::render(&store, &id),
    });
    ctx.out.emit(v, |v| {
        v["markdown"].as_str().unwrap_or_default().to_string()
    });
    Ok(())
}

pub fn db(ctx: &Ctx, cmd: DbCmd) -> Result<()> {
    let DbCmd::Query {
        database,
        limit,
        search,
    } = cmd;
    let limit = read_limit(limit, 1000)?;
    let client = ctx.client()?;
    let target = ids::parse(&database)?;
    let mut store = Store::new(client.base_url());
    let rows = db::query(
        &client,
        &mut store,
        &target,
        db::Query {
            limit,
            search: search.as_deref(),
        },
    )?;
    let v = json!({ "database": rows.database, "rows": rows.rows, "has_more": rows.has_more });
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
                let shown = match val {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a
                        .iter()
                        .map(|x| x.as_str().map(str::to_string).unwrap_or(x.to_string()))
                        .collect::<Vec<_>>()
                        .join(", "),
                    other => other.to_string(),
                };
                s.push_str(&format!("    {k}: {shown}\n"));
            }
        }
        if v["has_more"] == true {
            s.push_str("\n(more rows: raise --limit)\n");
        }
        s
    });
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
