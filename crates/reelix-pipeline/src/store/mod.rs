//! Persistence for history, blocklist, tracked downloads and pending releases
//! (tables from `migrations/sqlite/0006_pipeline.sql`).

pub mod blocklist;
pub mod history;
pub mod pending;
pub mod tracked;

use chrono::{DateTime, Utc};
use reelix_core::soulseek::SoulseekRelease;
use reelix_core::{BookFormat, Protocol};
use reelix_parser::{Language, QualityModel};
use serde::{Deserialize, Serialize};

/// A custom format reference (id + name), as stored with history/blocklist rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatRef {
    pub id: i64,
    pub name: String,
}

/// Snapshot of a release at grab (or delay) time; stored as JSON with tracked/pending rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrabbedRelease {
    pub guid: String,
    pub title: String,
    pub indexer_id: Option<i64>,
    pub indexer: String,
    pub protocol: Protocol,
    pub size: Option<u64>,
    pub publish_date: Option<DateTime<Utc>>,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub info_url: Option<String>,
    pub info_hash: Option<String>,
    pub seeders: Option<u32>,
    pub quality: QualityModel,
    #[serde(default)]
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    #[serde(default)]
    pub custom_formats: Vec<FormatRef>,
    #[serde(default)]
    pub format_score: i32,
    #[serde(default)]
    pub indexer_flags: Vec<String>,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<u32>,
    /// Soulseek: the peer's folder the slskd client enqueues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soulseek: Option<SoulseekRelease>,
}

impl GrabbedRelease {
    /// Identity used for blocklisting: the info hash, or the Soulseek `slsk:{user}/{dir}` key.
    pub fn release_key(&self) -> Option<String> {
        self.info_hash
            .clone()
            .or_else(|| self.soulseek.as_ref().map(SoulseekRelease::key))
    }
}

/// Book / album / radio episode reference of a pipeline row (columns from
/// `0010_media_shared.sql` and `0015_radio_filing.sql`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContentRef {
    pub book_id: Option<i64>,
    pub book_format: Option<BookFormat>,
    pub album_id: Option<i64>,
    #[serde(default)]
    pub radio_episode_id: Option<i64>,
}

impl ContentRef {
    pub fn book(id: i64, format: BookFormat) -> Self {
        Self {
            book_id: Some(id),
            book_format: Some(format),
            album_id: None,
            radio_episode_id: None,
        }
    }

    pub fn album(id: i64) -> Self {
        Self {
            album_id: Some(id),
            ..Self::default()
        }
    }

    pub fn radio_episode(id: i64) -> Self {
        Self {
            radio_episode_id: Some(id),
            ..Self::default()
        }
    }

    /// Neither a book, an album nor a radio episode.
    pub fn is_empty(&self) -> bool {
        self.book_id.is_none() && self.album_id.is_none() && self.radio_episode_id.is_none()
    }

    /// The book and its format (a missing format reads as ebook).
    pub fn book_ref(&self) -> Option<(i64, BookFormat)> {
        self.book_id
            .map(|id| (id, self.book_format.unwrap_or(BookFormat::Ebook)))
    }

    pub(crate) fn format_str(&self) -> Option<&'static str> {
        self.book_format.map(BookFormat::as_str)
    }

    pub(crate) fn from_columns(
        book_id: Option<i64>,
        book_format: Option<&str>,
        album_id: Option<i64>,
        radio_episode_id: Option<i64>,
    ) -> Self {
        Self {
            book_id,
            book_format: book_format.and_then(BookFormat::parse),
            album_id,
            radio_episode_id,
        }
    }
}

pub(crate) fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

pub(crate) fn protocol_str(p: Protocol) -> &'static str {
    p.as_str()
}

pub(crate) fn parse_protocol(s: &str) -> Option<Protocol> {
    Protocol::parse(s)
}
