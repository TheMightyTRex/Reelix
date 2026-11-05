//! slskd (Soulseek) adapter: network-wide searches through slskd's REST API (`/api/v0`,
//! `X-API-Key` auth), grouped into one release per peer folder.
//!
//! A search is `POST /searches`, polled (`GET /searches/{id}`) until slskd marks it complete
//! (or our deadline passes and it is cancelled with `PUT`), read with `GET /searches/{id}/responses`
//! (slskd only fills them in once the search has finished) and deleted again. Files of one
//! (username, directory) become one [`ReleaseInfo`] whose [`SoulseekRelease`] the slskd
//! download client enqueues. Only book, music and radio queries are sent; movie/TV searches
//! return nothing. Based on slskd 0.26.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::soulseek::{SoulseekFile, SoulseekRelease, last_component, split_remote};
use reelix_core::{BookFormat, Protocol};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::time::Instant;
use url::Url;

use crate::http::{build_client, read_limited, status_error};
use crate::model::{
    Category, IndexerCapabilities, ReleaseInfo, SearchKind, SearchMode, SearchParam, SearchQuery,
};
use crate::util::SettingsExt;
use crate::{BuildContext, Indexer, IndexerError, IndexerFlag, categories};

pub const IMPLEMENTATION: &str = "slskd";

/// Extensions searched by default (music, audiobooks, ebooks).
pub const DEFAULT_FILE_TYPES: [&str; 12] = [
    "flac", "mp3", "m4a", "aac", "ogg", "opus", "wav", "m4b", "epub", "mobi", "azw3", "pdf",
];

const AUDIO: [&str; 9] = ["flac", "alac", "mp3", "m4a", "aac", "ogg", "opus", "wav", "wma"];
const EBOOK: [&str; 6] = ["epub", "mobi", "azw3", "azw", "pdf", "cbz"];

/// Folder names that say nothing about the content (skipped when building titles).
const GENERIC_FOLDERS: [&str; 26] = [
    "music",
    "my music",
    "shared",
    "share",
    "shares",
    "downloads",
    "download",
    "complete",
    "completed",
    "incoming",
    "albums",
    "album",
    "audio",
    "mp3",
    "flac",
    "lossless",
    "books",
    "ebooks",
    "ebook",
    "audiobooks",
    "audiobook",
    "media",
    "soulseek",
    "slsk",
    "library",
    "uploads",
];

/// Settings schema for `slskd`.
pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "slskd (Soulseek)".into(),
        description: Some(
            "Soulseek searches through a self-hosted slskd daemon (music, books, radio)".into(),
        ),
        info_url: Some("https://github.com/slskd/slskd".into()),
        fields: vec![
            FieldSchema::new("base_url", "URL", FieldKind::Url)
                .required()
                .placeholder("http://localhost:5030")
                .help("slskd web address, usually on the LAN (private addresses are always allowed)"),
            FieldSchema::new("api_key", "API key", FieldKind::Secret)
                .required()
                .help("An slskd API key (web.authentication.api_keys or SLSKD_API_KEY)"),
            FieldSchema::new(
                "search_timeout_secs",
                "Search timeout (seconds)",
                FieldKind::Number,
            )
            .default_value(15)
            .help("How long slskd waits for further peer responses (5–20)"),
            FieldSchema::new("minimum_files", "Minimum files", FieldKind::Number)
                .default_value(1)
                .help("Folders with fewer matching files are ignored"),
            FieldSchema::new("max_results", "Maximum results", FieldKind::Number)
                .default_value(50)
                .help("Folders returned per search (best peers first)"),
            FieldSchema::new("file_types", "File types", FieldKind::List)
                .default_value(DEFAULT_FILE_TYPES.to_vec())
                .help("Extensions kept from responses; empty keeps every file")
                .advanced(),
            FieldSchema::new("require_free_slot", "Require a free upload slot", FieldKind::Bool)
                .default_value(false)
                .help("Skip peers that would queue the download"),
            FieldSchema::new("max_queue_length", "Maximum peer queue length", FieldKind::Number)
                .default_value(50)
                .help("Skip peers with more queued uploads; 0 for no limit")
                .advanced(),
            FieldSchema::new(
                "min_upload_speed_kbs",
                "Minimum upload speed (KB/s)",
                FieldKind::Number,
            )
            .default_value(0)
            .advanced(),
        ],
    }
}

