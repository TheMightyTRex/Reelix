//! Helpers shared by the adapters: common schema fields, settings decoding, URLs, HTTP.

use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use reelix_core::{ContentKind, MediaKind};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::model::{ClientInfo, Priority};
use crate::{BuildContext, ClientError};

/// Validates `settings` against `schema` (filling defaults) and decodes it into `T`.
pub fn decode_settings<T: DeserializeOwned>(
    schema: &ProviderSchema,
    settings: &Value,
) -> Result<T, ClientError> {
    let obj = schema.validate(settings).map_err(ClientError::InvalidSettings)?;
    serde_json::from_value(Value::Object(obj)).map_err(|e| ClientError::InvalidSettings(e.to_string()))
}

/// Accepts a number or numeric string (forms often submit strings).
pub fn de_num<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr + TryFrom<u64>,
{
    let v = Value::deserialize(d)?;
    let n = match &v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    };
    n.and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| serde::de::Error::custom(format!("expected a non-negative number, got {v}")))
}

/// Optional float, number or numeric string; empty → `None`.
pub fn de_opt_f64<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => Some(
            s.trim()
                .parse()
                .map_err(|_| serde::de::Error::custom("expected a number"))?,
        ),
        _ => None,
    })
}

/// Priority enum from its display string.
pub fn de_priority<'de, D>(d: D) -> Result<Priority, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    Priority::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown priority `{s}`")))
}

/// Settings fields every client has (`name`, `priority`, removal flags, tags).
#[derive(Debug, Clone, Deserialize)]
pub struct CommonSettings {
    pub name: String,
    #[serde(deserialize_with = "de_num")]
    pub priority: u8,
    pub remove_completed: bool,
    pub remove_failed: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl CommonSettings {
    pub fn info(&self, implementation: &str) -> ClientInfo {
        ClientInfo {
            implementation: implementation.into(),
            name: self.name.clone(),
            priority: self.priority,
            remove_completed: self.remove_completed,
            remove_failed: self.remove_failed,
            tags: self.tags.clone(),
        }
    }
}

/// Connection fields for HTTP-based clients.
#[derive(Debug, Clone, Deserialize)]
pub struct HttpSettings {
    pub host: String,
    #[serde(deserialize_with = "de_num")]
    pub port: u16,
    pub use_ssl: bool,
    #[serde(default = "yes")]
    pub verify_ssl: bool,
    #[serde(default)]
    pub url_base: String,
}

fn yes() -> bool {
    true
}

impl HttpSettings {
    /// `{scheme}://{host}:{port}{url_base}` with url_base normalised to `/x` or empty.
    pub fn base_url(&self) -> Result<String, ClientError> {
        let host = self.host.trim();
        if host.is_empty() || host.contains(['/', '?', '#', ' ']) {
            return Err(ClientError::InvalidSettings(format!("invalid host `{host}`")));
        }
        let scheme = if self.use_ssl { "https" } else { "http" };
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]") // bare IPv6
        } else {
            host.to_owned()
        };
        let base = self.url_base.trim().trim_matches('/');
        let base = if base.is_empty() {
            String::new()
        } else {
            format!("/{base}")
        };
        let url = format!("{scheme}://{host}:{}{base}", self.port);
        url::Url::parse(&url).map_err(|e| ClientError::InvalidSettings(e.to_string()))?;
        Ok(url)
    }

    /// The context client, or a dedicated one that skips certificate checks.
    pub fn client(&self, ctx: &BuildContext) -> Result<reqwest::Client, ClientError> {
        if self.use_ssl && !self.verify_ssl {
            return reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|e| ClientError::InvalidSettings(e.to_string()));
        }
        Ok(ctx.http.clone())
    }
}

/// Picks the category: explicit override, else the movie/TV setting.
pub fn category_for(
    opts_category: Option<&str>,
    kind: Option<MediaKind>,
    movie: &str,
    tv: &str,
) -> Option<String> {
    let c = match (opts_category, kind) {
        (Some(c), _) => c,
        (None, Some(MediaKind::Series)) => tv,
        (None, _) => movie,
    };
    let c = c.trim();
    (!c.is_empty()).then(|| c.to_owned())
}

/// The configured categories (movie + TV, deduplicated, non-empty).
pub fn configured_categories(movie: &str, tv: &str) -> Vec<String> {
    let mut v: Vec<String> = [movie, tv]
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    v.dedup();
    v
}

/// The configured categories (any number of settings, deduplicated, non-empty, in order).
pub fn configured_categories_all(categories: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for c in categories {
        let c = c.trim();
        if !c.is_empty() && !v.iter().any(|e| e == c) {
            v.push(c.to_owned());
        }
    }
    v
}

