use crate::config::Session;
use crate::error::{AppError, Result};
use serde_json::{Value, json};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Every Notion endpoint ntn may call. Anything else is refused before it hits
/// the network, so no code path can create, edit, move or delete content.
pub const READ_METHODS: &[&str] = &[
    "getSpaces",
    "loadCachedPageChunk",
    "loadPageChunk",
    "loadUserContent",
    "queryCollection",
    "search",
    "syncRecordValues",
];

const MIN_SPACING: Duration = Duration::from_millis(150);
const MAX_RETRIES: u32 = 3;
const SYNC_BATCH: usize = 100;

pub struct Client {
    http: reqwest::blocking::Client,
    session: Session,
    base: String,
    space_id: Option<String>,
    last_call: Cell<Option<Instant>>,
    verbose: bool,
}

impl Client {
    pub fn new(session: Session, space_id: Option<String>, verbose: bool) -> Result<Self> {
        let base = crate::config::normalize_base_url(&session.base_url)?;
        Self::with_base(session, space_id, base, verbose)
    }

    pub fn with_base(
        session: Session,
        space_id: Option<String>,
        base: String,
        verbose: bool,
    ) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| AppError::network(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            http,
            session,
            base,
            space_id,
            last_call: Cell::new(None),
            verbose,
        })
    }

    pub fn user_id(&self) -> &str {
        &self.session.user_id
    }

    pub fn space_id(&self) -> Option<&str> {
        self.space_id.as_deref()
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn call(&self, method: &str, body: Value) -> Result<Value> {
        if !READ_METHODS.contains(&method) {
            return Err(AppError::new(
                "forbidden",
                format!("{method} is not a read-only endpoint; ntn refuses to call it"),
            ));
        }
        let mut attempt = 0;
        loop {
            self.pace();
            let started = Instant::now();
            let outcome = self.send(method, &body);
            if self.verbose {
                eprintln!(
                    "ntn: {method} attempt={} {}ms",
                    attempt + 1,
                    started.elapsed().as_millis()
                );
            }
            match outcome {
                Ok(v) => return Ok(v),
                Err((err, wait)) if err.retryable && attempt < MAX_RETRIES => {
                    std::thread::sleep(wait.unwrap_or(Duration::from_secs(1 << attempt)));
                    attempt += 1;
                }
                Err((err, _)) => return Err(err),
            }
        }
    }

    /// Fetches records by id (`table` = block, collection, collection_view, notion_user, space).
    pub fn sync_records(&self, table: &str, ids: &[String]) -> Result<Value> {
        let mut merged = json!({});
        for chunk in ids.chunks(SYNC_BATCH) {
            let requests: Vec<Value> = chunk
                .iter()
                .map(|id| {
                    let mut pointer = json!({ "table": table, "id": id });
                    if let Some(space) = &self.space_id {
                        pointer["spaceId"] = json!(space);
                    }
                    json!({ "pointer": pointer, "version": -1 })
                })
                .collect();
            let r = self.call("syncRecordValues", json!({ "requests": requests }))?;
            merge_record_map(&mut merged, &r["recordMap"]);
        }
        Ok(merged)
    }

    fn pace(&self) {
        if let Some(last) = self.last_call.get() {
            let elapsed = last.elapsed();
            if elapsed < MIN_SPACING {
                std::thread::sleep(MIN_SPACING - elapsed);
            }
        }
        self.last_call.set(Some(Instant::now()));
    }

    fn send(
        &self,
        method: &str,
        body: &Value,
    ) -> std::result::Result<Value, (AppError, Option<Duration>)> {
        let s = &self.session;
        let mut req = self
            .http
            .post(format!("{}/api/v3/{method}", self.base))
            .header(
                "Cookie",
                format!("token_v2={}; notion_user_id={}", s.token_v2, s.user_id),
            )
            .header("x-notion-active-user-header", &s.user_id)
            .header("Origin", &self.base)
            .header(
                "User-Agent",
                s.user_agent
                    .as_deref()
                    .unwrap_or(concat!("ntn/", env!("CARGO_PKG_VERSION"))),
            )
            .json(body);
        if let Some(space) = &self.space_id {
            req = req.header("x-notion-space-id", space);
        }
        let resp = req
            .send()
            .map_err(|e| (AppError::network(format!("{method}: {e}")), None))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err((
                AppError::auth(format!(
                    "{method}: Notion redirected (session likely expired); log in again"
                )),
                None,
            ));
        }
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|s| Duration::from_secs(s.min(60)));
        let text = resp
            .text()
            .map_err(|e| (AppError::network(format!("{method}: {e}")), None))?;
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            if parsed.is_null() {
                return Err((
                    AppError::network(format!("{method}: invalid JSON response")),
                    None,
                ));
            }
            return Ok(parsed);
        }
        if status.is_server_error() {
            return Err((AppError::network(format!("{method}: HTTP {status}")), None));
        }
        Err((
            AppError::notion(method, status.as_u16(), &parsed),
            retry_after,
        ))
    }
}

