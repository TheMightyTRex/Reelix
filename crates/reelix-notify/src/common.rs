//! Shared helpers: typed access to validated settings and HTTP response checking.

use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use serde_json::{Map, Value};

use crate::error::NotifyError;
use crate::event::{EventKind, EventMask};

/// Validated settings object with typed getters.
pub(crate) struct Settings(Map<String, Value>);

impl Settings {
    /// Validates `raw` against `schema` (fills defaults, checks required fields).
    pub fn new(schema: &ProviderSchema, raw: &Value) -> Result<Self, NotifyError> {
        schema
            .validate(raw)
            .map(Self)
            .map_err(NotifyError::InvalidSettings)
    }

    pub fn opt_str(&self, key: &str) -> Option<String> {
        match self.0.get(key)? {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> Result<String, NotifyError> {
        self.opt_str(key)
            .ok_or_else(|| NotifyError::settings(format!("{key} is required")))
    }

    pub fn bool(&self, key: &str) -> bool {
        match self.0.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => matches!(s.as_str(), "true" | "1" | "yes"),
            Some(Value::Number(n)) => n.as_i64().is_some_and(|n| n != 0),
            _ => false,
        }
    }

    pub fn opt_i64(&self, key: &str) -> Option<i64> {
        match self.0.get(key)? {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    /// A list field: JSON array of strings, or a comma/newline separated string.
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.trim().to_string()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect(),
            Some(Value::String(s)) => s
                .split([',', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// An absolute http(s) URL, trailing slash removed.
    pub fn url(&self, key: &str) -> Result<String, NotifyError> {
        let raw = self.str(key)?;
        let parsed =
            url::Url::parse(&raw).map_err(|e| NotifyError::settings(format!("{key}: invalid URL ({e})")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(NotifyError::settings(format!("{key}: must be http or https")));
        }
        Ok(raw.trim_end_matches('/').to_string())
    }

    /// The `events` subscription (defaults to everything).
    pub fn events(&self) -> Result<EventMask, NotifyError> {
        match self.0.get("events") {
            None | Some(Value::Null) => Ok(EventMask::all()),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .and_then(EventKind::parse)
                        .ok_or_else(|| NotifyError::settings(format!("unknown event kind {v}")))
                })
                .collect::<Result<EventMask, _>>(),
            Some(_) => Err(NotifyError::settings("events must be a list")),
        }
    }
}

/// The `events` multi-select appended to every schema.
pub(crate) fn events_field() -> FieldSchema {
    let options = EventKind::ALL
        .into_iter()
        .filter(|k| *k != EventKind::Test)
        .map(|k| SelectOption {
            value: Value::String(k.as_str().into()),
            label: k.label().into(),
        })
        .collect();
    let all: Vec<Value> = EventKind::ALL
        .into_iter()
        .filter(|k| *k != EventKind::Test)
        .map(|k| Value::String(k.as_str().into()))
        .collect();
    FieldSchema::new("events", "Events", FieldKind::MultiSelect { options })
        .default_value(Value::Array(all))
        .help("Events that trigger this notification")
}

/// Builds a schema with the shared `events` field appended.
pub(crate) fn schema(
    implementation: &str,
    name: &str,
    description: &str,
    info_url: Option<&str>,
    mut fields: Vec<FieldSchema>,
) -> ProviderSchema {
    fields.push(events_field());
    ProviderSchema {
        implementation: implementation.into(),
        name: name.into(),
        description: Some(description.into()),
        info_url: info_url.map(String::from),
        fields,
    }
}

/// Maps non-success responses to [`NotifyError`]; returns the response on 2xx.
pub(crate) async fn check(resp: reqwest::Response) -> Result<reqwest::Response, NotifyError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status.as_u16() == 429 {
        let retry_after_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .map(|v| v.ceil() as u64);
        return Err(NotifyError::RateLimited { retry_after_secs });
    }
    let mut body = error_text(resp).await;
    truncate(&mut body, 500);
    if matches!(status.as_u16(), 401 | 403) {
        return Err(NotifyError::Unauthorized(body));
    }
    Err(NotifyError::Status {
        status: status.as_u16(),
        body,
    })
}

/// Largest JSON reply read from a notification service (after decompression).
pub(crate) const MAX_JSON_BYTES: usize = 1024 * 1024;
/// How much of an error body is read; only a short snippet is kept anyway.
const MAX_ERROR_BYTES: usize = 4 * 1024;

/// Reads a JSON reply, failing once the body exceeds [`MAX_JSON_BYTES`] so a hostile
/// endpoint (or gzip bomb) can't make Reelix buffer gigabytes.
pub(crate) async fn json<T: serde::de::DeserializeOwned>(
    mut resp: reqwest::Response,
) -> Result<T, NotifyError> {
    let too_big = || NotifyError::Api(format!("response larger than {} KB", MAX_JSON_BYTES / 1024));
    if resp.content_length().is_some_and(|n| n > MAX_JSON_BYTES as u64) {
        return Err(too_big());
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > MAX_JSON_BYTES {
            return Err(too_big());
        }
        out.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&out).map_err(|e| NotifyError::Api(format!("invalid response: {e}")))
}

/// The first few KB of an (error) body as text; read failures yield what was read so far.
pub(crate) async fn error_text(mut resp: reqwest::Response) -> String {
    let mut out = Vec::new();
    while out.len() < MAX_ERROR_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => out.extend_from_slice(&chunk),
            _ => break,
        }
    }
    out.truncate(MAX_ERROR_BYTES);
    String::from_utf8_lossy(&out).into_owned()
}

/// Truncates on a char boundary.
pub(crate) fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_accepts_array_or_csv() {
        let s = schema("x", "X", "", None, vec![]);
        let st = Settings::new(&s, &json!({"a": ["x", " y "], "b": "p, q\nr"})).unwrap();
        assert_eq!(st.list("a"), ["x", "y"]);
        assert_eq!(st.list("b"), ["p", "q", "r"]);
        assert!(st.events().unwrap().contains(EventKind::Grab));
        assert!(!st.events().unwrap().contains(EventKind::Test));
        let st = Settings::new(&s, &json!({"events": ["grab"]})).unwrap();
        assert_eq!(st.events().unwrap().iter().collect::<Vec<_>>(), [EventKind::Grab]);
        let st = Settings::new(&s, &json!({"events": ["nope"]})).unwrap();
        assert!(st.events().is_err());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let mut s = "ééé".to_string();
        truncate(&mut s, 3);
        assert_eq!(s, "é…");
    }
}