/// Category for a content kind: books (ebook and audiobook) use `book`, music uses `music`;
/// either falls back to the movie category when empty. `None` when the result is empty.
pub fn category_for_kind(
    kind: ContentKind,
    movie: &str,
    tv: &str,
    book: &str,
    music: &str,
) -> Option<String> {
    fn pick<'a>(c: &'a str, movie: &'a str) -> &'a str {
        if c.trim().is_empty() { movie } else { c }
    }
    let c = match kind {
        ContentKind::Movie => movie,
        ContentKind::Series => tv,
        ContentKind::Ebook | ContentKind::Audiobook => pick(book, movie),
        ContentKind::Music => pick(music, movie),
    };
    let c = c.trim();
    (!c.is_empty()).then(|| c.to_owned())
}

// ---- schema field builders ----

pub fn f_name(default: &str) -> FieldSchema {
    FieldSchema::new("name", "Name", FieldKind::Text)
        .required()
        .default_value(default)
        .help("Display name. Multiple instances are allowed.")
}

pub fn f_host() -> FieldSchema {
    FieldSchema::new("host", "Host", FieldKind::Text)
        .required()
        .default_value("localhost")
        .help("Hostname, IP or container name")
}

pub fn f_port(default: u16) -> FieldSchema {
    FieldSchema::new("port", "Port", FieldKind::Number)
        .required()
        .default_value(default)
}

pub fn f_ssl() -> [FieldSchema; 2] {
    [
        FieldSchema::new("use_ssl", "Use SSL", FieldKind::Bool).default_value(false),
        FieldSchema::new("verify_ssl", "Verify SSL certificate", FieldKind::Bool)
            .default_value(true)
            .help("Shown when SSL is on. Turn off for self-signed certificates.")
            .advanced(),
    ]
}

pub fn f_url_base(default: &str) -> FieldSchema {
    FieldSchema::new("url_base", "URL base", FieldKind::Text)
        .default_value(default)
        .help("Path prefix when the client runs behind a reverse proxy")
        .advanced()
}

pub fn f_username(default: Option<&str>) -> FieldSchema {
    let f = FieldSchema::new("username", "Username", FieldKind::Text);
    match default {
        Some(d) => f.default_value(d),
        None => f,
    }
}

pub fn f_password(default: Option<&str>) -> FieldSchema {
    let f = FieldSchema::new("password", "Password", FieldKind::Secret);
    match default {
        Some(d) => f.default_value(d),
        None => f,
    }
}

pub fn f_categories(movie: &str, tv: &str, label: &str) -> [FieldSchema; 2] {
    [
        FieldSchema::new("movie_category", &format!("Movie {label}"), FieldKind::Text)
            .default_value(movie)
            .help("Only downloads in this category are tracked by Reelix"),
        FieldSchema::new("tv_category", &format!("TV {label}"), FieldKind::Text).default_value(tv),
    ]
}

/// `book_category`, `music_category` (advanced, empty = use the movie category).
pub fn f_extra_categories(label: &str) -> [FieldSchema; 2] {
    [
        FieldSchema::new("book_category", &format!("Book {label}"), FieldKind::Text)
            .default_value("")
            .help("Ebooks and audiobooks. Empty = use the movie category")
            .advanced(),
        FieldSchema::new("music_category", &format!("Music {label}"), FieldKind::Text)
            .default_value("")
            .help("Empty = use the movie category")
            .advanced(),
    ]
}

pub fn priority_options() -> Vec<SelectOption> {
    Priority::ALL
        .iter()
        .map(|p| SelectOption {
            value: p.as_str().into(),
            label: p.as_str().into(),
        })
        .collect()
}

pub fn f_recent_older_priority() -> [FieldSchema; 2] {
    [
        FieldSchema::new(
            "recent_priority",
            "Recent priority",
            FieldKind::Select {
                options: priority_options(),
            },
        )
        .default_value("Default")
        .help("Priority for releases aired or released in the last 14 days"),
        FieldSchema::new(
            "older_priority",
            "Older priority",
            FieldKind::Select {
                options: priority_options(),
            },
        )
        .default_value("Default")
        .help("Priority for everything else"),
    ]
}

/// `priority`, `remove_completed`, `remove_failed`, `tags`.
pub fn f_common_tail() -> [FieldSchema; 4] {
    [
        FieldSchema::new("priority", "Client priority", FieldKind::Number)
            .default_value(1)
            .help("Used when several clients of the same protocol match (lower wins)")
            .advanced(),
        FieldSchema::new("remove_completed", "Remove completed", FieldKind::Bool)
            .default_value(true)
            .help("Remove from the client after import (torrents: once seed goals are met)"),
        FieldSchema::new("remove_failed", "Remove failed", FieldKind::Bool)
            .default_value(true)
            .help("Remove failed downloads from the client after failure handling"),
        FieldSchema::new("tags", "Tags", FieldKind::List)
            .help("Limits this client to media with one of these tags"),
    ]
}

/// Makes a string safe as a single file name component.
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.chars().take(200).collect()
    }
}

/// Response body text with a bounded length, for error messages.
pub fn snippet(s: &str) -> String {
    let s = s.trim();
    if s.len() > 300 {
        let mut end = 300;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    } else {
        s.to_owned()
    }
}

