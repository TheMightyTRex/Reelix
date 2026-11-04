//! Radio: shows (radio programmes and podcasts), their series and episodes, stations, and the
//! episode files of an Audiobookshelf podcast library.
//!
//! [`RadioService`] owns the radio tables (migration 0014). Show data comes from
//! [`RadioProvider`]s held in a [`ProviderRegistry`] (the generic podcast feed provider lives
//! in [`providers::rss`]). Episode statuses follow the show's follow choice, the matched file
//! and the first broadcast ([`status`]). Reading the podcast library and matching its files is
//! behind [`RadioLibrarySync`] (a no-op until an Audiobookshelf implementation is attached).
//! Radio root folders share `root_folders` with the library via
//! [`reelix_library::content_roots`]. See `docs/radio.md`.

mod artwork;
mod convert;
mod error;
pub mod jobs;
mod library;
mod matching;
pub mod provider;
pub mod providers;
mod refresh;
mod repo;
mod root_folders;
mod service;
mod settings;
mod shows;
pub mod status;
mod wanted;
mod xml;

pub use error::{RadioError, Result};
pub use library::{LibrarySyncSummary, NewEpisodeFile, NoopLibrarySync, RadioLibrarySync, clean_title};
pub use matching::source_ids;
pub use provider::{
    EpisodeData, ProviderError, ProviderRegistry, RadioProvider, SearchPrefs, SeriesData, ShowData,
    ShowFetch, ShowHit, StationData,
};
pub use refresh::RefreshSummary;
pub use service::{RadioDeps, RadioService};
pub use settings::check_folder as check_library_folder;
pub use wanted::{WantedEpisode, episode_file_name};

/// Job kinds owned by the radio service.
pub mod kinds {
    /// Every show (or the payload's `show_ids`), every 12 hours.
    pub const REFRESH_RADIO_SHOWS: &str = "refresh_radio_shows";
    /// Shows from podcast feeds only, every hour.
    pub const RADIO_FEED_SYNC: &str = "radio_feed_sync";
    /// Reads the Audiobookshelf podcast library and re-matches ([`crate::RadioLibrarySync`]).
    pub const RADIO_LIBRARY_SYNC: &str = "radio_library_sync";
}

/// Settings keys.
pub mod keys {
    pub const SETTINGS: &str = "radio.settings";
    /// Unix ms of the last finished library sync (set by [`crate::RadioService::mark_library_synced`]).
    pub const LIBRARY_SYNCED_AT: &str = "radio.library_synced_at";
}

/// `entity.changed` entity names.
pub mod entities {
    pub const SHOW: &str = "radio_show";
    pub const EPISODE: &str = "radio_episode";
    pub const EPISODE_FILE: &str = "radio_episode_file";
}
