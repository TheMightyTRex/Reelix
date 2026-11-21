//! Subtitles (`/settings/subtitles`, `/subtitles/...`): OpenSubtitles and Stremio subtitle
//! add-ons, saved beside the video as Jellyfin reads them. Off until activated.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Shown instead of a stored secret; sending it back keeps the stored value.
pub const SUBTITLE_SECRET_MASK: &str = "********";

/// Subtitle settings (`GET`/`PUT /api/v1/settings/subtitles`, admin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubtitleSettings {
    /// Fetch subtitles at all. Off by default; nothing is asked of any source while off.
    #[serde(default)]
    pub enabled: bool,
    /// Languages in order of preference, each with the variants to fetch.
    #[serde(default)]
    pub languages: Vec<SubtitleLanguage>,
    #[serde(default)]
    pub opensubtitles: OpenSubtitlesSettings,
    /// Stremio subtitle add-on addresses (the add-on's base, with or without `/manifest.json`).
    #[serde(default)]
    pub stremio_addons: Vec<String>,
    /// Search as soon as a film or episode is imported.
    #[serde(default = "yes")]
    pub fetch_after_import: bool,
    /// Search on a schedule for subtitles still missing, and for better matches.
    #[serde(default = "yes")]
    pub search_missing: bool,
    /// Replace a subtitle with one made for the file (a file-hash match) when one appears later.
    #[serde(default = "yes")]
    pub upgrade: bool,
}

fn yes() -> bool {
    true
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            languages: Vec::new(),
            opensubtitles: OpenSubtitlesSettings::default(),
            stremio_addons: Vec::new(),
            fetch_after_import: true,
            search_missing: true,
            upgrade: true,
        }
    }
}

/// One language and the variants wanted for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubtitleLanguage {
    /// Two-letter ISO 639-1 code (`en`), or with a region where the sources tell them apart
    /// (`pt-BR`, `zh-TW`).
    pub code: String,
    /// Normal subtitles (`Name (Year).en.srt`).
    #[serde(default = "yes")]
    pub normal: bool,
    /// For people who are deaf or hard of hearing (SDH: `Name (Year).en.sdh.srt`).
    #[serde(default)]
    pub sdh: bool,
    /// Forced: only the foreign-language parts (`Name (Year).en.forced.srt`).
    #[serde(default)]
    pub forced: bool,
}

/// OpenSubtitles.com REST API: the person's own API key and account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OpenSubtitlesSettings {
    #[serde(default)]
    pub enabled: bool,
    /// API key from opensubtitles.com (Profile, API consumers). Shown masked.
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub username: String,
    /// Shown masked.
    #[serde(default)]
    pub password: String,
}

/// Which file a subtitle belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum SubtitleMediaKind {
    /// A film's file (`movie_files`).
    Movie,
    /// An episode file (`episode_files`).
    Episode,
}

impl SubtitleMediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Episode => "episode",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "movie" => Some(Self::Movie),
            "episode" => Some(Self::Episode),
            _ => None,
        }
    }
}

/// A subtitle Reelix fetched (`GET /api/v1/subtitles/{kind}/{file_id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubtitleFileDto {
    pub id: i64,
    pub media_kind: SubtitleMediaKind,
    pub file_id: i64,
    pub language: String,
    /// `normal`, `sdh` or `forced`.
    pub variant: String,
    /// Where it is saved, beside the video.
    pub path: String,
    /// `opensubtitles` or `stremio`.
    pub source: String,
    /// `hash` (made for this file), `id` (same film or episode) or `release` (release name only).
    pub match_level: String,
    pub release_name: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_off_and_fill_missing_fields() {
        let s: SubtitleSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, SubtitleSettings::default());
        assert!(!s.enabled);
        assert!(s.fetch_after_import && s.search_missing && s.upgrade);
        let l: SubtitleLanguage = serde_json::from_str(r#"{"code":"en"}"#).unwrap();
        assert!(l.normal && !l.sdh && !l.forced);
    }
}