/// Notion wraps records as `{value:{...},role}` or, in newer responses,
/// `{spaceId, value:{value:{...},role}}`. Returns the inner record either way.
pub fn record_value(entry: &Value) -> Option<&Value> {
    let v = &entry["value"];
    let inner = if v["value"].is_object() {
        &v["value"]
    } else {
        v
    };
    inner.is_object().then_some(inner)
}

pub fn merge_record_map(into: &mut Value, from: &Value) {
    let Some(tables) = from.as_object() else {
        return;
    };
    for (table, records) in tables {
        let Some(records) = records.as_object() else {
            continue;
        };
        if !into[table].is_object() {
            into[table] = json!({});
        }
        for (id, rec) in records {
            if record_value(rec).is_some() {
                into[table][id] = rec.clone();
            }
        }
    }
}

pub fn get<'a>(map: &'a Value, table: &str, id: &str) -> Option<&'a Value> {
    record_value(&map[table][id])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn serve(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            head.push_str(&String::from_utf8(body).unwrap());
            (&stream).write_all(response.as_bytes()).unwrap();
            tx.send(head).unwrap();
        });
        (format!("http://{addr}"), rx)
    }

    fn session() -> Session {
        Session {
            token_v2: "v03%3Atok".into(),
            user_id: "u-1".into(),
            base_url: "https://www.notion.so".into(),
            user_agent: Some("UA/1".into()),
        }
    }

    #[test]
    fn refuses_write_endpoints() {
        let c = Client::with_base(session(), None, "http://127.0.0.1:9".into(), false).unwrap();
        for m in [
            "submitTransaction",
            "saveTransactions",
            "deleteBlocks",
            "enqueueTask",
        ] {
            assert_eq!(c.call(m, json!({})).unwrap_err().code, "forbidden");
        }
    }

    #[test]
    fn sends_cookie_headers_and_json() {
        let (base, rx) = serve("200 OK", r#"{"results":[]}"#);
        let c = Client::with_base(session(), Some("s-1".into()), base, false).unwrap();
        c.call("search", json!({"query": "x"})).unwrap();
        let req = rx.recv().unwrap();
        let lower = req.to_lowercase();
        assert!(lower.starts_with("post /api/v3/search"));
        assert!(lower.contains("cookie: token_v2=v03%3atok; notion_user_id=u-1"));
        assert!(lower.contains("x-notion-active-user-header: u-1"));
        assert!(lower.contains("x-notion-space-id: s-1"));
        assert!(lower.contains("user-agent: ua/1"));
        assert!(req.ends_with(r#"{"query":"x"}"#));
    }

    #[test]
    fn http_errors_map_to_codes() {
        let (base, _rx) = serve(
            "401 Unauthorized",
            r#"{"name":"UnauthorizedError","message":"Token was invalid or expired."}"#,
        );
        let c = Client::with_base(session(), None, base, false).unwrap();
        let err = c.call("loadUserContent", json!({})).unwrap_err();
        assert_eq!((err.code.as_str(), err.exit_code()), ("authentication", 3));
    }

    #[test]
    fn record_value_handles_both_shapes() {
        let old = json!({"role":"reader","value":{"id":"a"}});
        let new = json!({"spaceId":"s","value":{"role":"reader","value":{"id":"a"}}});
        assert_eq!(record_value(&old).unwrap()["id"], "a");
        assert_eq!(record_value(&new).unwrap()["id"], "a");
        assert!(record_value(&json!({"role":"none"})).is_none());
    }
}
