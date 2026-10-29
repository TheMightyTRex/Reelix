//! Albums (release groups), their tracks and track files, wanted lists and album lookup.

use std::collections::HashMap;
use std::path::Path;

use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::library::Paged;
use reelix_api_types::music::{
    AddAlbumRequest, AddArtistRequest, AlbumDto, AlbumEdit, AlbumListQuery, AlbumLookupDto,
    AlbumMonitorRequest, AlbumSearchRequest, AlbumWantedQuery, ArtistMonitor, NewAlbumMonitor, TrackDto,
    TrackFileDto,
};
use reelix_core::ContentKind;
use reelix_metadata::ReleaseGroupSearchHit;
use reelix_parser::QualityFamily;

use crate::artists::mbid_term;
use crate::convert::{album_type, parse_date, secondary_type};
use crate::error::{MusicError, Result};
use crate::{MusicService, repo};

/// Cover Art Archive thumbnail of a release group (redirects; 404 when there is none).
pub(crate) fn caa_thumb(rg: &str) -> String {
    format!("https://coverartarchive.org/release-group/{rg}/front-250")
}

impl MusicService {
    pub async fn list_albums(&self, q: &AlbumListQuery) -> Result<Paged<AlbumDto>> {
        let (items, total, page, page_size) = repo::albums::page(&self.db, q).await?;
        Ok(Paged {
            items,
            total,
            page,
            page_size,
        })
    }

    /// With releases (fetched from MusicBrainz on first access).
    pub async fn get_album(&self, id: i64) -> Result<AlbumDto> {
        repo::albums::require(&self.db, id).await?;
        self.ensure_releases_quiet(id).await;
        let mut a = repo::albums::require(&self.db, id).await?;
        a.releases = repo::releases::of_album(&self.db, id)
            .await?
            .iter()
            .map(|r| r.to_dto(a.selected_release_id))
            .collect();
        Ok(a)
    }

    pub async fn album_by_mbid(&self, rg_mbid: &str) -> Result<Option<AlbumDto>> {
        repo::albums::by_mbid(&self.db, rg_mbid).await
    }

