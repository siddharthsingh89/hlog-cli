use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_API: &str = "https://hlog.dev";

#[derive(Deserialize, Debug, Clone)]
pub struct Line {
    pub handle: String,
    pub ts_ms: i64,
    pub level: String,
    pub text: String,
    pub cursor: String,
}

#[derive(Deserialize, Debug)]
pub struct Page {
    pub lines: Vec<Line>,
    #[allow(dead_code)]
    pub next: Option<String>,
}

#[derive(Debug)]
pub enum ApiError {
    /// HTTP error with the API's {error:{code,message}} body, plus Retry-After for 429s.
    Http { status: u16, code: String, message: String, retry_after: Option<u64> },
    Network(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Http { status, message, .. } => write!(f, "{message} (HTTP {status})"),
            ApiError::Network(m) => write!(f, "network error: {m}"),
        }
    }
}

pub struct Client {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("hlog-cli/", env!("CARGO_PKG_VERSION")))
            .build();
        Client { base: base.trim_end_matches('/').to_string(), token, agent }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api{}", self.base, path)
    }

    fn send(&self, req: ureq::Request, body: Option<Value>) -> Result<Option<Value>, ApiError> {
        let req = match &self.token {
            Some(t) => req.set("Authorization", &format!("Bearer {t}")),
            None => req,
        };
        let res = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match res {
            Ok(r) => {
                if r.status() == 204 {
                    return Ok(None);
                }
                let text = r.into_string().map_err(|e| ApiError::Network(e.to_string()))?;
                if text.is_empty() {
                    return Ok(None);
                }
                serde_json::from_str(&text).map(Some).map_err(|e| ApiError::Network(format!("bad response: {e}")))
            }
            Err(ureq::Error::Status(status, r)) => {
                let retry_after = r.header("Retry-After").and_then(|v| v.parse().ok());
                let v: Value = r.into_json().unwrap_or(Value::Null);
                let code = v["error"]["code"].as_str().unwrap_or("error").to_string();
                let message = v["error"]["message"].as_str().unwrap_or("request failed").to_string();
                Err(ApiError::Http { status, code, message, retry_after })
            }
            Err(e) => Err(ApiError::Network(e.to_string())),
        }
    }

    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ApiError> {
        let mut req = self.agent.get(&self.url(path));
        for (k, v) in query {
            req = req.query(k, v);
        }
        Ok(self.send(req, None)?.unwrap_or(Value::Null))
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Option<Value>, ApiError> {
        self.send(self.agent.post(&self.url(path)), Some(body))
    }

    pub fn delete(&self, path: &str) -> Result<(), ApiError> {
        self.send(self.agent.delete(&self.url(path)), None).map(|_| ())
    }

    pub fn append(&self, text: &str, level: Option<&str>) -> Result<Line, ApiError> {
        let mut body = json!({ "text": text });
        if let Some(l) = level {
            body["level"] = json!(l);
        }
        let v = self.post("/lines", body)?.unwrap_or(Value::Null);
        serde_json::from_value(v).map_err(|e| ApiError::Network(format!("bad response: {e}")))
    }

    pub fn page(&self, path: &str, query: &[(&str, String)]) -> Result<Page, ApiError> {
        let v = self.get(path, query)?;
        serde_json::from_value(v).map_err(|e| ApiError::Network(format!("bad response: {e}")))
    }
}
