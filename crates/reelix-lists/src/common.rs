//! Helpers shared by the adapters: settings access, HTTP status mapping, common schema fields.

use std::time::Duration;

use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{DEFAULT_SYNC_INTERVAL_HOURS, ListError, SYNC_INTERVAL_FIELD};

/// Validated settings with typed accessors.
pub(crate) struct Settings(Map<String, Value>);

impl Settings {
    pub fn validate(schema: &ProviderSchema, v: &Value) -> Result<Self, ListError> {
        schema.validate(v).map(Self).map_err(ListError::InvalidSettings)
    }

    /// Non-empty trimmed string (numbers are stringified).
    pub fn str(&self, k: &str) -> Option<String> {
        match self.0.get(k) {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn req(&self, k: &str) -> Result<String, ListError> {
        self.str(k)
            .ok_or_else(|| ListError::InvalidSettings(format!("{k} is required")))
    }

    pub fn u64(&self, k: &str) -> Option<u64> {
        match self.0.get(k) {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
    }

    /// The per-instance `base_url` override, else `fallback`; trailing slashes removed.
    pub fn base_url(&self, fallback: &str) -> String {
        self.str("base_url")
            .unwrap_or_else(|| fallback.to_string())
            .trim_end_matches('/')
            .to_string()
    }

    /// A required server URL (Jellyfin, Reelix …), trailing slashes removed.
    pub fn server_url(&self, k: &str) -> Result<String, ListError> {
        let u = self.req(k)?;
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err(ListError::InvalidSettings(format!(
                "{k} must start with http:// or https://"
            )));
        }
        Ok(u.trim_end_matches('/').to_string())
    }
}

/// Sends a request and maps non-success statuses to typed errors.
pub(crate) async fn send(req: RequestBuilder) -> Result<Response, ListError> {
    let resp = req.send().await?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    // Path only: query strings may contain API keys.
    let path = resp.url().path().to_string();
    Err(match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ListError::Unauthorized,
        StatusCode::NOT_FOUND => ListError::NotFound(path),
        StatusCode::TOO_MANY_REQUESTS => ListError::RateLimited {
            retry_after: resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok())
                .map(Duration::from_secs),
        },
        _ => {
            let body = error_text(resp).await;
            ListError::Status {
                status: status.as_u16(),
                body: body.chars().take(200).collect(),
            }
        }
    })
}

/// Largest list response body read (after decompression).
pub(crate) const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// How much of an error body is read; only a short snippet is kept anyway.
const MAX_ERROR_BYTES: usize = 8 * 1024;

