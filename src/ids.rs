use crate::error::{AppError, Result};
use regex::Regex;
use std::sync::LazyLock;

static HEX32: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([0-9a-f]{8})-?([0-9a-f]{4})-?([0-9a-f]{4})-?([0-9a-f]{4})-?([0-9a-f]{12})")
        .unwrap()
});

#[derive(Debug, PartialEq)]
pub struct Target {
    pub id: String,
    /// `?v=` view id from a database URL
    pub view_id: Option<String>,
    /// `#<block>` anchor or `?p=` peeked page
    pub peek_id: Option<String>,
}

fn dashed(s: &str) -> Option<String> {
    HEX32
        .captures(s)
        .map(|c| format!("{}-{}-{}-{}-{}", &c[1], &c[2], &c[3], &c[4], &c[5]).to_lowercase())
}

/// Accepts a page/database URL (`.../Title-<32hex>?v=<view>&p=<page>`) or a raw id.
pub fn parse(input: &str) -> Result<Target> {
    let bad = || AppError::input(format!("not a Notion URL or id: {input}"));
    let input = input.trim();
    if !input.starts_with("http") {
        return Ok(Target {
            id: dashed(input).ok_or_else(bad)?,
            view_id: None,
            peek_id: None,
        });
    }
    let url = url::Url::parse(input).map_err(|_| bad())?;
    let last = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or_default();
    // The id is the trailing 32 hex chars of the last path segment.
    let tail = last.len().saturating_sub(32);
    let id = dashed(&last[tail..])
        .or_else(|| dashed(last))
        .ok_or_else(bad)?;
    let query = |k: &str| {
        url.query_pairs()
            .find(|(q, _)| q == k)
            .and_then(|(_, v)| dashed(&v))
    };
    let peek_id = query("p").or_else(|| url.fragment().and_then(dashed));
    Ok(Target {
        id,
        view_id: query("v"),
        peek_id,
    })
}

pub fn url_for(base: &str, id: &str) -> String {
    format!("{base}/{}", id.replace('-', ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls_and_ids() {
        let t = parse("https://www.notion.so/acme/Roadmap-Q3-1a2b3c4d5e6f40718293a4b5c6d7e8f9?v=0123456789abcdef0123456789abcdef&pvs=4").unwrap();
        assert_eq!(t.id, "1a2b3c4d-5e6f-4071-8293-a4b5c6d7e8f9");
        assert_eq!(
            t.view_id.as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        let t = parse("1A2B3C4D5E6F40718293A4B5C6D7E8F9").unwrap();
        assert_eq!(t.id, "1a2b3c4d-5e6f-4071-8293-a4b5c6d7e8f9");
        let t = parse("https://www.notion.so/1a2b3c4d5e6f40718293a4b5c6d7e8f9#0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(
            t.peek_id.as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert!(parse("https://www.notion.so/acme/no-id").is_err());
        assert!(parse("hello").is_err());
    }

    #[test]
    fn builds_urls() {
        assert_eq!(
            url_for("https://www.notion.so", "1a2b-3c"),
            "https://www.notion.so/1a2b3c"
        );
    }
}
