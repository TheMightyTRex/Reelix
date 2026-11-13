//! Artists added with the owner's own details (RFX-90): no MusicBrainz match, a typed name,
//! overview and picture, and a folder that already exists in a music root folder. The
//! artist's `musicbrainz_id` is `custom:{id}` and `custom = 1` (migration 0020): refreshes
//! leave it alone, and each scan makes an album for every folder of audio files no album
//! matches ([`MusicService::custom_album`]), named after the files' album tag or the folder.
//! Custom albums have no MusicBrainz releases, so their files are kept without a tracklist.

use std::path::Path;

use reelix_api_types::custom::{CustomDetails, CustomDetailsChange};
use reelix_api_types::library::ImagesDto;
use reelix_api_types::music::{AlbumDto, ArtistDto, NewAlbumMonitor};
use reelix_library::custom::{custom_folder, folder_owner, folder_taken};
use reelix_parser::QualityFamily;

use crate::convert::clean_name;
use crate::error::{MusicError, Result};
use crate::repo::albums::AlbumFields;
use crate::repo::artists::{ArtistFields, NewArtist};
use crate::{MusicService, repo};

impl MusicService {
    /// Adds an artist with typed details from an existing folder of a music root folder;
    /// scans it at once (as adding an artist does), which makes the albums.
    pub async fn add_custom_artist(
        &self,
        d: &CustomDetails,
        root_folder_id: i64,
        path: &str,
        quality_profile_id: Option<i64>,
        monitored: bool,
    ) -> Result<ArtistDto> {
        let root = self.music_root(root_folder_id).await?;
        let folder = custom_folder(&root.path, path)?;
        let artists = repo::artists::load(&self.db, None).await?;
        if let Some((id, name)) = folder_owner(
            &folder,
            artists.iter().map(|a| (a.id, a.name.as_str(), a.path.as_str())),
        ) {
            return Err(folder_taken(&folder, "artist", id, &name).into());
        }
        let profile = match quality_profile_id {
            Some(id) => {
                self.check_quality_profile(id).await?;
                id
            }
            None => self
                .db
                .quality_profiles()
                .list_family(QualityFamily::Music)
                .await?
                .first()
                .map(|p| p.id)
                .ok_or_else(|| MusicError::Validation("there is no music quality profile".into()))?,
        };
        let metadata_profile = self
            .metadata_profiles()
            .await?
            .first()
            .map(|p| p.id)
            .ok_or_else(|| MusicError::Validation("there is no metadata profile".into()))?;
        let id = repo::artists::insert(
            &self.db,
            &NewArtist {
                fields: ArtistFields {
                    // Unique until the row has an id; then `custom:{id}`.
                    musicbrainz_id: format!(
                        "custom:new:{}",
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
                    ),
                    name: d.title.clone(),
                    sort_name: d.title.clone(),
                    disambiguation: None,
                    artist_type: None,
                    country: None,
                    overview: d.overview.clone(),
                    genres: Vec::new(),
                    images: ImagesDto::default(),
                    links: Vec::new(),
                },
                monitored,
                monitor_new_items: NewAlbumMonitor::None,
                quality_profile_id: profile,
                metadata_profile_id: metadata_profile,
                root_folder_id: root.id,
                path: folder.clone(),
                tags: Vec::new(),
                added_by_request_id: None,
                custom: true,
            },
        )
        .await?;
        let now = reelix_db::now_ms();
        sqlx::query("UPDATE artists SET musicbrainz_id = ?, last_refreshed = ? WHERE id = ?")
            .bind(format!("custom:{id}"))
            .bind(now)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        tracing::info!(id, name = %d.title, "custom artist added");
        if Path::new(&folder).is_dir()
            && let Err(e) = self.scan_artists(&[id]).await
        {
            tracing::warn!(artist_id = id, error = %e, "scan after add failed");
        }
        self.emit("artist", "created", id);
        self.get_artist(id).await
    }

    async fn custom_artist(&self, id: i64) -> Result<ArtistDto> {
        let a = repo::artists::require(&self.db, id).await?;
        if !a.custom {
            return Err(MusicError::Validation(format!(
                "artist {id} has a MusicBrainz match; only artists added with their own details can be changed here"
            )));
        }
        Ok(a)
    }

    async fn save_custom_fields(&self, a: &ArtistDto) -> Result<()> {
        sqlx::query(
            "UPDATE artists SET name = ?, sort_name = ?, clean_name = ?, overview = ?, images = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&a.name)
        .bind(&a.sort_name)
        .bind(clean_name(&a.name))
        .bind(&a.overview)
        .bind(serde_json::to_string(&a.images).unwrap_or_else(|_| "{}".into()))
        .bind(reelix_db::now_ms())
        .bind(a.id)
        .execute(self.db.pool())
        .await?;
        self.emit("artist", "updated", a.id);
        Ok(())
    }

    /// Changes the typed name and overview of a custom artist (artists have no year).
    pub async fn change_custom_artist(&self, id: i64, c: &CustomDetailsChange) -> Result<ArtistDto> {
        let mut a = self.custom_artist(id).await?;
        if let Some(t) = &c.title {
            a.name = t.clone();
            a.sort_name = t.clone();
        }
        if let Some(o) = &c.overview {
            a.overview = o.clone();
        }
        self.save_custom_fields(&a).await?;
        self.get_artist(id).await
    }

    /// Sets (or removes) the picture of a custom artist.
    pub async fn set_custom_artist_picture(&self, id: i64, poster: Option<String>) -> Result<()> {
        let mut a = self.custom_artist(id).await?;
        a.images.poster = poster;
        self.save_custom_fields(&a).await
    }

    /// The album of a custom artist for a folder no album matched: made (not monitored, with
    /// no releases to fetch) when the artist has none of that title yet.
    pub(crate) async fn custom_album(&self, artist: &ArtistDto, title: &str) -> Result<Option<AlbumDto>> {
        let key = clean_name(title);
        if key.is_empty() {
            return Ok(None);
        }
        let rg = format!("custom:{}:{key}", artist.id);
        if let Some(a) = repo::albums::by_mbid(&self.db, &rg).await? {
            return Ok(Some(a));
        }
        let id = repo::albums::insert(
            &self.db,
            artist.id,
            &AlbumFields {
                musicbrainz_release_group_id: rg,
                title: title.to_owned(),
                disambiguation: None,
                album_type: Default::default(),
                secondary_types: Vec::new(),
                release_date: None,
                genres: Vec::new(),
                images: ImagesDto::default(),
                ratings: Default::default(),
            },
            false,
            None,
        )
        .await?;
        // No MusicBrainz releases: marking it synced stops Reelix asking for them.
        repo::albums::mark_synced(&self.db, id).await?;
        self.emit("album", "created", id);
        Ok(Some(repo::albums::require(&self.db, id).await?))
    }
}
