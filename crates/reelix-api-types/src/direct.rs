//! Shared direct-download settings, requests and queue entries.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct DirectSettings {
    pub enabled: bool,
    pub output_dir: String,
    pub ffmpeg: String,
    /// yt-dlp executable for YouTube listing and stream downloads; empty uses the
    /// built-in RustyPipe extractor.
    pub yt_dlp: String,
    /// Reusable librespot credentials (compatible with Zotify credentials.json).
    pub spotify_credentials: String,
}
impl Default for DirectSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            output_dir: String::new(),
            ffmpeg: "ffmpeg".into(),
            yt_dlp: "yt-dlp".into(),
            spotify_credentials: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DirectSource {
    Youtube,
    Spotify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DownloadMode {
    Audio,
    Video,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DirectRequest {
    pub url: String,
    pub source: DirectSource,
    pub mode: DownloadMode,
    /// Audio: original, mp3, m4a, opus, flac (Spotify also ogg). Video: mkv or mp4.
    pub format: String,
    /// Video resolution cap; must be absent for audio.
    pub max_height: Option<u32>,
    /// Spotify: auto, normal, high or very_high (account permitting).
    pub spotify_quality: Option<String>,
    /// YouTube playlists require explicit opt-in.
    pub playlist: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DirectDownload {
    pub id: i64,
    pub request: DirectRequest,
    pub state: String,
    /// Current file percentage; playlists may start again at zero for each item.
    pub progress: Option<f64>,
    pub message: String,
    pub output_dir: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DirectHealth {
    pub tool: String,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DirectPreview {
    pub title: String,
    pub items: Vec<String>,
    pub truncated: bool,
}
