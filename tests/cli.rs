mod common;

use common::{FAKE_TOKEN, FakeNotion, ntn, write_login};
use serde_json::json;

fn spaces_reply() -> serde_json::Value {
    json!({ "u-1": { "space": { "s-1": { "value": { "value": {
        "id": "s-1", "name": "Acme", "domain": "acme"
    }, "role": "editor" } } } } })
}

#[test]
fn talks_to_the_overridden_base_url_with_the_saved_session() {
    let fake = FakeNotion::start(|_, _| (200, spaces_reply()));
    let dir = tempfile::tempdir().unwrap();
    write_login(dir.path());

    let run = ntn(&fake.base, dir.path(), &["spaces"], &[]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json()["spaces"][0]["name"], "Acme");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        (seen[0].endpoint.as_str(), &seen[0].body),
        ("getSpaces", &json!({}))
    );
    assert!(
        seen[0]
            .headers
            .to_lowercase()
            .contains(&format!("cookie: token_v2={FAKE_TOKEN}"))
    );
}

#[test]
fn base_url_override_refuses_non_loopback_hosts() {
    let dir = tempfile::tempdir().unwrap();
    write_login(dir.path());

    let run = ntn("http://evil.example.com", dir.path(), &["spaces"], &[]);

    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "invalid_input");
}

const FOUND: &str = "1a2b3c4d-5e6f-4071-8293-a4b5c6d7e8f9";
const MISSING: &str = "9f8e7d6c-5b4a-4392-8170-6f5e4d3c2b1a";

fn page_block() -> serde_json::Value {
    json!({ "spaceId": "s-1", "value": { "value": {
        "id": FOUND, "type": "page", "alive": true,
        "properties": { "title": [["Hello world"]] }, "content": []
    }, "role": "reader" } })
}

/// Notion answers lookups of pages you cannot see with a 200 and a wrapper that holds no record.
fn pages_server() -> FakeNotion {
    FakeNotion::start(|endpoint, body| match endpoint {
        "loadPageChunk" => {
            let mut map = json!({ "__version__": 3 });
            if body["pageId"] == FOUND {
                map["block"] = json!({ FOUND: page_block() });
            }
            (200, json!({ "recordMap": map, "cursor": { "stack": [] } }))
        }
        "syncRecordValues" => {
            let mut blocks = json!({});
            for r in body["requests"].as_array().into_iter().flatten() {
                let id = r["pointer"]["id"].as_str().unwrap();
                blocks[id] = if id == FOUND {
                    page_block()
                } else {
                    json!({ "spaceId": "s-1", "value": { "value": null, "role": "none" } })
                };
            }
            (200, json!({ "recordMap": { "block": blocks } }))
        }
        other => (404, json!({ "name": "NotFound", "message": other })),
    })
}

#[test]
fn reading_a_missing_page_is_a_not_found_error() {
    let fake = pages_server();
    let dir = tempfile::tempdir().unwrap();
    write_login(dir.path());

    let run = ntn(&fake.base, dir.path(), &["page", "read", MISSING], &[]);

    assert_eq!(run.code, 4, "stdout: {}", run.stdout);
    assert!(run.stdout.is_empty(), "stdout: {}", run.stdout);
    let err = run.error();
    assert_eq!(err["error"]["code"], "not_found");
    assert!(err["error"]["message"].as_str().unwrap().contains(MISSING));
}

#[test]
fn reading_pages_where_one_is_missing_fails_naming_the_missing_id() {
    let fake = pages_server();
    let dir = tempfile::tempdir().unwrap();
    write_login(dir.path());

    let run = ntn(
        &fake.base,
        dir.path(),
        &["page", "read", FOUND, MISSING],
        &[],
    );

    assert_eq!(run.code, 4, "stdout: {}", run.stdout);
    assert!(run.stdout.is_empty(), "stdout: {}", run.stdout);
    let err = run.error();
    assert_eq!(err["error"]["code"], "not_found");
    assert_eq!(err["error"]["details"]["missing"], json!([MISSING]));
}

#[test]
fn reading_an_existing_page_still_works() {
    let fake = pages_server();
    let dir = tempfile::tempdir().unwrap();
    write_login(dir.path());

    let run = ntn(&fake.base, dir.path(), &["page", "read", FOUND], &[]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json()["title"], "Hello world");
}

fn page_id(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn block(n: usize) -> serde_json::Value {
    json!({ "spaceId": "s-1", "value": { "value": {
        "id": page_id(n), "type": "page", "alive": true,
        "properties": { "title": [[format!("Page {n}")]] },
        "last_edited_time": 1_700_000_000_000_i64
    }, "role": "reader" } })
}

/// A search endpoint that, like Notion's, honours `limit` and knows nothing about offsets.
fn search_server(hits: Vec<serde_json::Value>) -> FakeNotion {
    FakeNotion::start(move |endpoint, body| match endpoint {
        "getSpaces" => (200, spaces_reply()),
        "search" => {
            let limit = body["limit"].as_u64().unwrap() as usize;
            let page: Vec<_> = hits.iter().take(limit).cloned().collect();
            let mut blocks = json!({});
            for h in &page {
                let n: usize = h["id"].as_str().unwrap()[24..].parse().unwrap();
                blocks[h["id"].as_str().unwrap()] = block(n);
            }
            (
                200,
                json!({ "total": hits.len(), "results": page, "recordMap": { "block": blocks } }),
            )
        }
        other => (404, json!({ "name": "NotFound", "message": other })),
    })
}

#[test]
fn search_only_shows_a_snippet_when_notion_sent_highlight_text() {
    let fake = search_server(vec![
        json!({ "id": page_id(1), "highlight": { "text": "see <gzkNfoUU>needle</gzkNfoUU> here", "title": "" } }),
        json!({ "id": page_id(2), "highlight": { "text": "", "title": "<gzkNfoUU>Page</gzkNfoUU> 2" } }),
        json!({ "id": page_id(3) }),
        json!({ "id": page_id(4), "highlights": { "textHighlights": ["a <gzkNfoUU>b</gzkNfoUU>", { "text": "c" }] } }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    write_login(dir.path());

    let run = ntn(&fake.base, dir.path(), &["search", "needle"], &[]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let results = run.json()["results"].clone();
    assert_eq!(results[0]["snippet"], "see needle here");
    for empty in [1, 2] {
        assert!(
            results[empty].get("snippet").is_none(),
            "hit {empty} must not carry an empty snippet: {}",
            results[empty]
        );
    }
    assert_eq!(results[3]["snippet"], "a b … c");
    assert_eq!(results[1]["title"], "Page 2");
}
