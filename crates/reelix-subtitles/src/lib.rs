//! Subtitles for films and episodes (fork issue #62, RLX-09).
//!
//! Off until activated in the subtitle settings. Sources: OpenSubtitles.com (the person's own
//! API key and account) and Stremio subtitle add-ons. A subtitle made for the file (the
//! OpenSubtitles file hash matches) comes first, then one for the same film or episode (IMDb or
//! TMDb id, season and episode), then one whose release name matches. Each language from the
//! settings can have a normal, an SDH and a forced subtitle, saved beside the video as Jellyfin
//! reads them (`Name (Year).en.srt`, `.en.sdh.srt`, `.en.forced.srt`). Searches run after an
//! import and on a schedule; a hash match found later replaces a weaker match; a subtitle whose
//! last cue is far beyond the video's end is turned down. What was fetched is recorded in
//! `subtitle_files`.

pub mod error;
pub mod hash;
pub mod jobs;
pub mod matching;
pub mod naming;
pub mod opensubtitles;
pub mod pace;
pub mod service;
pub mod srt;
pub mod store;
pub mod stremio;

pub use error::{Error, Result};
pub use service::{Options, SearchReport, SubtitleService};

/// Job kinds.
pub mod kinds {
    pub const SEARCH: &str = "subtitles_search";
}
