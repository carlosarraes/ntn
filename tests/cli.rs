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
