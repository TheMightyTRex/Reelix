//! Provider-neutral music metadata (artists, release groups, releases, tracks) and the
//! [`MusicMetadataProvider`] trait implemented by [`MusicBrainz`](crate::MusicBrainz).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::MetaError;

/// One credited artist; `join_phrase` follows the name (e.g. ` feat. `).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ArtistCredit {
    pub artist_id: String,
    pub name: String,
    pub join_phrase: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TrackMetadata {
    /// MusicBrainz track id.
    pub id: String,
    pub recording_id: String,
    /// 1-based position on the medium.
    pub position: u32,
    /// Display number, e.g. `A1`.
    pub number: String,
    pub title: String,
    pub length_ms: Option<u64>,
    pub artist_credit: Vec<ArtistCredit>,
}

/// One disc / side set of a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MediumMetadata {
    pub position: u32,
    pub format: Option<String>,
    pub title: Option<String>,
    pub tracks: Vec<TrackMetadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ReleaseMetadata {
    pub id: String,
    pub release_group_id: String,
    pub title: String,
    pub status: Option<String>,
    pub country: Option<String>,
    pub date: Option<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
    pub disambiguation: Option<String>,
    pub media: Vec<MediumMetadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ReleaseGroupMetadata {
    pub id: String,
    pub title: String,
    pub disambiguation: Option<String>,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    pub first_release_date: Option<String>,
    pub artist_credit: Vec<ArtistCredit>,
    pub genres: Vec<String>,
    pub rating: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ArtistMetadata {
    pub id: String,
    pub name: String,
    pub sort_name: String,
    pub disambiguation: Option<String>,
    pub artist_type: Option<String>,
    pub country: Option<String>,
    pub genres: Vec<String>,
    pub links: Vec<String>,
    pub release_groups: Vec<ReleaseGroupMetadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ArtistSearchHit {
    pub id: String,
    pub name: String,
    pub sort_name: String,
    pub disambiguation: Option<String>,
    pub artist_type: Option<String>,
    pub country: Option<String>,
    /// MusicBrainz search score, 0–100.
    pub score: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ReleaseGroupSearchHit {
    pub id: String,
    pub title: String,
    pub artist_id: String,
    pub artist_name: String,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    pub first_release_date: Option<String>,
    pub score: u8,
}

/// Cover Art Archive images of a release group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CoverArt {
    pub front: Option<String>,
    pub thumb_500: Option<String>,
    pub thumb_250: Option<String>,
}

/// Music metadata source used by the music service.
#[async_trait]
pub trait MusicMetadataProvider: Send + Sync {
    /// Stable implementation id (`musicbrainz`).
    fn name(&self) -> &'static str;
    async fn search_artists(&self, q: &str) -> Result<Vec<ArtistSearchHit>, MetaError>;
    async fn search_release_groups(
        &self,
        q: &str,
        artist: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError>;
    /// Artist incl. every release group (browse, paged 100).
    async fn artist(&self, mbid: &str) -> Result<ArtistMetadata, MetaError>;
    async fn release_group(&self, mbid: &str) -> Result<ReleaseGroupMetadata, MetaError>;
    /// Releases of a group with media + tracks (`/release?release-group=…&inc=recordings+media+labels`).
    async fn releases(&self, release_group_mbid: &str) -> Result<Vec<ReleaseMetadata>, MetaError>;
    async fn release(&self, mbid: &str) -> Result<ReleaseMetadata, MetaError>;
    /// Cover Art Archive `/release-group/{id}`; `Ok(None)` on 404.
    async fn cover_art(&self, release_group_mbid: &str) -> Result<Option<CoverArt>, MetaError>;
}

/// Optional persistent response cache (e.g. backed by the `metadata_cache` table) consulted by
/// [`MusicBrainz`](crate::MusicBrainz) and [`OpenLibrary`](crate::OpenLibrary) after their
/// in-memory cache and before the network. Implementations log their own errors.
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// The raw JSON payload stored for `(provider, key)` if it has not expired.
    async fn load(&self, provider: &str, key: &str) -> Option<String>;
    /// Stores a raw JSON payload for `ttl`.
    async fn save(&self, provider: &str, key: &str, payload: &str, ttl: std::time::Duration);
}
