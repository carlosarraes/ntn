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
