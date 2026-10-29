//! Persistence for history, blocklist, tracked downloads and pending releases
//! (tables from `migrations/sqlite/0006_pipeline.sql`).

pub mod blocklist;
pub mod history;
pub mod pending;
pub mod tracked;

use chrono::{DateTime, Utc};
use reelix_core::Protocol;
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
}

pub(crate) fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

pub(crate) fn protocol_str(p: Protocol) -> &'static str {
    match p {
        Protocol::Torrent => "torrent",
        Protocol::Usenet => "usenet",
    }
}

pub(crate) fn parse_protocol(s: &str) -> Option<Protocol> {
    match s {
        "torrent" => Some(Protocol::Torrent),
        "usenet" => Some(Protocol::Usenet),
        _ => None,
    }
}
