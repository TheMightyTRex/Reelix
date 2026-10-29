//! Metadata and cover writing into imported files (only files Reelix owns: never into a
//! hardlink that is still seeding).
//!
//! - [`epub`]: OPF metadata (title, creators, ISBN, `calibre:series`, cover) in EPUB files.
//! - [`m4b`]: audio tags via lofty (`©nam`, `©ART`, `©alb`, `©wrt` narrator, cover) for
//!   M4B/M4A/MP3/FLAC/OGG/Opus.

pub mod epub;
pub mod m4b;

use std::path::Path;

/// What gets written into a file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookTags {
    pub title: String,
    pub authors: Vec<String>,
    pub narrator: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<String>,
    pub isbn: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub description: Option<String>,
    /// JPEG or PNG bytes.
    pub cover: Option<Vec<u8>>,
    /// 1-based part number and part count (multi-file audiobooks).
    pub part: Option<(u32, u32)>,
}

/// Writes `tags` into `path` according to its extension. Formats without a writer are
/// skipped (`Ok(false)`).
pub fn write(path: &Path, tags: &BookTags) -> Result<bool, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "epub" => epub::write(path, tags).map(|()| true),
        "m4b" | "m4a" | "mp3" | "flac" | "ogg" | "opus" => m4b::write(path, tags).map(|()| true),
        _ => Ok(false),
    }
}

/// `image/jpeg` / `image/png` from magic bytes.
pub(crate) fn image_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else {
        None
    }
}
