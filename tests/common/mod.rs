//! A tiny fake Notion server plus a runner that spawns the real `ntn` binary
//! against it. Nothing here talks to the network beyond 127.0.0.1.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

pub const FAKE_TOKEN: &str = "fake-token-v2";

pub struct Seen {
    pub endpoint: String,
    pub body: Value,
    pub headers: String,
}

pub struct FakeNotion {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeNotion {
    /// `handler(endpoint, body)` returns the HTTP status and JSON body to reply with.
    pub fn start(handler: impl Fn(&str, &Value) -> (u16, Value) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut headers = String::new();
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let mut raw = vec![0; len];
                reader.read_exact(&mut raw).unwrap();
                let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                let endpoint = request_line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.rsplit('/').next())
                    .unwrap_or_default()
                    .to_string();
                let (status, reply) = handler(&endpoint, &body);
                log.lock().unwrap().push(Seen {
                    endpoint,
                    body,
                    headers,
                });
                let reply = reply.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = (&stream).write_all(response.as_bytes());
            }
        });
        Self { base, seen }
    }
}

impl FakeNotion {
    pub fn endpoints(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|s| s.endpoint.clone()).collect()
    }

    /// The `Cookie` header of every request, in order.
    pub fn cookies(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter()
            .map(|s| {
                s.headers
                    .lines()
                    .find_map(|l| l.strip_prefix("cookie: ").or(l.strip_prefix("Cookie: ")))
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }

    pub fn error(&self) -> Value {
        serde_json::from_str(&self.stderr)
            .unwrap_or_else(|e| panic!("stderr is not JSON ({e}): {}", self.stderr))
    }
}

/// Saved login with a fake token, as `ntn auth login` would have written it.
pub fn write_login(config_dir: &Path) {
    let accounts = json!({
        "default_account": "fake",
        "accounts": { "fake": {
            "name": "Fake", "email": "fake@example.com",
            "token_v2": FAKE_TOKEN, "user_id": "u-1",
            "base_url": "https://www.notion.so"
        }}
    });
    std::fs::write(config_dir.join("accounts.json"), accounts.to_string()).unwrap();
}

/// Spawns the real binary with a scrubbed environment pointing at `base`.
pub fn ntn(base: &str, config_dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_ntn"))
        .args(args)
        .env_clear()
        .env("HOME", config_dir)
        .env("NTN_CONFIG_DIR", config_dir)
        .env("NTN_BASE_URL", base)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}
