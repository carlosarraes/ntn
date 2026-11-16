//! Query a database (collection) through one of its views.

use crate::client::{Client, get};
use crate::error::{AppError, Result};
use crate::ids::{Target, url_for};
use crate::records::{Store, collection_id, ms_to_iso, plain, refs};
use serde_json::{Map, Value, json};

pub struct Query<'a> {
    pub limit: usize,
    pub search: Option<&'a str>,
}

pub struct Rows {
    pub database: Value,
    pub rows: Vec<Value>,
    pub has_more: bool,
}

pub fn query(client: &Client, store: &mut Store, target: &Target, q: Query) -> Result<Rows> {
    store.fetch_missing(client, "block", std::slice::from_ref(&target.id))?;
    let block = store.block(&target.id).cloned().ok_or_else(|| {
        AppError::not_found(format!("{} not found or not shared with you", target.id))
    })?;
    let cid = collection_id(&block).ok_or_else(|| {
        AppError::input(format!(
            "{} is a {} block, not a database; use `ntn page read`",
            target.id,
            block["type"].as_str().unwrap_or("?")
        ))
    })?;
    let view_id = target
        .view_id
        .clone()
        .or_else(|| block["view_ids"][0].as_str().map(str::to_string))
        .ok_or_else(|| AppError::input("database has no views; pass a URL with ?v=<view>"))?;
    let space = block["space_id"]
        .as_str()
        .or(client.space_id())
        .unwrap_or_default()
        .to_string();
    store.fetch_missing(client, "collection", std::slice::from_ref(&cid))?;

    let r = client.call(
        "queryCollection",
        json!({
            "collection": { "id": cid, "spaceId": space },
            "collectionView": { "id": view_id, "spaceId": space },
            "loader": {
                "type": "reducer",
                "reducers": { "collection_group_results": { "type": "results", "limit": q.limit } },
                "searchQuery": q.search.unwrap_or_default(),
                "userTimeZone": "UTC",
            },
        }),
    )?;
    store.merge(&r["recordMap"]);
    let group = &r["result"]["reducerResults"]["collection_group_results"];
    let ids: Vec<String> = group["blockIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    store.fetch_missing(client, "block", &ids)?;

    let schema = get(&store.map, "collection", &cid)
        .map(|c| c["schema"].clone())
        .unwrap_or_default();
    let (mut users, mut pages) = (vec![], vec![]);
    for id in &ids {
        if let Some(b) = store.block(id) {
            for v in b["properties"]
                .as_object()
                .into_iter()
                .flat_map(|o| o.values())
            {
                refs(v, &mut users, &mut pages);
            }
            for k in ["created_by_id", "last_edited_by_id"] {
                users.extend(b[k].as_str().map(str::to_string));
            }
        }
    }
    store.fetch_missing(client, "notion_user", &users)?;
    store.fetch_missing(client, "block", &pages)?;

    let rows = ids
        .iter()
        .filter_map(|id| store.block(id).map(|b| row(store, &schema, b)))
        .collect::<Vec<_>>();
    let total = group["total"].as_u64().unwrap_or(rows.len() as u64);
    let database = json!({
        "id": target.id,
        "collection_id": cid,
        "view_id": view_id,
        "title": store.title(&target.id),
        "url": url_for(&store.base, &target.id),
        "columns": schema.as_object().map(|s| s.values().map(|p| json!({ "name": p["name"], "type": p["type"] })).collect::<Vec<_>>()),
    });
    Ok(Rows {
        database,
        has_more: total > rows.len() as u64,
        rows,
    })
}

fn row(store: &Store, schema: &Value, b: &Value) -> Value {
    let id = b["id"].as_str().unwrap_or_default();
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("url".into(), json!(url_for(&store.base, id)));
    let Some(schema) = schema.as_object() else {
        return Value::Object(out);
    };
    let mut ordered: Vec<(&String, &Value)> = schema.iter().collect();
    ordered.sort_by_key(|(_, d)| d["type"] != "title");
    for (pid, def) in ordered {
        let name = def["name"].as_str().unwrap_or(pid).to_string();
        let raw = &b["properties"][pid];
        let value = match def["type"].as_str().unwrap_or_default() {
            "created_time" => ms_to_iso(&b["created_time"]).map(Value::from),
            "last_edited_time" => ms_to_iso(&b["last_edited_time"]).map(Value::from),
            "created_by" => b["created_by_id"]
                .as_str()
                .map(|u| json!(store.user_name(u))),
            "last_edited_by" => b["last_edited_by_id"]
                .as_str()
                .map(|u| json!(store.user_name(u))),
            _ if raw.is_null() => None,
            "checkbox" => Some(json!(plain(raw, store) == "Yes")),
            "number" => {
                let s = plain(raw, store);
                Some(s.parse::<f64>().map(Value::from).unwrap_or(json!(s)))
            }
            "multi_select" => Some(json!(
                plain(raw, store)
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            )),
            "person" | "relation" => {
                let names: Vec<String> = raw
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|seg| {
                        plain(&json!([seg]), store)
                            .trim_start_matches('@')
                            .to_string()
                    })
                    .filter(|s| !s.trim().is_empty() && s.trim() != ",")
                    .collect();
                Some(json!(names))
            }
            _ => Some(json!(plain(raw, store))),
        };
        if let Some(v) = value {
            out.insert(name, v);
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_row_properties_by_schema() {
        let mut store = Store::new("https://n.so");
        store.merge(&json!({
            "notion_user": {"u1": {"value": {"id": "u1", "name": "Ana"}}},
            "block": {"r1": {"value": {
                "id": "r1", "created_time": 1700000000000i64, "last_edited_by_id": "u1",
                "properties": {
                    "title": [["Ship it"]], "st": [["Done"]], "tg": [["a,b"]], "ck": [["Yes"]],
                    "nm": [["42"]], "ow": [["‣", [["u", "u1"]]]],
                    "dt": [["‣", [["d", {"type": "date", "start_date": "2026-02-01"}]]]]
                }
            }}}
        }));
        let schema = json!({
            "title": {"name": "Name", "type": "title"}, "st": {"name": "Status", "type": "status"},
            "tg": {"name": "Tags", "type": "multi_select"}, "ck": {"name": "Done", "type": "checkbox"},
            "nm": {"name": "Points", "type": "number"}, "ow": {"name": "Owner", "type": "person"},
            "dt": {"name": "Due", "type": "date"}, "ct": {"name": "Created", "type": "created_time"},
            "le": {"name": "Editor", "type": "last_edited_by"}, "empty": {"name": "Notes", "type": "text"}
        });
        let r = row(&store, &schema, store.block("r1").unwrap());
        assert_eq!(
            r,
            json!({
                "id": "r1", "url": "https://n.so/r1", "Name": "Ship it", "Status": "Done",
                "Tags": ["a", "b"], "Done": true, "Points": 42.0, "Owner": ["Ana"],
                "Due": "2026-02-01", "Created": "2023-11-14T22:13:20Z", "Editor": "Ana"
            })
        );
    }
}
