//! Radio: shows (radio programmes and podcasts), their series and episodes, stations, and the
//! episode files of an Audiobookshelf podcast library.
//!
//! [`RadioService`] owns the radio tables (migration 0014). Show data comes from
//! [`RadioProvider`]s held in a [`ProviderRegistry`] (the generic podcast feed provider lives
//! in [`providers::rss`]). Episode statuses follow the show's follow choice, the matched file
//! and the first broadcast ([`status`]). Reading the podcast library and matching its files is
//! behind [`RadioLibrarySync`] (a no-op until an Audiobookshelf implementation is attached).
//! Radio root folders share `root_folders` with the library via
//! [`reelix_library::content_roots`]. Indexer searches start from [`RadioService::search_targets`]
//! ([`search`]); finished downloads are filed into the podcast library by
//! [`RadioService::file_download`] ([`filing`]). See `docs/radio.md`.

pub mod artwork;
pub mod catchup;
mod convert;
pub mod custom;
mod discover;
mod error;
pub mod filing;
pub mod genres;
pub mod jobs;
pub mod keep;
mod library;
mod matching;
pub mod naming;
pub mod provider;
pub mod providers;
mod refresh;
mod repo;
mod root_folders;
pub mod search;
mod service;
mod settings;
mod shows;
pub mod status;
pub mod svg;
pub mod tags;
mod wanted;
mod xml;

pub use artwork::{ImageKind, StoredImage};
pub use error::{RadioError, Result};
pub use filing::{FiledEpisode, FilingOutcome};
pub use keep::KeepSummary;
pub use library::{LibrarySyncSummary, NewEpisodeFile, NoopLibrarySync, RadioLibrarySync, clean_title};
pub use matching::source_ids;
pub use naming::EpisodeNaming;
pub use provider::{
    DISCOVER_LIMIT, DISCOVER_TTL, DiscoverList, EpisodeData, ProviderError, ProviderRegistry, RadioProvider,
    SearchPrefs, SeriesData, ShowData, ShowFetch, ShowHit, StationData,
};
pub use refresh::RefreshSummary;
pub use search::{RadioTarget, match_release};
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
    /// Indexer search for episodes (payload `RadioSearchRequest`); handled by the pipeline.
    pub const RADIO_EPISODE_SEARCH: &str = "radio_episode_search";
    /// Indexer search for every wanted episode, every 6 hours; handled by the pipeline.
    pub const RADIO_MISSING_SEARCH: &str = "radio_missing_search";
    /// Catch-up audio for wanted episodes from sources with catch-up on, every hour (RFX-96).
    pub const RADIO_CATCHUP: &str = "radio_catchup";
}

/// Settings keys.
pub mod keys {
    pub const SETTINGS: &str = "radio.settings";
    /// Naming of filed episodes (`RadioNamingDto`).
    pub const NAMING: &str = "radio.naming";
    /// Unix ms of the last finished library sync (set by [`crate::RadioService::mark_library_synced`]).
    pub const LIBRARY_SYNCED_AT: &str = "radio.library_synced_at";
    /// Address of the Audiobookshelf server that held the podcast library at the last sync
    /// (links to its items).
    pub const ABS_WEB_URL: &str = "radio.abs_web_url";
}

/// `entity.changed` entity names.
pub mod entities {
    pub const SHOW: &str = "radio_show";
    pub const EPISODE: &str = "radio_episode";
    pub const EPISODE_FILE: &str = "radio_episode_file";
}