/// Reads a response body, failing once it exceeds `limit` bytes (a hostile or broken
/// server can't make Reelix buffer gigabytes, e.g. via a gzip bomb).
pub(crate) async fn read_limited(mut resp: Response, limit: usize) -> Result<Vec<u8>, ListError> {
    let too_big = || ListError::Parse(format!("response larger than {} MB", limit / (1024 * 1024)));
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_big());
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > limit {
            return Err(too_big());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// The body as (lossy UTF-8) text, capped at [`MAX_BODY_BYTES`].
pub(crate) async fn text(resp: Response) -> Result<String, ListError> {
    let bytes = read_limited(resp, MAX_BODY_BYTES).await?;
    Ok(String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// The first few KB of an (error) body as text; read failures yield what was read so far.
pub(crate) async fn error_text(mut resp: Response) -> String {
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

/// Decodes a JSON body with parse errors mapped to [`ListError::Parse`].
pub(crate) async fn json<T: DeserializeOwned>(resp: Response) -> Result<T, ListError> {
    let bytes = read_limited(resp, MAX_BODY_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|e| ListError::Parse(e.to_string()))
}

/// `send` + `json`.
pub(crate) async fn get_json<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ListError> {
    json(send(req).await?).await
}

/// Year from an ISO date or bare year string (`"1999-03-31"`, `"1999"`).
pub(crate) fn year_from_date(s: Option<&str>) -> Option<i32> {
    s?.get(..4)?.parse().ok()
}

/// Lenient u64 from a JSON number or numeric string.
pub(crate) fn value_u64(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Lenient i32 from a JSON number or numeric/date string.
pub(crate) fn value_year(v: Option<&Value>) -> Option<i32> {
    match v? {
        Value::Number(n) => n.as_i64().and_then(|n| i32::try_from(n).ok()),
        Value::String(s) => year_from_date(Some(s.trim())),
        _ => None,
    }
}

/// Lenient non-empty string from a JSON string.
pub(crate) fn value_str(v: Option<&Value>) -> Option<String> {
    v?.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Normalises an IMDb id (`tt…`), rejecting anything else.
pub(crate) fn imdb_id(s: Option<String>) -> Option<String> {
    s.filter(|s| s.len() > 2 && s.starts_with("tt") && s[2..].bytes().all(|b| b.is_ascii_digit()))
}

/// Normalises a MusicBrainz id (lower-case UUID), rejecting anything else.
pub(crate) fn mbid(s: Option<String>) -> Option<String> {
    let s = s?.trim().to_ascii_lowercase();
    let ok = s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    ok.then_some(s)
}

/// The id after `musicbrainz.org/<entity>/` in a URL (JSPF identifiers), or a bare MBID.
pub(crate) fn mbid_from_url(s: &str, entity: &str) -> Option<String> {
    let needle = format!("musicbrainz.org/{entity}/");
    let id = match s.split_once(&needle) {
        Some((_, rest)) => rest.split(['/', '?', '#']).next().unwrap_or_default(),
        None => s,
    };
    mbid(Some(id.to_string()))
}

pub(crate) fn sync_interval_field() -> FieldSchema {
    FieldSchema::new(SYNC_INTERVAL_FIELD, "Sync interval (hours)", FieldKind::Number)
        .default_value(DEFAULT_SYNC_INTERVAL_HOURS)
        .help("How often the list is re-fetched (minimum 1 hour).")
}

pub(crate) fn base_url_field(default: &str) -> FieldSchema {
    FieldSchema::new("base_url", "API base URL", FieldKind::Url)
        .placeholder(default)
        .help("Only change this for proxies or testing.")
        .advanced()
}

pub(crate) fn select(pairs: &[(&str, &str)]) -> FieldKind {
    FieldKind::Select {
        options: pairs
            .iter()
            .map(|(v, l)| SelectOption {
                value: Value::String((*v).into()),
                label: (*l).into(),
            })
            .collect(),
    }
}

/// Which media kinds a mixed-source list should return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KindFilter {
    Movies,
    Series,
    Both,
}

impl KindFilter {
    pub const OPTIONS: &'static [(&'static str, &'static str)] = &[
        ("both", "Movies and series"),
        ("movies", "Movies only"),
        ("series", "Series only"),
    ];

    pub fn parse(s: Option<String>) -> Result<Self, ListError> {
        match s.as_deref().unwrap_or("both") {
            "both" => Ok(Self::Both),
            "movies" | "movie" => Ok(Self::Movies),
            "series" | "shows" | "tv" => Ok(Self::Series),
            o => Err(ListError::InvalidSettings(format!("unknown media filter {o:?}"))),
        }
    }

    pub fn allows(self, k: MediaKind) -> bool {
        match self {
            Self::Both => true,
            Self::Movies => k == MediaKind::Movie,
            Self::Series => k == MediaKind::Series,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_parsers() {
        assert_eq!(year_from_date(Some("1999-03-31")), Some(1999));
        assert_eq!(year_from_date(Some("")), None);
        assert_eq!(value_u64(Some(&Value::String("42".into()))), Some(42));
        assert_eq!(imdb_id(Some("tt0133093".into())).as_deref(), Some("tt0133093"));
        assert_eq!(imdb_id(Some("nm0000206".into())), None);
        let id = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
        assert_eq!(mbid(Some(id.to_uppercase())).as_deref(), Some(id));
        assert_eq!(mbid(Some("nope".into())), None);
        assert_eq!(
            mbid_from_url(&format!("https://musicbrainz.org/artist/{id}"), "artist").as_deref(),
            Some(id)
        );
    }
}
