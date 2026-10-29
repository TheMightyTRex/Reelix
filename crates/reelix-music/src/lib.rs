//! Music (M9): artists, albums (MusicBrainz release groups), releases, tracks, track files,
//! library import, download import and renaming.
//!
//! [`MusicService`] owns the music tables (migration 0009). Metadata comes through
//! [`reelix_metadata::MusicMetadataProvider`] (MusicBrainz + Cover Art Archive). The pipeline
//! searches and grabs through [`MusicService::search_api`] and hands finished downloads to
//! [`MusicService::import_api`]; music root folders share `root_folders` with the library via
//! [`reelix_library::content_roots`].

mod albums;
mod artists;
mod availability;
mod convert;
mod error;
pub mod import_api;
pub mod jobs;
mod library_import;
pub mod matching;
pub mod naming;
mod profiles;
mod refresh;
mod releases;
mod rename;
mod repo;
mod root_folders;
mod scan;
pub mod search_api;
mod service;
mod settings;
pub mod tags;

pub use error::{MusicError, Result};
pub use import_api::{AlbumImportOutcome, ImportedAlbum, ImportedTrackFile, MusicImportApi};
pub use reelix_import::content::ContentDownload;
pub use reelix_library::{Availability, AvailabilityStatus};
pub use search_api::{AlbumTarget, MusicSearchApi, Wanted};
pub use service::{MusicDeps, MusicService, ScanSummary};

/// The metadata seam, re-exported so dependants (API tests, fakes) need no direct
/// `reelix-metadata` dependency.
pub mod provider {
    pub use async_trait::async_trait;
    pub use reelix_metadata::{
        ArtistCredit, ArtistMetadata, ArtistSearchHit, CoverArt, MediumMetadata, MetaError,
        MusicMetadataProvider, ReleaseGroupMetadata, ReleaseGroupSearchHit, ReleaseMetadata, TrackMetadata,
    };
}

/// Job kinds owned by the music service.
pub mod kinds {
    pub const REFRESH_ARTISTS: &str = "refresh_artists";
    pub const MUSIC_SCAN: &str = "music_scan";
    pub const MUSIC_RENAME: &str = "music_rename";
    /// Handled by the pipeline.
    pub const ALBUM_SEARCH: &str = "album_search";
}

/// Settings keys.
pub mod keys {
    pub const SETTINGS: &str = "music.settings";
}