/// An slskd instance used as an indexer.
pub struct SlskdIndexer {
    indexer_id: i64,
    name: String,
    api: Url,
    api_key: String,
    search_timeout: Duration,
    minimum_files: usize,
    max_results: usize,
    file_types: Vec<String>,
    require_free_slot: bool,
    max_queue_length: u32,
    min_upload_speed: u64,
    poll_interval: Duration,
    client: reqwest::Client,
}

fn num(s: &Map<String, Value>, k: &str, default: u64) -> u64 {
    s.str_opt(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn flag(s: &Map<String, Value>, k: &str) -> bool {
    s.get(k)
        .is_some_and(|v| v.as_bool().unwrap_or(v.as_str() == Some("true")))
}

impl SlskdIndexer {
    /// Validates `settings` against [`schema`] and builds the adapter.
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, IndexerError> {
        let s = schema().validate(settings).map_err(IndexerError::Config)?;
        let base = s.str_req("base_url")?;
        let mut api = Url::parse(&base).map_err(|e| IndexerError::Config(format!("URL: {e}")))?;
        if !matches!(api.scheme(), "http" | "https") {
            return Err(IndexerError::Config("URL must be http(s)".into()));
        }
        let joined = format!("{}/api/v0/", api.path().trim_end_matches('/'));
        api.set_path(&joined);
        let file_types = match s.get("file_types") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(|t| t.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect(),
            Some(Value::String(t)) => t
                .split(',')
                .map(|t| t.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        // slskd is the user's own daemon, normally on the LAN: like a download client it may
        // be a private address regardless of the indexer SSRF setting.
        let mut opts = ctx.http_options();
        opts.guard.allow_private = true;
        Ok(Self {
            indexer_id: ctx.indexer_id,
            name: ctx.name.clone(),
            api,
            api_key: s.str_req("api_key")?,
            search_timeout: Duration::from_secs(num(&s, "search_timeout_secs", 15).clamp(5, 20)),
            minimum_files: num(&s, "minimum_files", 1).max(1) as usize,
            max_results: num(&s, "max_results", 50).max(1) as usize,
            file_types,
            require_free_slot: flag(&s, "require_free_slot"),
            max_queue_length: num(&s, "max_queue_length", 50).min(u64::from(u32::MAX)) as u32,
            min_upload_speed: num(&s, "min_upload_speed_kbs", 0) * 1024,
            poll_interval: Duration::from_secs(1),
            client: build_client(&opts, false)?,
        })
    }

    /// Polling cadence and slskd search timeout, unclamped (tests shorten both).
    pub fn with_timing(mut self, poll_interval: Duration, search_timeout: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.search_timeout = search_timeout;
        self
    }

    fn url(&self, path: &str) -> Url {
        // `path` segments are ours (ids are GUIDs), so a plain join is safe.
        self.api.join(path).unwrap_or_else(|_| self.api.clone())
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Vec<u8>, IndexerError> {
        let resp = req.header("X-API-Key", &self.api_key).send().await?;
        if let Some(e) = status_error(&resp) {
            return Err(e);
        }
        read_limited(resp, crate::http::MAX_PAGE_BYTES).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, IndexerError> {
        let bytes = self.send(self.client.get(self.url(path))).await?;
        serde_json::from_slice(&bytes).map_err(|e| IndexerError::Parse(format!("slskd {path}: {e}")))
    }

    /// The query text, or `None` when the query is not for music, books or radio.
    pub fn search_text(q: &SearchQuery) -> Option<String> {
        let raw = match q.kind {
            SearchKind::Book | SearchKind::Music => q.keywords(),
            // Radio searches are basic queries in the radio (audio) categories; a basic search
            // without audio/book categories (e.g. an anime absolute-number fallback) is skipped.
            SearchKind::Basic if q.categories.iter().any(|c| matches!(c / 1000, 3 | 7)) => q.keywords(),
            _ => return None,
        };
        // Soulseek matches whole words of the path; punctuation only gets in the way.
        let cleaned: String = raw
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect();
        let words: Vec<&str> = cleaned.split_whitespace().collect();
        (!words.is_empty()).then(|| words.join(" "))
    }

    async fn run_search(&self, text: &str) -> Result<Vec<SearchResponse>, IndexerError> {
        let id = new_id();
        let body = json!({
            "id": id,
            "searchText": text,
            "searchTimeout": self.search_timeout.as_millis() as u64,
            "responseLimit": 100,
            "fileLimit": 10000,
            "filterResponses": true,
            "minimumResponseFileCount": self.minimum_files,
            "minimumPeerUploadSpeed": self.min_upload_speed,
            "maximumPeerQueueLength": if self.max_queue_length == 0 { 1_000_000 } else { self.max_queue_length },
        });
        // slskd allows one search POST at a time (429 otherwise): retry briefly.
        let mut attempt = 0;
        loop {
            match self
                .send(self.client.post(self.url("searches")).json(&body))
                .await
            {
                Ok(_) => break,
                Err(IndexerError::RequestLimit { .. }) if attempt < 5 => {
                    attempt += 1;
                    tokio::time::sleep(self.poll_interval / 2).await;
                }
                Err(e) => return Err(e),
            }
        }
        let result = self.collect(&id).await;
        if let Err(e) = self
            .send(self.client.delete(self.url(&format!("searches/{id}"))))
            .await
        {
            tracing::debug!(indexer = %self.name, error = %e, "could not delete slskd search");
        }
        result
    }

    /// Waits for the search to complete (cancelling it at our deadline), then reads responses.
    async fn collect(&self, id: &str) -> Result<Vec<SearchResponse>, IndexerError> {
        let path = format!("searches/{id}");
        // slskd's timeout counts from the last response, so a busy search can run longer:
        // cancel it a few polls past the timeout and give it a few more to finalise.
        let deadline = Instant::now() + self.search_timeout + self.poll_interval * 4;
        let mut cancelled = false;
        loop {
            let s: SearchState = self.get_json(&path).await?;
            if s.is_complete {
                break;
            }
            let now = Instant::now();
            if !cancelled && now >= deadline {
                self.send(self.client.put(self.url(&path))).await?;
                cancelled = true;
            } else if cancelled && now >= deadline + self.poll_interval * 3 {
                // Still not finalised; responses are only filled in once it is.
                tracing::debug!(indexer = %self.name, "slskd search did not finish in time");
                return Ok(Vec::new());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        self.get_json(&format!("{path}/responses")).await
    }

    /// Groups responses into releases (one per peer folder), applying the filters.
    pub fn releases(&self, responses: Vec<SearchResponse>, q: &SearchQuery) -> Vec<ReleaseInfo> {
        let now = Utc::now();
        let mut out = Vec::new();
        for r in responses {
            if (self.require_free_slot && !r.has_free_upload_slot)
                || (self.max_queue_length > 0 && r.queue_length > u64::from(self.max_queue_length))
                || r.upload_speed < self.min_upload_speed
            {
                continue;
            }
            let mut folders: BTreeMap<String, Vec<SoulseekFile>> = BTreeMap::new();
            for f in r.files {
                let file = f.into_file();
                let ext = file.extension();
                if !self.file_types.is_empty() && !self.file_types.contains(&ext) {
                    continue;
                }
                let dir = split_remote(&file.filename).0.to_owned();
                folders.entry(dir).or_default().push(file);
            }
            for (directory, files) in folders {
                if files.len() < self.minimum_files || directory.is_empty() {
                    continue;
                }
                let rel = SoulseekRelease {
                    username: r.username.clone(),
                    directory,
                    files,
                    has_free_upload_slot: r.has_free_upload_slot,
                    queue_length: r.queue_length.min(u64::from(u32::MAX)) as u32,
                    upload_speed: r.upload_speed,
                };
                out.push(self.release(rel, q, now));
            }
        }
        out.sort_by(|a, b| {
            let (sa, sb) = (a.soulseek.as_ref(), b.soulseek.as_ref());
            let k = |s: Option<&SoulseekRelease>| {
                s.map_or((false, u32::MAX, 0), |s| {
                    (s.has_free_upload_slot, s.queue_length, s.upload_speed)
                })
            };
            let (ka, kb) = (k(sa), k(sb));
            kb.0.cmp(&ka.0).then(ka.1.cmp(&kb.1)).then(kb.2.cmp(&ka.2))
        });
        out.truncate(self.max_results);
        out
    }

    fn release(&self, rel: SoulseekRelease, q: &SearchQuery, now: chrono::DateTime<Utc>) -> ReleaseInfo {
        let mut i = ReleaseInfo::new(Protocol::Soulseek, self.indexer_id, &self.name);
        let tag = quality_tag(&rel.files);
        let base = folder_title(&rel.directory);
        i.title = match &tag {
            Some(t) if !base.to_ascii_lowercase().contains(&t.to_ascii_lowercase()) => {
                format!("{base} [{t}]")
            }
            _ => base,
        };
        i.guid = rel.key();
        i.size = Some(rel.total_size());
        i.files = Some(rel.files.len() as u32);
        // No publish date on Soulseek: the search time stands in, so delay profiles count from
        // when Reelix first saw the folder (pending releases keep that first time).
        i.publish_date = Some(now);
        i.categories = vec![category_of(&rel.files, q)];
        if rel.has_free_upload_slot {
            i.indexer_flags.push(IndexerFlag::FreeUploadSlot);
        }
        i.soulseek = Some(rel);
        i.finish();
        i
    }
}

/// Random v4-style GUID for the search id (slskd requires a GUID).
fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(Utc::now().timestamp_nanos_opt().unwrap_or_default() as u128);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn is_generic(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.is_empty()
        || n.starts_with("@@")
        || (n.len() == 2 && n.ends_with(':'))
        || GENERIC_FOLDERS.contains(&n.as_str())
}

fn is_disc_folder(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase().replace([' ', '_', '-', '.'], "");
    ["cd", "disc", "disk"].iter().any(|p| {
        n.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Release title from a remote folder: the folder name, prefixed with its parent
/// (`Artist - 1997 - Album`) when the folder name alone doesn't carry the artist/author.
/// Disc folders (`CD1`) are titled after the album folder above them.
pub fn folder_title(directory: &str) -> String {
    let parts: Vec<&str> = directory
        .split(['\\', '/'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut idx = parts.len().saturating_sub(1);
    if parts.len() > 1 && is_disc_folder(parts[idx]) {
        idx -= 1;
    }
    let Some(leaf) = parts.get(idx).copied() else {
        return last_component(directory).to_owned();
    };
    let self_contained = leaf.split_once(" - ").is_some_and(|(first, _)| {
        let f = first.trim();
        !(f.len() == 4 && f.chars().all(|c| c.is_ascii_digit()))
    });
    match idx.checked_sub(1).map(|p| parts[p]) {
        Some(parent) if !self_contained && !is_generic(parent) => format!("{parent} - {leaf}"),
        _ => leaf.to_owned(),
    }
}

/// Format tag the release parsers understand (`FLAC`, `FLAC 24-96`, `MP3 320`, `MP3 V0`,
/// `AAC 256`, `OPUS`, `M4B`, `EPUB`, …) from the folder's most common file type.
pub fn quality_tag(files: &[SoulseekFile]) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in files {
        *counts.entry(f.extension()).or_default() += 1;
    }
    let (ext, _) = counts
        .iter()
        .filter(|(e, _)| !e.is_empty())
        .max_by_key(|(_, n)| **n)?;
    let of: Vec<&SoulseekFile> = files.iter().filter(|f| &f.extension() == ext).collect();
    let min_bitrate = of.iter().filter_map(|f| f.bit_rate).min();
    let vbr = of.iter().any(|f| f.is_variable_bit_rate == Some(true));
    let avg_bitrate = {
        let v: Vec<u32> = of.iter().filter_map(|f| f.bit_rate).collect();
        (!v.is_empty()).then(|| v.iter().sum::<u32>() / v.len() as u32)
    };
    Some(match ext.as_str() {
        "flac" => {
            let depth = of.iter().filter_map(|f| f.bit_depth).max();
            let rate = of.iter().filter_map(|f| f.sample_rate).max();
            match (depth, rate) {
                (Some(d), Some(r)) if d > 16 => format!("FLAC {d}-{}", r / 1000),
                (Some(d), None) if d > 16 => format!("FLAC {d}bit"),
                _ => "FLAC".into(),
            }
        }
        "mp3" => match (vbr, avg_bitrate, min_bitrate) {
            (true, Some(b), _) if b >= 220 => "MP3 V0".into(),
            (true, Some(_), _) => "MP3 V2".into(),
            (false, _, Some(b)) => format!("MP3 {}", standard_bitrate(b)),
            _ => "MP3".into(),
        },
        "m4a" | "aac" => match min_bitrate.filter(|_| !vbr) {
            Some(b) => format!("AAC {}", standard_bitrate(b)),
            None => "AAC".into(),
        },
        "ogg" | "oga" => "OGG Vorbis".into(),
        other => other.to_ascii_uppercase(),
    })
}

/// Snaps a peer-reported bitrate to the nearest common CBR value at or below it.
fn standard_bitrate(b: u32) -> u32 {
    [320, 256, 224, 192, 160, 128, 96, 64]
        .into_iter()
        .find(|s| b + 2 >= *s)
        .unwrap_or(b)
}

/// Newznab category of a folder: ebook/audiobook for book searches, lossless/MP3 audio otherwise.
fn category_of(files: &[SoulseekFile], q: &SearchQuery) -> u32 {
    let has = |set: &[&str]| files.iter().any(|f| set.contains(&f.extension().as_str()));
    if q.kind == SearchKind::Book {
        return match q.book_format {
            Some(BookFormat::Audiobook) => categories::AUDIO_AUDIOBOOK,
            Some(BookFormat::Ebook) => categories::BOOKS_EBOOK,
            None if has(&EBOOK) => categories::BOOKS_EBOOK,
            None => categories::AUDIO_AUDIOBOOK,
        };
    }
    if has(&EBOOK) && !has(&AUDIO) {
        return categories::BOOKS_EBOOK;
    }
    if files.iter().any(|f| f.extension() == "m4b") {
        return categories::AUDIO_AUDIOBOOK;
    }
    if q.kind == SearchKind::Basic {
        return categories::AUDIO_OTHER;
    }
    if has(&["flac", "alac", "wav"]) {
        categories::AUDIO_LOSSLESS
    } else {
        categories::AUDIO_MP3
    }
}

/// `GET /searches/{id}` (only the fields we need).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchState {
    #[serde(default)]
    is_complete: bool,
}

/// One peer's response (`GET /searches/{id}/responses`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub username: String,
    #[serde(default)]
    pub files: Vec<ResponseFile>,
    #[serde(default)]
    pub has_free_upload_slot: bool,
    #[serde(default)]
    pub queue_length: u64,
    /// Bytes/s.
    #[serde(default)]
    pub upload_speed: u64,
}

/// A file of a response. Missing attributes are omitted by slskd, not `null`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseFile {
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    pub bit_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub length: Option<u32>,
    pub is_variable_bit_rate: Option<bool>,
}

impl ResponseFile {
    fn into_file(self) -> SoulseekFile {
        SoulseekFile {
            filename: self.filename,
            size: self.size,
            bit_rate: self.bit_rate,
            bit_depth: self.bit_depth,
            sample_rate: self.sample_rate,
            length: self.length,
            is_variable_bit_rate: self.is_variable_bit_rate,
        }
    }
}

#[async_trait]
impl Indexer for SlskdIndexer {
    fn protocol(&self) -> Protocol {
        Protocol::Soulseek
    }

    async fn capabilities(&self) -> Result<IndexerCapabilities, IndexerError> {
        Ok(IndexerCapabilities {
            search: SearchMode::new(&[SearchParam::Q]),
            tv_search: SearchMode::default(),
            movie_search: SearchMode::default(),
            book_search: SearchMode::new(&[SearchParam::Q, SearchParam::Author, SearchParam::Title]),
            audio_search: SearchMode::new(&[SearchParam::Q, SearchParam::Artist, SearchParam::Album]),
            categories: vec![
                Category::new(categories::AUDIO, "Audio"),
                Category::new(categories::BOOKS, "Books"),
            ],
            limit_default: Some(self.max_results as u32),
            limit_max: Some(self.max_results as u32),
        })
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let Some(text) = Self::search_text(query) else {
            return Ok(Vec::new());
        };
        let responses = self.run_search(&text).await?;
        Ok(self.releases(responses, query))
    }

    /// Soulseek has no feed of new uploads.
    async fn rss(&self, _categories: &[u32]) -> Result<Vec<ReleaseInfo>, IndexerError> {
        Ok(Vec::new())
    }

    /// `GET /session` checks the key (200 / 401); `GET /application` whether slskd is logged in.
    async fn test(&self) -> Result<(), IndexerError> {
        self.send(self.client.get(self.url("session"))).await?;
        let app: Value = self.get_json("application").await?;
        if app.pointer("/server/isLoggedIn").and_then(Value::as_bool) == Some(false) {
            return Err(IndexerError::Api {
                code: 503,
                description: "slskd is not logged in to the Soulseek network".into(),
            });
        }
        Ok(())
    }

    async fn download(&self, _url: &str) -> Result<Vec<u8>, IndexerError> {
        Err(IndexerError::Unsupported(
            "Soulseek releases are enqueued by the slskd download client".into(),
        ))
    }
}
