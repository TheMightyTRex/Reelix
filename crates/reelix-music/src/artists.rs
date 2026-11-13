//! Artists: list, add (with album monitoring), edit, delete and MusicBrainz lookup.

use std::path::{Path, PathBuf};

use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::library::{ImagesDto, Paged};
use reelix_api_types::music::{
    AddArtistRequest, AlbumSearchRequest, ArtistDto, ArtistEdit, ArtistListQuery, ArtistLookupDto,
    ArtistMonitor, NewAlbumMonitor,
};
use reelix_core::ContentKind;
use reelix_library::content_roots;
use reelix_metadata::{ArtistMetadata, ArtistSearchHit};

use crate::error::{MusicError, Result};
use crate::repo::artists::{ArtistFields, NewArtist};
use crate::{MusicService, repo};

/// A MusicBrainz id (UUID) given directly or with an `mbid:` prefix.
pub(crate) fn mbid_term(term: &str) -> Option<&str> {
    let t = term.trim();
    let t = t
        .strip_prefix("mbid:")
        .or_else(|| t.strip_prefix("lidarr:"))
        .map(str::trim)
        .unwrap_or(t);
    let is_uuid = t.len() == 36
        && t.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    is_uuid.then_some(t)
}

pub(crate) fn artist_fields(m: &ArtistMetadata) -> ArtistFields {
    ArtistFields {
        musicbrainz_id: m.id.clone(),
        name: m.name.clone(),
        sort_name: if m.sort_name.trim().is_empty() {
            m.name.clone()
        } else {
            m.sort_name.clone()
        },
        disambiguation: m.disambiguation.clone().filter(|d| !d.is_empty()),
        artist_type: m.artist_type.as_ref().map(|t| t.to_ascii_lowercase()),
        country: m.country.clone(),
        overview: None,
        genres: m.genres.clone(),
        images: ImagesDto::default(),
        links: m.links.clone(),
    }
}