    /// Adds one album; creates its artist (nothing else monitored) when missing.
    pub async fn add_album(&self, req: AddAlbumRequest) -> Result<AlbumDto> {
        let rg_id = req.musicbrainz_release_group_id.trim().to_owned();
        if let Some(existing) = repo::albums::by_mbid(&self.db, &rg_id).await? {
            if existing.monitored {
                return Err(MusicError::Conflict(format!(
                    "album {rg_id} is already in the library"
                )));
            }
            // Present but unmonitored (added with its artist): monitor it.
            repo::albums::set_monitored(&self.db, &[existing.id], true).await?;
            self.ensure_releases_quiet(existing.id).await;
            self.after_album_added(existing.id, req.search_on_add).await?;
            return self.get_album(existing.id).await;
        }
        let rg = self.metadata.release_group(&rg_id).await?;
        let artist_mbid = rg
            .artist_credit
            .first()
            .map(|c| c.artist_id.clone())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MusicError::Validation(format!("release group {rg_id} has no artist")))?;
        let artist = match repo::artists::by_mbid(&self.db, &artist_mbid).await? {
            Some(a) => a,
            None => {
                let quality_profile_id = match req.quality_profile_id {
                    Some(id) => id,
                    None => self
                        .db
                        .quality_profiles()
                        .list_family(QualityFamily::Music)
                        .await?
                        .first()
                        .map(|p| p.id)
                        .ok_or_else(|| MusicError::Validation("no music quality profile exists".into()))?,
                };
                let metadata_profile_id = match req.metadata_profile_id {
                    Some(id) => id,
                    None => self
                        .metadata_profiles()
                        .await?
                        .first()
                        .map(|p| p.id)
                        .ok_or_else(|| MusicError::Validation("no metadata profile exists".into()))?,
                };
                let root_folder_id = match req.root_folder_id {
                    Some(id) => id,
                    None => reelix_library::content_roots::list(&self.db, &[ContentKind::Music])
                        .await?
                        .first()
                        .map(|r| r.id)
                        .ok_or_else(|| MusicError::Validation("no music root folder exists".into()))?,
                };
                self.add_artist_at(
                    AddArtistRequest {
                        musicbrainz_id: artist_mbid.clone(),
                        quality_profile_id,
                        metadata_profile_id,
                        root_folder_id,
                        monitored: true,
                        monitor: ArtistMonitor::None,
                        monitor_new_items: NewAlbumMonitor::None,
                        selected_release_group_ids: Vec::new(),
                        tags: req.tags.clone(),
                        search_on_add: false,
                    },
                    None,
                    req.added_by_request_id,
                )
                .await?
            }
        };
        let id = match repo::albums::by_mbid(&self.db, &rg_id).await? {
            Some(a) => {
                repo::albums::set_monitored(&self.db, &[a.id], true).await?;
                a.id
            }
            // Filtered out by the metadata profile: add it explicitly.
            None => {
                repo::albums::insert(
                    &self.db,
                    artist.id,
                    &crate::refresh::album_fields(&rg),
                    true,
                    req.added_by_request_id,
                )
                .await?
            }
        };
        if let Some(r) = req.added_by_request_id {
            sqlx::query(
                "UPDATE albums SET added_by_request_id = ? WHERE id = ? AND added_by_request_id IS NULL",
            )
            .bind(r)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        }
        self.ensure_releases_quiet(id).await;
        self.emit("album", "created", id);
        self.after_album_added(id, req.search_on_add).await?;
        self.get_album(id).await
    }

    async fn after_album_added(&self, id: i64, search: bool) -> Result<()> {
        if search {
            self.request_search(AlbumSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![id],
            })
            .await?;
        }
        Ok(())
    }

    pub async fn update_album(&self, id: i64, e: AlbumEdit) -> Result<AlbumDto> {
        repo::albums::require(&self.db, id).await?;
        if let Some(m) = e.monitored {
            repo::albums::set_monitored(&self.db, &[id], m).await?;
        }
        if let Some(v) = e.any_release_ok {
            repo::albums::set_any_release_ok(&self.db, id, v).await?;
        }
        self.emit("album", "updated", id);
        self.get_album(id).await
    }

    pub async fn monitor_albums(&self, req: AlbumMonitorRequest) -> Result<Vec<AlbumDto>> {
        let existing = repo::albums::load(&self.db, Some(&req.album_ids)).await?;
        let ids: Vec<i64> = existing.iter().map(|a| a.id).collect();
        repo::albums::set_monitored(&self.db, &ids, req.monitored).await?;
        if req.monitored {
            for id in &ids {
                self.ensure_releases_quiet(*id).await;
            }
        }
        for id in &ids {
            self.emit("album", "updated", *id);
        }
        repo::albums::load(&self.db, Some(&ids)).await
    }

    pub async fn delete_album(&self, id: i64, delete_files: bool) -> Result<()> {
        let album = repo::albums::require(&self.db, id).await?;
        if delete_files {
            let artist = repo::artists::require(&self.db, album.artist_id).await?;
            for f in repo::files::of_albums(&self.db, &[id]).await? {
                let p = Path::new(&artist.path).join(&f.relative_path);
                self.remove_path(&p).await?;
                if let Some(parent) = p.parent() {
                    Self::prune_empty_dirs(parent, Path::new(&artist.path));
                }
            }
        }
        repo::albums::delete(&self.db, id).await?;
        self.emit("album", "deleted", id);
        Ok(())
    }

    pub async fn lookup_albums(&self, term: &str) -> Result<Vec<AlbumLookupDto>> {
        let hits: Vec<ReleaseGroupSearchHit> = match mbid_term(term) {
            Some(id) => {
                let rg = self.metadata.release_group(id).await?;
                let credit = rg.artist_credit.first().cloned().unwrap_or_default();
                vec![ReleaseGroupSearchHit {
                    id: rg.id,
                    title: rg.title,
                    artist_id: credit.artist_id,
                    artist_name: crate::convert::credit_name(&rg.artist_credit).unwrap_or(credit.name),
                    primary_type: rg.primary_type,
                    secondary_types: rg.secondary_types,
                    first_release_date: rg.first_release_date,
                    score: 100,
                }]
            }
            None if term.trim().is_empty() => Vec::new(),
            None => self.metadata.search_release_groups(term.trim(), None).await?,
        };
        let mut out = Vec::new();
        for h in hits {
            let album_id = repo::albums::by_mbid(&self.db, &h.id).await?.map(|a| a.id);
            out.push(AlbumLookupDto {
                cover: Some(caa_thumb(&h.id)),
                musicbrainz_release_group_id: h.id,
                title: h.title,
                artist_name: h.artist_name,
                artist_musicbrainz_id: h.artist_id,
                album_type: album_type(h.primary_type.as_deref()),
                secondary_types: h
                    .secondary_types
                    .iter()
                    .filter_map(|s| secondary_type(s))
                    .collect(),
                year: parse_date(h.first_release_date.as_deref()).map(|d| chrono::Datelike::year(&d)),
                album_id,
            });
        }
        Ok(out)
    }

    /// Tracks of the album's selected release, with their files.
    pub async fn tracks(&self, album_id: i64) -> Result<Vec<TrackDto>> {
        let album = repo::albums::require(&self.db, album_id).await?;
        self.ensure_releases_quiet(album_id).await;
        let album = repo::albums::require(&self.db, album.id).await?;
        let Some(release) = album.selected_release_id else {
            return Ok(Vec::new());
        };
        let files: HashMap<i64, TrackFileDto> = self
            .track_files(album_id)
            .await?
            .into_iter()
            .map(|f| (f.id, f))
            .collect();
        let mut tracks = repo::tracks::of_release(&self.db, release).await?;
        for t in &mut tracks {
            t.track_file = t.track_file_id.and_then(|f| files.get(&f).cloned());
        }
        Ok(tracks)
    }

    pub async fn track_files(&self, album_id: i64) -> Result<Vec<TrackFileDto>> {
        let album = repo::albums::require(&self.db, album_id).await?;
        let artist = repo::artists::require(&self.db, album.artist_id).await?;
        let cache = self.profiles().await?;
        let profile = cache.profile(artist.quality_profile_id);
        Ok(repo::files::of_albums(&self.db, &[album_id])
            .await?
            .iter()
            .map(|f| f.to_dto(&artist.path, profile, &cache.format_names))
            .collect())
    }

    pub async fn delete_track_file(&self, id: i64) -> Result<()> {
        let f = repo::files::get(&self.db, id)
            .await?
            .ok_or_else(|| MusicError::NotFound(format!("track file {id} not found")))?;
        let album = repo::albums::require(&self.db, f.album_id).await?;
        let artist = repo::artists::require(&self.db, album.artist_id).await?;
        let p = Path::new(&artist.path).join(&f.relative_path);
        self.remove_path(&p).await?;
        if let Some(parent) = p.parent() {
            Self::prune_empty_dirs(parent, Path::new(&artist.path));
        }
        repo::files::delete(&self.db, id).await?;
        self.emit("trackfile", "deleted", id);
        self.emit("album", "updated", album.id);
        Ok(())
    }

    /// Whether a monitored album lacks tracks.
    pub(crate) fn is_missing(a: &AlbumDto) -> bool {
        if a.statistics.track_count == 0 {
            a.statistics.size_on_disk == 0
        } else {
            a.statistics.track_file_count < a.statistics.track_count
        }
    }

    /// Whether some file of the album is below the profile cutoff.
    pub(crate) async fn below_cutoff(&self, albums: &[AlbumDto]) -> Result<HashMap<i64, bool>> {
        let cache = self.profiles().await?;
        let ids: Vec<i64> = albums.iter().map(|a| a.id).collect();
        let artist_ids: Vec<i64> = albums.iter().map(|a| a.artist_id).collect();
        let artists: HashMap<i64, i64> = repo::artists::load(&self.db, Some(&artist_ids))
            .await?
            .into_iter()
            .map(|a| (a.id, a.quality_profile_id))
            .collect();
        let mut out: HashMap<i64, bool> = HashMap::new();
        let profile_of: HashMap<i64, i64> = albums
            .iter()
            .filter_map(|a| artists.get(&a.artist_id).map(|p| (a.id, *p)))
            .collect();
        for f in repo::files::of_albums(&self.db, &ids).await? {
            let met = profile_of
                .get(&f.album_id)
                .and_then(|p| cache.profile(*p))
                .is_some_and(|p| p.cutoff_met(f.quality.quality, f.custom_format_score));
            let e = out.entry(f.album_id).or_insert(false);
            *e |= !met;
        }
        Ok(out)
    }

    /// Monitored, released albums that are missing tracks (`kind=missing`, default) or whose
    /// files are below the profile cutoff (`kind=cutoff`).
    pub async fn wanted(&self, q: &AlbumWantedQuery) -> Result<Paged<AlbumDto>> {
        let all = repo::albums::wanted_candidates(&self.db, self.today()).await?;
        let list: Vec<AlbumDto> = match q.kind.as_deref().unwrap_or("missing") {
            "missing" => all.into_iter().filter(Self::is_missing).collect(),
            "cutoff" => {
                let with_files: Vec<AlbumDto> = all
                    .into_iter()
                    .filter(|a| a.statistics.size_on_disk > 0)
                    .collect();
                let below = self.below_cutoff(&with_files).await?;
                with_files
                    .into_iter()
                    .filter(|a| below.get(&a.id).copied().unwrap_or(false))
                    .collect()
            }
            other => {
                return Err(MusicError::Validation(format!(
                    "unknown wanted kind {other:?} (missing | cutoff)"
                )));
            }
        };
        let total = list.len() as u64;
        let (page, size, window) = repo::page_window(q.page, q.page_size);
        let items = match window {
            Some((o, l)) => list.into_iter().skip(o as usize).take(l as usize).collect(),
            None => list,
        };
        Ok(Paged {
            items,
            total,
            page,
            page_size: size,
        })
    }
}