/// Largest `.torrent` / `.nzb` fetched from a URL.
pub const MAX_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Reads a response body, failing once it exceeds `limit` bytes.
pub async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, ClientError> {
    let too_big =
        || ClientError::InvalidPayload(format!("download larger than {} MB", limit / (1024 * 1024)));
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

/// Fails with `Http` unless the status is 2xx; returns the body text.
pub async fn ok_text(resp: reqwest::Response) -> Result<String, ClientError> {
    let status = resp.status();
    let body = resp.text().await?;
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(ClientError::Auth(format!("HTTP {status}")));
    }
    if !status.is_success() {
        return Err(ClientError::Http {
            status: status.as_u16(),
            body: snippet(&body),
        });
    }
    Ok(body)
}

/// Compares dotted versions numerically (`"3.7.2RC1" >= "3.0"`). Non-numeric parts count as 0.
pub fn version_at_least(found: &str, minimum: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches(['v', 'V'])
            .split(['.', '-', ' '])
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let (a, b) = (parts(found), parts(minimum));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    true
}

/// Checks that `bytes` is a non-empty, well-formed NZB document with at least one `<file>`.
pub fn validate_nzb(bytes: &[u8]) -> Result<(), ClientError> {
    use quick_xml::events::Event;
    let bad = |m: String| ClientError::InvalidPayload(format!("invalid NZB: {m}"));
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(bad("empty file".into()));
    }
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut buf = Vec::new();
    let (mut root_seen, mut files, mut depth) = (false, 0usize, 0i64);
    loop {
        let ev = reader.read_event_into(&mut buf);
        match &ev {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(_)) => depth -= 1,
            _ => {}
        }
        match ev {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.local_name();
                if !root_seen {
                    if !name.as_ref().eq_ignore_ascii_case(b"nzb") {
                        return Err(bad("root element is not <nzb>".into()));
                    }
                    root_seen = true;
                } else if name.as_ref().eq_ignore_ascii_case(b"file") {
                    files += 1;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(bad(e.to_string())),
        }
        buf.clear();
    }
    if !root_seen {
        return Err(bad("no <nzb> element".into()));
    }
    if depth != 0 {
        return Err(bad("truncated document".into()));
    }
    if files == 0 {
        return Err(bad("contains no files".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nzb_validation() {
        let ok = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="a" date="1" subject="s"><groups><group>a.b</group></groups><segments><segment bytes="1" number="1">x@y</segment></segments></file></nzb>"#;
        assert!(validate_nzb(ok).is_ok());
        assert!(validate_nzb(b"   ").is_err());
        assert!(validate_nzb(b"<html><body>Not found</body></html>").is_err());
        assert!(validate_nzb(b"<nzb></nzb>").is_err());
        assert!(validate_nzb(b"<nzb><file>").is_err());
    }

    #[test]
    fn base_url_normalises() {
        let s = HttpSettings {
            host: "sab".into(),
            port: 8080,
            use_ssl: true,
            verify_ssl: true,
            url_base: "sabnzbd/".into(),
        };
        assert_eq!(s.base_url().unwrap(), "https://sab:8080/sabnzbd");
        let s = HttpSettings {
            host: "::1".into(),
            url_base: String::new(),
            use_ssl: false,
            ..s
        };
        assert_eq!(s.base_url().unwrap(), "http://[::1]:8080");
        let bad = HttpSettings {
            host: "a/b".into(),
            ..s
        };
        assert!(bad.base_url().is_err());
    }

    #[test]
    fn content_categories() {
        assert_eq!(
            category_for_kind(ContentKind::Ebook, "movies", "tv", "books", "").as_deref(),
            Some("books")
        );
        assert_eq!(
            category_for_kind(ContentKind::Audiobook, "movies", "tv", "books", "").as_deref(),
            Some("books")
        );
        assert_eq!(
            category_for_kind(ContentKind::Music, "movies", "tv", "books", " ").as_deref(),
            Some("movies")
        );
        assert_eq!(
            category_for_kind(ContentKind::Series, "movies", "tv", "books", "music").as_deref(),
            Some("tv")
        );
        assert_eq!(category_for_kind(ContentKind::Music, "", "tv", "", ""), None);
        assert_eq!(
            configured_categories_all(&["movies", " tv ", "", "movies", "books"]),
            ["movies", "tv", "books"]
        );
    }

    #[test]
    fn versions() {
        assert!(version_at_least("3.7.2", "3.0.0"));
        assert!(version_at_least("4.0.0RC1", "3.0"));
        assert!(!version_at_least("2.3.9", "3.0.0"));
        assert!(version_at_least("v4.6.2", "4.1"));
        assert!(!version_at_least("15.0", "16.0"));
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_file_name("../a/b:c"), "_a_b_c");
        assert_eq!(sanitize_file_name(".."), "download");
    }
}