impl MusicService {
    pub async fn list_artists(&self, q: &ArtistListQuery) -> Result<Paged<ArtistDto>> {
        let (items, total, page, page_size) = repo::artists::page(&self.db, q).await?;
        Ok(Paged {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn get_artist(&self, id: i64) -> Result<ArtistDto> {
        repo::artists::require(&self.db, id).await
    }

    pub async fn artist_by_mbid(&self, mbid: &str) -> Result<Option<ArtistDto>> {
        repo::artists::by_mbid(&self.db, mbid).await
    }

    pub async fn add_artist(&self, req: AddArtistRequest) -> Result<ArtistDto> {
        self.add_artist_at(req, None, None).await
    }

    /// Adds an artist; `path` overrides the folder computed from the naming settings
    /// (library import).
    pub(crate) async fn add_artist_at(
        &self,
        req: AddArtistRequest,
        path: Option<String>,
        added_by_request_id: Option<i64>,
    ) -> Result<ArtistDto> {
        let mbid = req.musicbrainz_id.trim().to_owned();
        if mbid.is_empty() {
            return Err(MusicError::Validation("musicbrainz_id is required".into()));
        }
        if repo::artists::by_mbid(&self.db, &mbid).await?.is_some() {
            return Err(MusicError::Conflict(format!(
                "artist {mbid} is already in the library"
            )));
        }
        self.check_quality_profile(req.quality_profile_id).await?;
        self.metadata_profile(req.metadata_profile_id).await?;
        let root = self.music_root(req.root_folder_id).await?;
        let meta = self.metadata.artist(&mbid).await?;
        let fields = artist_fields(&meta);
        let path = match path {
            Some(p) => {
                content_roots::ensure_inside(&p, [root.path.as_str()])?;
                p
            }
            None => {
                let settings = self.settings().await?;
                let cfg = self.naming_config().await?;
                let folder =
                    crate::naming::artist_folder(&settings.naming, &fields.name, &fields.sort_name, &cfg);
                Path::new(&root.path).join(folder).to_string_lossy().into_owned()
            }
        };
        let id = repo::artists::insert(
            &self.db,
            &NewArtist {
                fields,
                monitored: req.monitored,
                monitor_new_items: req.monitor_new_items,
                quality_profile_id: req.quality_profile_id,
                metadata_profile_id: req.metadata_profile_id,
                root_folder_id: req.root_folder_id,
                path: path.clone(),
                tags: req.tags.clone(),
                added_by_request_id,
                custom: false,
            },
        )
        .await?;
        let artist = repo::artists::require(&self.db, id).await?;
        self.sync_albums(
            &artist,
            &meta,
            Some((req.monitor, &req.selected_release_group_ids)),
        )
        .await?;
        repo::artists::update_fields(&self.db, id, &artist_fields(&meta)).await?;
        if Path::new(&path).is_dir()
            && let Err(e) = self.scan_artists(&[id]).await
        {
            tracing::warn!(artist_id = id, error = %e, "scan after add failed");
        }
        self.apply_post_scan_monitor(id, req.monitor).await?;
        self.emit("artist", "created", id);
        // Tracklists of the monitored albums load in the background.
        self.enqueue(
            reelix_db::NewJob::new(
                crate::kinds::REFRESH_ARTISTS,
                serde_json::json!({ "artist_ids": [id], "manual": false }),
            )
            .dedupe(format!("refresh_artists:{id}")),
        )
        .await?;
        if req.search_on_add && req.monitored {
            self.request_search(AlbumSearchRequest {
                kind: ContentSearchKind::Parent,
                ids: vec![id],
            })
            .await?;
        }
        self.get_artist(id).await
    }

    /// `Existing`/`Missing` depend on the files found by the first scan.
    async fn apply_post_scan_monitor(&self, artist_id: i64, monitor: ArtistMonitor) -> Result<()> {
        if !matches!(monitor, ArtistMonitor::Existing | ArtistMonitor::Missing) {
            return Ok(());
        }
        let albums = repo::albums::of_artists(&self.db, &[artist_id]).await?;
        let (with, without): (Vec<_>, Vec<_>) = albums.iter().partition(|a| a.statistics.size_on_disk > 0);
        let ids = |v: &[&reelix_api_types::music::AlbumDto]| v.iter().map(|a| a.id).collect::<Vec<_>>();
        let existing = monitor == ArtistMonitor::Existing;
        repo::albums::set_monitored(&self.db, &ids(&with), existing).await?;
        repo::albums::set_monitored(&self.db, &ids(&without), !existing).await?;
        Ok(())
    }

    pub async fn update_artist(&self, id: i64, e: ArtistEdit) -> Result<ArtistDto> {
        let mut a = repo::artists::require(&self.db, id).await?;
        if let Some(q) = e.quality_profile_id {
            self.check_quality_profile(q).await?;
            a.quality_profile_id = q;
        }
        if let Some(m) = e.metadata_profile_id {
            self.metadata_profile(m).await?;
            a.metadata_profile_id = m;
        }
        let old_path = a.path.clone();
        if let Some(r) = e.root_folder_id
            && r != a.root_folder_id
        {
            let root = self.music_root(r).await?;
            a.root_folder_id = r;
            if e.path.is_none() {
                let folder = Path::new(&old_path)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| a.name.clone());
                a.path = Path::new(&root.path).join(folder).to_string_lossy().into_owned();
            }
        }
        if let Some(p) = e.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            if !Path::new(p).is_absolute() {
                return Err(MusicError::Validation("path must be absolute".into()));
            }
            a.path = p.trim_end_matches(['/', '\\']).to_owned();
        }
        if let Some(m) = e.monitored {
            a.monitored = m;
        }
        if let Some(m) = e.monitor_new_items {
            a.monitor_new_items = m;
        }
        if let Some(t) = e.tags {
            a.tags = t;
        }
        if a.path != old_path {
            content_roots::ensure_inside_kinds(&self.db, &a.path, &[ContentKind::Music]).await?;
        }
        if a.path != old_path && e.move_files && Path::new(&old_path).is_dir() {
            let guard = self.path_guard().await?;
            guard.check_existing(Path::new(&old_path))?;
            let to = guard.check(Path::new(&a.path))?;
            if to.exists() {
                return Err(MusicError::Conflict(format!("{} already exists", a.path)));
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&old_path, &to)?;
        }
        repo::artists::save_settings(&self.db, &a).await?;
        self.emit("artist", "updated", id);
        self.get_artist(id).await
    }

    pub async fn delete_artist(&self, id: i64, delete_files: bool) -> Result<()> {
        let a = repo::artists::require(&self.db, id).await?;
        if delete_files {
            self.remove_path(&PathBuf::from(&a.path)).await?;
        }
        repo::artists::delete(&self.db, id).await?;
        self.emit("artist", "deleted", id);
        Ok(())
    }

    /// `mbid:` prefix (or a bare MusicBrainz id) supported.
    pub async fn lookup_artists(&self, term: &str) -> Result<Vec<ArtistLookupDto>> {
        let hits: Vec<ArtistSearchHit> = match mbid_term(term) {
            Some(id) => {
                let a = self.metadata.artist(id).await?;
                vec![ArtistSearchHit {
                    id: a.id,
                    name: a.name,
                    sort_name: a.sort_name,
                    disambiguation: a.disambiguation,
                    artist_type: a.artist_type,
                    country: a.country,
                    score: 100,
                }]
            }
            None if term.trim().is_empty() => Vec::new(),
            None => self.metadata.search_artists(term.trim()).await?,
        };
        let index = repo::artists::mbids(&self.db).await?;
        Ok(hits
            .into_iter()
            .map(|h| ArtistLookupDto {
                artist_id: index.iter().find(|(_, m)| *m == h.id).map(|(id, _)| *id),
                musicbrainz_id: h.id,
                sort_name: if h.sort_name.is_empty() {
                    h.name.clone()
                } else {
                    h.sort_name
                },
                name: h.name,
                disambiguation: h.disambiguation.filter(|d| !d.is_empty()),
                artist_type: h.artist_type,
                country: h.country,
                image: None,
            })
            .collect())
    }

    /// Monitoring of an album added later by a refresh.
    pub(crate) fn monitor_new(&self, artist: &ArtistDto, release_date: Option<chrono::NaiveDate>) -> bool {
        match artist.monitor_new_items {
            NewAlbumMonitor::All => true,
            NewAlbumMonitor::None => false,
            NewAlbumMonitor::New => release_date.is_none_or(|d| d >= artist.added.date_naive()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbid_terms() {
        let id = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
        assert_eq!(mbid_term(id), Some(id));
        assert_eq!(mbid_term(&format!("mbid:{id}")), Some(id));
        assert_eq!(mbid_term("radiohead"), None);
    }
}
