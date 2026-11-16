use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Box<Value>,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: Box::new(json!({})),
        }
    }

    pub fn input(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::new("authentication", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn ambiguous(message: impl Into<String>, candidates: Value) -> Self {
        Self::new("ambiguous", message).with_details(json!({ "candidates": candidates }))
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new("configuration", message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        let mut err = Self::new("network", message);
        err.retryable = true;
        err
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Box::new(details);
        self
    }

    /// Maps a Notion HTTP error (`{errorId, name, message}`) onto a stable code.
    pub fn notion(method: &str, status: u16, body: &Value) -> Self {
        let name = body["name"].as_str().unwrap_or_default();
        let msg = body["message"].as_str().unwrap_or_default();
        let (code, hint) = match (status, name) {
            (401, _) | (_, "UnauthorizedError") => (
                "authentication",
                "session expired or invalid; run `ntn auth parse-curl --login` again",
            ),
            (403, _) => ("permission", "you don't have access to this"),
            (404, _) => ("not_found", "not found or not visible to you"),
            (400, _) | (_, "ValidationError") => ("invalid_input", "Notion rejected the request"),
            (429, _) => ("rate_limited", "Notion rate limit hit; retry later"),
            _ => ("notion_api", "Notion returned an error"),
        };
        let mut err = Self::new(
            code,
            format!("{method}: HTTP {status} {name} {msg} ({hint})"),
        );
        err.retryable = code == "rate_limited";
        err.details = Box::new(json!({ "method": method, "status": status, "name": name }));
        err
    }

    pub fn exit_code(&self) -> i32 {
        match self.code.as_str() {
            "invalid_input" => 2,
            "authentication" | "permission" => 3,
            "not_found" => 4,
            "ambiguous" => 5,
            "rate_limited" => 6,
            "network" => 7,
            "io" | "configuration" => 9,
            _ => 8,
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::new("io", format!("I/O error: {err}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        Self::new("io", format!("JSON error: {err}"))
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
