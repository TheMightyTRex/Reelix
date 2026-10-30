//! Music screens.

mod add;
mod album_detail;
mod artist_detail;
mod artists;
mod import;
mod metadata_profiles;
mod settings;

pub use add::AddArtistPage;
pub use album_detail::AlbumDetailPage;
pub use artist_detail::ArtistDetailPage;
pub use artists::ArtistsPage;
pub use import::ImportArtistsPage;
pub use metadata_profiles::MetadataProfilesPage;
pub use settings::{MusicMetadataCard, MusicNamingCard};
