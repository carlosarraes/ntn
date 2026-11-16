//! Helpers over a Notion `recordMap` plus rich-text rendering.

use crate::client::{Client, get, merge_record_map, record_value};
use crate::error::Result;
use serde_json::{Value, json};

pub struct Store {
    pub map: Value,
    pub base: String,
}

impl Store {
    pub fn new(base: &str) -> Self {
        Self {
            map: json!({}),
            base: base.to_string(),
        }
    }

    pub fn merge(&mut self, record_map: &Value) {
        merge_record_map(&mut self.map, record_map);
    }

    pub fn block(&self, id: &str) -> Option<&Value> {
        get(&self.map, "block", id)
    }

    pub fn has(&self, table: &str, id: &str) -> bool {
        record_value(&self.map[table][id]).is_some()
    }

    /// Fetches any of `ids` not yet in `table`.
    pub fn fetch_missing(&mut self, client: &Client, table: &str, ids: &[String]) -> Result<()> {
        let mut missing: Vec<String> = ids
            .iter()
            .filter(|id| !self.has(table, id))
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            let r = client.sync_records(table, &missing)?;
            self.merge(&r);
        }
        Ok(())
    }

    pub fn user_name(&self, id: &str) -> String {
        get(&self.map, "notion_user", id)
            .and_then(|u| {
                u["name"].as_str().map(str::to_string).or_else(|| {
                    let first = u["given_name"].as_str().unwrap_or_default();
                    let last = u["family_name"].as_str().unwrap_or_default();
                    Some(format!("{first} {last}").trim().to_string()).filter(|s| !s.is_empty())
                })
            })
            .unwrap_or_else(|| id.to_string())
    }

    /// Title of a page, database page or database (collection).
    pub fn title(&self, id: &str) -> String {
        let Some(b) = self.block(id).or_else(|| get(&self.map, "collection", id)) else {
            return "Untitled".into();
        };
        if let Some(cid) = collection_id(b)
            && let Some(c) = get(&self.map, "collection", &cid)
        {
            let t = plain(&c["name"], self);
            if !t.is_empty() {
                return t;
            }
        }
        let t = plain(
            b.get("properties").map_or(&b["name"], |p| &p["title"]),
            self,
        );
        if t.is_empty() { "Untitled".into() } else { t }
    }
}

pub fn collection_id(block: &Value) -> Option<String> {
    block["format"]["collection_pointer"]["id"]
        .as_str()
        .or(block["collection_id"].as_str())
        .map(str::to_string)
}

/// Ids referenced from rich text: (users, pages).
pub fn refs(rich: &Value, users: &mut Vec<String>, pages: &mut Vec<String>) {
    for seg in rich.as_array().into_iter().flatten() {
        for deco in seg[1].as_array().into_iter().flatten() {
            match (deco[0].as_str(), deco[1].as_str()) {
                (Some("u"), Some(id)) => users.push(id.to_string()),
                (Some("p"), Some(id)) => pages.push(id.to_string()),
                _ => {}
            }
        }
    }
}

fn fmt_date(d: &Value) -> String {
    let start = d["start_date"].as_str().unwrap_or_default();
    let mut s = match d["start_time"].as_str() {
        Some(t) => format!("{start} {t}"),
        None => start.to_string(),
    };
    if let Some(end) = d["end_date"].as_str() {
        s.push_str(" → ");
        s.push_str(end);
    }
    s
}

/// Rich text without formatting; mentions become names/titles.
pub fn plain(rich: &Value, store: &Store) -> String {
    render(rich, store, false)
}

/// Rich text as Markdown.
pub fn markdown(rich: &Value, store: &Store) -> String {
    render(rich, store, true)
}

fn render(rich: &Value, store: &Store, md: bool) -> String {
    if let Some(s) = rich.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    for seg in rich.as_array().into_iter().flatten() {
        let mut text = seg[0].as_str().unwrap_or_default().to_string();
        let decos = seg[1].as_array().cloned().unwrap_or_default();
        let (mut bold, mut italic, mut strike, mut code, mut link) =
            (false, false, false, false, None);
        for d in &decos {
            match d[0].as_str().unwrap_or_default() {
                "u" => text = format!("@{}", store.user_name(d[1].as_str().unwrap_or_default())),
                "p" => {
                    let id = d[1].as_str().unwrap_or_default();
                    text = store.title(id);
                    if md {
                        link = Some(crate::ids::url_for(&store.base, id));
                    }
                }
                "d" => text = fmt_date(&d[1]),
                "e" => text = format!("${}$", d[1].as_str().unwrap_or_default()),
                "a" => link = d[1].as_str().map(str::to_string),
                "b" => bold = true,
                "i" => italic = true,
                "s" => strike = true,
                "c" => code = true,
                _ => {}
            }
        }
        if md && !text.trim().is_empty() {
            if code {
                text = format!("`{text}`");
            }
            if bold {
                text = format!("**{text}**");
            }
            if italic {
                text = format!("_{text}_");
            }
            if strike {
                text = format!("~~{text}~~");
            }
            if let Some(l) = link {
                text = format!("[{text}]({l})");
            }
        }
        out.push_str(&text);
    }
    out
}

pub fn ms_to_iso(ms: &Value) -> Option<String> {
    let ms = ms.as_i64().or_else(|| ms.as_f64().map(|f| f as i64))?;
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let mut s = Store::new("https://n.so");
        s.merge(&json!({
            "notion_user": {"u1": {"value": {"id": "u1", "name": "Ana"}}},
            "block": {"p1": {"value": {"id": "p1", "type": "page", "properties": {"title": [["Roadmap"]]}}}}
        }));
        s
    }

    #[test]
    fn renders_rich_text() {
        let rich = json!([
            ["Hi "], ["‣", [["u", "u1"]]], [" see "], ["‣", [["p", "p1"]]],
            [" "], ["bold", [["b"], ["i"]]], [" "], ["x", [["c"]]], [" "],
            ["site", [["a", "https://x.io"]]], [" due "], ["‣", [["d", {"type": "date", "start_date": "2026-01-02"}]]]
        ]);
        let s = store();
        assert_eq!(
            plain(&rich, &s),
            "Hi @Ana see Roadmap bold x site due 2026-01-02"
        );
        assert_eq!(
            markdown(&rich, &s),
            "Hi @Ana see [Roadmap](https://n.so/p1) _**bold**_ `x` [site](https://x.io) due 2026-01-02"
        );
    }

    #[test]
    fn collects_refs() {
        let (mut u, mut p) = (vec![], vec![]);
        refs(
            &json!([["‣", [["u", "u9"]]], ["‣", [["p", "p9"]]], ["x", [["b"]]]]),
            &mut u,
            &mut p,
        );
        assert_eq!((u, p), (vec!["u9".to_string()], vec!["p9".to_string()]));
    }
}
