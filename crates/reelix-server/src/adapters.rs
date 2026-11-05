//! Adapters that connect domain crates which must not depend on each other.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use reelix_api_types::books::{AddBookRequest as LibAddBook, AuthorEdit, BookEdit, BookSearchRequest};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::library::{
    AddMovieRequest as LibAddMovie, AddSeriesRequest as LibAddSeries, LibrarySearchKind, MinimumAvailability,
    SearchRequest, SeasonMonitorRequest, SeriesType,
};
use reelix_api_types::music::{AddAlbumRequest as LibAddAlbum, AlbumSearchRequest, AlbumType};
use reelix_api_types::radio::{
    AddRadioShowRequest, RadioSearchRequest, RadioShowHitDto, RadioShowPreviewDto,
};
use reelix_api_types::requests::{
    AvailabilityStatus, ContentRequestKind, NamedIdDto, RadioDiscoverDto, RootFolderOptionDto,
    split_radio_foreign_id,
};
use reelix_core::{BookFormat, ContentKind, MediaKind};
use reelix_db::Db;
use reelix_library::{Availability as LibAvailability, AvailabilityStatus as LibStatus, LibraryService};
use reelix_parser::QualityFamily;
use reelix_requests::gateway::{
    AddAlbumRequest, AddBookRequest, AddMovieRequest, AddRadioRequest, AddSeriesRequest, ArtistHit,
    Availability, ContentAdded, ContentHit, LibraryGateway,
};

/// [`LibraryGateway`] backed by the real [`LibraryService`].
pub struct LibraryAdapter {
    pub library: Arc<LibraryService>,
    pub db: Db,
    /// Book requests.
    pub books: Arc<reelix_books::BooksService>,
    /// Album requests.
    pub music: Arc<reelix_music::MusicService>,
    /// Book Discover rows (shares its cache and rate limit with the books service's client).
    pub open_library: reelix_metadata::OpenLibrary,
    /// Music Discover rows.
    pub listenbrainz: reelix_metadata::ListenBrainz,
    /// Radio Discover, search, previews and follows.
    pub radio: Arc<reelix_radio::RadioService>,
}

/// A radio error as an upstream failure (`None` for an unknown show, id or source).
fn radio_missing(e: &reelix_radio::RadioError) -> bool {
    use reelix_radio::{ProviderError, RadioError};
    matches!(
        e,
        RadioError::NotFound(_)
            | RadioError::Validation(_)
            | RadioError::Provider(ProviderError::NotFound(_))
    )
}

fn status(s: LibStatus) -> AvailabilityStatus {
    match s {
        LibStatus::Available => AvailabilityStatus::Available,
        LibStatus::PartiallyAvailable => AvailabilityStatus::PartiallyAvailable,
        LibStatus::Monitored => AvailabilityStatus::Monitored,
        // Unmonitored items are treated as not requested, so they can be requested again.
        LibStatus::Unmonitored | LibStatus::NotInLibrary => AvailabilityStatus::Unknown,
    }
}

fn availability(a: LibAvailability) -> Availability {
    Availability {
        status: status(a.status),
        library_id: a.library_id,
        seasons: a.seasons.into_iter().map(|(n, s)| (n, status(s))).collect(),
    }
}

impl LibraryAdapter {
    async fn default_profile(&self, requested: Option<i64>) -> anyhow::Result<i64> {
        if let Some(id) = requested {
            return Ok(id);
        }
        let profiles = self
            .db
            .quality_profiles()
            .list()
            .await
            .map_err(|e| anyhow!("{e}"))?;
        profiles
            .first()
            .map(|p| p.id)
            .context("no quality profile exists; create one under Settings → Profiles")
    }

    async fn default_root(&self, kind: MediaKind, requested: Option<i64>) -> anyhow::Result<i64> {
        if let Some(id) = requested {
            return Ok(id);
        }
        let roots = self.library.root_folders().await.map_err(|e| anyhow!("{e}"))?;
        roots
            .into_iter()
            .find(|r| r.media_kind == kind)
            .map(|r| r.id)
            .context("no root folder for this media type; add one under Settings → Media Management")
    }

    /// First quality profile of a book/music family (used when the request settings name none).
    async fn first_content_profile(&self, kind: ContentKind) -> anyhow::Result<Option<i64>> {
        let profiles = self
            .db
            .quality_profiles()
            .list_family(family(kind))
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(profiles.first().map(|p| p.id))
    }

    async fn first_content_root(&self, kind: ContentKind) -> anyhow::Result<Option<i64>> {
        Ok(self.content_roots(kind).await?.into_iter().next().map(|r| r.id))
    }

    async fn content_roots(&self, kind: ContentKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
        let roots = match kind {
            ContentKind::Music => self.music.root_folders().await.map_err(|e| anyhow!("{e}"))?,
            ContentKind::Radio => self.radio.root_folders().await.map_err(|e| anyhow!("{e}"))?,
            _ => self.books.root_folders().await.map_err(|e| anyhow!("{e}"))?,
        };
        Ok(roots
            .into_iter()
            .filter(|r| r.kind == kind)
            .map(|r| RootFolderOptionDto {
                id: r.id,
                path: r.path,
                free_space: r.free_space,
            })
            .collect())
    }

    /// Ids (of `column` = `book_id` / `album_id`) with a download in progress.
    async fn downloading(&self, column: &str, ids: &[i64]) -> anyhow::Result<HashSet<i64>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(format!(
            "SELECT DISTINCT {column} FROM tracked_downloads WHERE state IN \
             ('downloading', 'import_pending', 'importing', 'failed_pending') AND {column} IN ("
        ));
        let mut sep = q.separated(", ");
        for id in ids {
            sep.push_bind(*id);
        }
        q.push(")");
        let rows: Vec<i64> = q
            .build_query_scalar()
            .fetch_all(self.db.pool())
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(rows.into_iter().collect())
    }

    /// Makes sure the book's author has a profile (and root) for every requested format, so
    /// the book can be monitored in that format.
    async fn ensure_author_formats(&self, author_id: i64, req: &AddBookRequest) -> anyhow::Result<()> {
        let author = self
            .books
            .get_author(author_id)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let mut edit = AuthorEdit::default();
        let mut changed = false;
        if req.formats.contains(&BookFormat::Ebook) && author.quality_profile_id.is_none() {
            let p = match req.ebook_profile_id {
                Some(p) => Some(p),
                None => self.first_content_profile(ContentKind::Ebook).await?,
            };
            edit.quality_profile_id = Some(p);
            if author.root_folder_id.is_none() {
                edit.root_folder_id = Some(match req.ebook_root_folder_id {
                    Some(r) => Some(r),
                    None => self.first_content_root(ContentKind::Ebook).await?,
                });
            }
            changed = true;
        }
        if req.formats.contains(&BookFormat::Audiobook) && author.audio_quality_profile_id.is_none() {
            let p = match req.audiobook_profile_id {
                Some(p) => Some(p),
                None => self.first_content_profile(ContentKind::Audiobook).await?,
            };
            edit.audio_quality_profile_id = Some(p);
            if author.audio_root_folder_id.is_none() {
                edit.audio_root_folder_id = Some(match req.audiobook_root_folder_id {
                    Some(r) => Some(r),
                    None => self.first_content_root(ContentKind::Audiobook).await?,
                });
            }
            changed = true;
        }
        if changed {
            self.books
                .update_author(author_id, edit)
                .await
                .map_err(|e| anyhow!("{e}"))?;
        }
        Ok(())
    }
}

fn family(kind: ContentKind) -> QualityFamily {
    match kind {
        ContentKind::Ebook => QualityFamily::Ebook,
        ContentKind::Audiobook | ContentKind::Radio => QualityFamily::Audiobook,
        ContentKind::Music => QualityFamily::Music,
        ContentKind::Movie | ContentKind::Series => QualityFamily::Video,
    }
}

fn album_type_name(t: AlbumType) -> &'static str {
    match t {
        AlbumType::Album => "Album",
        AlbumType::Ep => "EP",
        AlbumType::Single => "Single",
        AlbumType::Broadcast => "Broadcast",
        AlbumType::Other => "Other",
    }
}

fn is_not_found(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("not found") || e.contains("404")
}

#[async_trait]
impl LibraryGateway for LibraryAdapter {
    async fn availability(
        &self,
        kind: MediaKind,
        tmdb_ids: &[u32],
    ) -> anyhow::Result<HashMap<u32, Availability>> {
        let map = match kind {
            MediaKind::Movie => self.library.movie_availability(tmdb_ids).await,
            MediaKind::Series => self.library.series_availability(tmdb_ids).await,
        }
        .map_err(|e| anyhow!("{e}"))?;
        Ok(map.into_iter().map(|(id, a)| (id, availability(a))).collect())
    }

    async fn add_movie(&self, req: AddMovieRequest) -> anyhow::Result<i64> {
        // Already in the library (e.g. unmonitored): reuse it rather than failing on a duplicate.
        let existing = self
            .library
            .movie_availability(&[req.tmdb_id])
            .await
            .map_err(|e| anyhow!("{e}"))?;
        if let Some(id) = existing.get(&req.tmdb_id).and_then(|a| a.library_id) {
            return Ok(id);
        }
        let minimum_availability = match req.minimum_availability.as_str() {
            "announced" => MinimumAvailability::Announced,
            "in_cinemas" | "incinemas" => MinimumAvailability::InCinemas,
            _ => MinimumAvailability::Released,
        };
        let movie = self
            .library
            .add_movie(LibAddMovie {
                tmdb_id: req.tmdb_id,
                quality_profile_id: self.default_profile(req.quality_profile_id).await?,
                root_folder_id: self.default_root(MediaKind::Movie, req.root_folder_id).await?,
                monitor: Default::default(),
                minimum_availability,
                tags: req.tags,
                search_on_add: false,
                path: None,
                added_by_request_id: Some(req.request_id),
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(movie.id)
    }

    async fn add_series(&self, req: AddSeriesRequest) -> anyhow::Result<i64> {
        // Existing series: monitor the newly requested seasons instead of adding again.
        if let Some(series) = self
            .library
            .series_by_tmdb(req.tmdb_id)
            .await
            .map_err(|e| anyhow!("{e}"))?
        {
            for season_number in &req.seasons {
                self.library
                    .monitor_season(SeasonMonitorRequest {
                        series_id: series.id,
                        season_number: *season_number,
                        monitored: true,
                        include_episodes: true,
                    })
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
            }
            return Ok(series.id);
        }
        let series_type = match req.series_type.as_str() {
            "daily" => SeriesType::Daily,
            "anime" => SeriesType::Anime,
            _ => SeriesType::Standard,
        };
        let series = self
            .library
            .add_series(LibAddSeries {
                tmdb_id: Some(req.tmdb_id),
                tvdb_id: req.tvdb_id,
                quality_profile_id: self.default_profile(req.quality_profile_id).await?,
                root_folder_id: self.default_root(MediaKind::Series, req.root_folder_id).await?,
                monitor: Default::default(),
                seasons: (!req.seasons.is_empty()).then_some(req.seasons),
                series_type,
                season_folder: req.season_folder,
                monitor_new_items: Default::default(),
                tags: req.tags,
                search_on_add: false,
                search_cutoff_unmet: false,
                path: None,
                added_by_request_id: Some(req.request_id),
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(series.id)
    }

    async fn request_search(&self, kind: MediaKind, ids: &[i64]) -> anyhow::Result<()> {
        let kind = match kind {
            MediaKind::Movie => LibrarySearchKind::Movie,
            MediaKind::Series => LibrarySearchKind::Series,
        };
        self.library
            .request_search(SearchRequest {
                kind,
                ids: ids.to_vec(),
                season: None,
                media: None,
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    async fn root_folders(&self, kind: MediaKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
        let roots = self.library.root_folders().await.map_err(|e| anyhow!("{e}"))?;
        Ok(roots
            .into_iter()
            .filter(|r| r.media_kind == kind)
            .map(|r| RootFolderOptionDto {
                id: r.id,
                path: r.path,
                free_space: r.free_space,
            })
            .collect())
    }

    // ---------------------------------------------------------------- books / music

    async fn content_availability(
        &self,
        kind: ContentRequestKind,
        ids: &[String],
    ) -> anyhow::Result<HashMap<String, Availability>> {
        if kind == ContentRequestKind::Radio {
            // A followed show is in the library (there is nothing to download for it as such).
            let keys: Vec<(String, String)> = ids
                .iter()
                .filter_map(|id| split_radio_foreign_id(id))
                .map(|(p, e)| (p.to_owned(), e.to_owned()))
                .collect();
            let followed = self
                .radio
                .followed_shows(&keys)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            return Ok(followed
                .into_iter()
                .map(|((p, e), id)| {
                    (
                        reelix_api_types::requests::radio_foreign_id(&p, &e),
                        Availability {
                            status: AvailabilityStatus::Available,
                            library_id: Some(id),
                            ..Availability::default()
                        },
                    )
                })
                .collect());
        }
        let (map, column) = match kind {
            ContentRequestKind::Book => (
                self.books.availability(ids).await.map_err(|e| anyhow!("{e}"))?,
                "book_id",
            ),
            ContentRequestKind::Music => (
                self.music.availability(ids).await.map_err(|e| anyhow!("{e}"))?,
                "album_id",
            ),
            ContentRequestKind::Radio => unreachable!("handled above"),
        };
        let mut out: HashMap<String, Availability> =
            map.into_iter().map(|(id, a)| (id, availability(a))).collect();
        // Monitored items with a download in progress are "processing".
        let monitored: Vec<i64> = out
            .values()
            .filter(|a| a.status == AvailabilityStatus::Monitored)
            .filter_map(|a| a.library_id)
            .collect();
        let active = self.downloading(column, &monitored).await?;
        for a in out.values_mut() {
            if a.status == AvailabilityStatus::Monitored
                && a.library_id.is_some_and(|id| active.contains(&id))
            {
                a.status = AvailabilityStatus::Processing;
            }
        }
        Ok(out)
    }

    async fn lookup_books(&self, term: &str) -> anyhow::Result<Vec<ContentHit>> {
        let hits = self.books.lookup_books(term).await.map_err(|e| anyhow!("{e}"))?;
        Ok(hits
            .into_iter()
            .map(|h| ContentHit {
                foreign_id: h.openlibrary_work_id,
                title: h.title,
                subtitle: h.author_name,
                subtitle_id: h.author_openlibrary_id,
                year: h.year,
                cover: h.cover,
                album_type: None,
                overview: None,
                in_library: None,
            })
            .collect())
    }

    async fn lookup_albums(&self, term: &str) -> anyhow::Result<Vec<ContentHit>> {
        let hits = self.music.lookup_albums(term).await.map_err(|e| anyhow!("{e}"))?;
        Ok(hits
            .into_iter()
            .map(|h| ContentHit {
                foreign_id: h.musicbrainz_release_group_id,
                title: h.title,
                subtitle: Some(h.artist_name).filter(|n| !n.is_empty()),
                subtitle_id: Some(h.artist_musicbrainz_id).filter(|n| !n.is_empty()),
                year: h.year,
                cover: h.cover,
                album_type: Some(album_type_name(h.album_type).to_string()),
                overview: None,
                in_library: None,
            })
            .collect())
    }

    async fn discover_content(&self, kind: ContentRequestKind, row: &str) -> anyhow::Result<Vec<ContentHit>> {
        use reelix_metadata::StatsRange;
        const ROW_SIZE: usize = 40;
        match kind {
            ContentRequestKind::Book => {
                let works = match row.strip_prefix("subject-") {
                    Some(subject) => self.open_library.subject_works(subject, ROW_SIZE).await?,
                    None if row == "trending" => self.open_library.trending_works(ROW_SIZE).await?,
                    None => anyhow::bail!("unknown book discover row: {row}"),
                };
                Ok(works
                    .into_iter()
                    .map(|w| ContentHit {
                        foreign_id: w.id,
                        title: w.title,
                        subtitle: w.author_names.into_iter().next(),
                        subtitle_id: w.author_ids.into_iter().next(),
                        year: w.first_publish_year,
                        cover: w.cover_url,
                        album_type: None,
                        overview: None,
                        in_library: None,
                    })
                    .collect())
            }
            ContentRequestKind::Music => {
                let albums = match row {
                    "new-releases" => self.listenbrainz.new_releases(14, ROW_SIZE).await?,
                    "popular-week" => {
                        self.listenbrainz
                            .top_release_groups(StatsRange::Week, ROW_SIZE)
                            .await?
                    }
                    "popular-month" => {
                        self.listenbrainz
                            .top_release_groups(StatsRange::Month, ROW_SIZE)
                            .await?
                    }
                    _ => anyhow::bail!("unknown music discover row: {row}"),
                };
                Ok(albums
                    .into_iter()
                    .map(|a| ContentHit {
                        foreign_id: a.release_group_mbid,
                        title: a.title,
                        subtitle: Some(a.artist_name).filter(|n| !n.is_empty()),
                        subtitle_id: a.artist_mbid,
                        year: a.year,
                        cover: a.cover_url,
                        album_type: a.album_type,
                        overview: None,
                        in_library: None,
                    })
                    .collect())
            }
            ContentRequestKind::Radio => Ok(self
                .radio_discover(row)
                .await?
                .iter()
                .map(ContentHit::radio)
                .collect()),
        }
    }

    async fn radio_lists(&self) -> anyhow::Result<RadioDiscoverDto> {
        self.radio.discover_lists().await.map_err(|e| anyhow!("{e}"))
    }

    async fn radio_discover(&self, row: &str) -> anyhow::Result<Vec<RadioShowHitDto>> {
        self.radio.discover(row).await.map_err(|e| anyhow!("{e}"))
    }

    async fn lookup_radio(&self, term: &str) -> anyhow::Result<Vec<RadioShowHitDto>> {
        self.radio.search(term, None).await.map_err(|e| anyhow!("{e}"))
    }

    async fn radio_preview(
        &self,
        provider: &str,
        external_id: &str,
    ) -> anyhow::Result<Option<RadioShowPreviewDto>> {
        match self.radio.preview_show(provider, external_id).await {
            Ok(p) => Ok(Some(p)),
            Err(e) if radio_missing(&e) => Ok(None),
            Err(e) => Err(anyhow!("{e}")),
        }
    }

    /// Follows the show with the radio settings' default follow choice; an already followed
    /// show (also under another id, e.g. its feed) is returned as it is.
    async fn add_radio(&self, req: AddRadioRequest) -> anyhow::Result<ContentAdded> {
        let key = (req.provider.clone(), req.external_id.clone());
        let added = self
            .radio
            .add_show(AddRadioShowRequest {
                provider: Some(req.provider),
                external_id: Some(req.external_id),
                link: None,
                follow: None,
                monitored: true,
                root_folder_id: req.root_folder_id,
                quality_profile_id: req.quality_profile_id,
                tags: req.tags,
            })
            .await;
        let id = match added {
            Ok(show) => show.id,
            Err(reelix_radio::RadioError::Conflict(msg)) => self
                .radio
                .followed_shows(std::slice::from_ref(&key))
                .await
                .map_err(|e| anyhow!("{e}"))?
                .remove(&key)
                .ok_or_else(|| anyhow!("{msg}"))?,
            Err(e) => return Err(anyhow!("{e}")),
        };
        Ok(ContentAdded {
            item_id: id,
            parent_id: None,
        })
    }

    async fn lookup_artists(&self, term: &str) -> anyhow::Result<Vec<ArtistHit>> {
        let hits = self
            .music
            .lookup_artists(term)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(hits
            .into_iter()
            .map(|h| ArtistHit {
                musicbrainz_id: h.musicbrainz_id,
                name: h.name,
                disambiguation: h.disambiguation,
                artist_type: h.artist_type,
                country: h.country,
                library_id: h.artist_id,
            })
            .collect())
    }

    async fn content(&self, kind: ContentRequestKind, id: &str) -> anyhow::Result<Option<ContentHit>> {
        let id = id.trim();
        match kind {
            ContentRequestKind::Book => {
                // Only work ids (`OL…W`) are accepted here; other terms would be searches.
                let is_work = id.len() > 3
                    && id.starts_with("OL")
                    && id.ends_with('W')
                    && id[2..id.len() - 1].chars().all(|c| c.is_ascii_digit());
                if !is_work {
                    return Ok(None);
                }
                let mut hit = match self.lookup_books(&format!("olid:{id}")).await {
                    Ok(mut hits) if !hits.is_empty() => hits.remove(0),
                    Ok(_) => return Ok(None),
                    Err(e) if is_not_found(&e.to_string()) => return Ok(None),
                    Err(e) => return Err(e),
                };
                if let Ok(Some(b)) = self.books.book_by_work(id).await {
                    hit.overview = b.overview;
                }
                Ok(Some(hit))
            }
            ContentRequestKind::Music => {
                let is_mbid = id.len() == 36
                    && id.chars().enumerate().all(|(i, c)| {
                        if [8, 13, 18, 23].contains(&i) {
                            c == '-'
                        } else {
                            c.is_ascii_hexdigit()
                        }
                    });
                if !is_mbid {
                    return Ok(None);
                }
                match self.lookup_albums(id).await {
                    Ok(mut hits) if !hits.is_empty() => Ok(Some(hits.remove(0))),
                    Ok(_) => Ok(None),
                    Err(e) if is_not_found(&e.to_string()) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            ContentRequestKind::Radio => {
                let Some((provider, external_id)) = split_radio_foreign_id(id) else {
                    return Ok(None);
                };
                Ok(self.radio_preview(provider, external_id).await?.map(|p| {
                    let mut hit = ContentHit::radio(&p.show);
                    hit.foreign_id = id.to_owned();
                    hit
                }))
            }
        }
    }

    async fn add_book(&self, req: AddBookRequest) -> anyhow::Result<ContentAdded> {
        let wants = |f: BookFormat| req.formats.is_empty() || req.formats.contains(&f);
        // Already in the library (e.g. added with its author but unmonitored): monitor the
        // requested formats instead of adding again.
        let existing = self
            .books
            .book_by_work(&req.work_id)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let book = match existing {
            Some(b) => b,
            None => {
                let ebook_profile = match req.ebook_profile_id {
                    Some(p) => Some(p),
                    None if wants(BookFormat::Ebook) => {
                        self.first_content_profile(ContentKind::Ebook).await?
                    }
                    None => None,
                };
                let audio_profile = match req.audiobook_profile_id {
                    Some(p) => Some(p),
                    None if wants(BookFormat::Audiobook) => {
                        self.first_content_profile(ContentKind::Audiobook).await?
                    }
                    None => None,
                };
                let ebook_root = match req.ebook_root_folder_id {
                    Some(r) => Some(r),
                    None if wants(BookFormat::Ebook) => self.first_content_root(ContentKind::Ebook).await?,
                    None => None,
                };
                let audio_root = match req.audiobook_root_folder_id {
                    Some(r) => Some(r),
                    None if wants(BookFormat::Audiobook) => {
                        self.first_content_root(ContentKind::Audiobook).await?
                    }
                    None => None,
                };
                if ebook_root.is_none() && audio_root.is_none() {
                    anyhow::bail!("no book root folder exists; add one under Settings → Media Management");
                }
                self.books
                    .add_book(LibAddBook {
                        openlibrary_work_id: req.work_id.clone(),
                        formats: req.formats.clone(),
                        quality_profile_id: ebook_profile,
                        root_folder_id: ebook_root,
                        audio_quality_profile_id: audio_profile,
                        audio_root_folder_id: audio_root,
                        tags: req.tags.clone(),
                        search_on_add: false,
                        added_by_request_id: Some(req.request_id),
                    })
                    .await
                    .map_err(|e| anyhow!("{e}"))?
            }
        };
        let missing = (wants(BookFormat::Ebook) && !book.monitored_ebook)
            || (wants(BookFormat::Audiobook) && !book.monitored_audiobook);
        if missing {
            // The author may lack a profile for a requested format.
            self.ensure_author_formats(book.author_id, &req).await?;
            self.books
                .update_book(
                    book.id,
                    BookEdit {
                        monitored_ebook: wants(BookFormat::Ebook).then_some(true),
                        monitored_audiobook: wants(BookFormat::Audiobook).then_some(true),
                        preferred_edition_id: None,
                    },
                )
                .await
                .map_err(|e| anyhow!("{e}"))?;
        }
        Ok(ContentAdded {
            item_id: book.id,
            parent_id: Some(book.author_id),
        })
    }

    async fn add_album(&self, req: AddAlbumRequest) -> anyhow::Result<ContentAdded> {
        if let Some(a) = self
            .music
            .album_by_mbid(&req.release_group_id)
            .await
            .map_err(|e| anyhow!("{e}"))?
            .filter(|a| a.monitored)
        {
            return Ok(ContentAdded {
                item_id: a.id,
                parent_id: Some(a.artist_id),
            });
        }
        let album = self
            .music
            .add_album(LibAddAlbum {
                musicbrainz_release_group_id: req.release_group_id.clone(),
                quality_profile_id: req.quality_profile_id,
                metadata_profile_id: req.metadata_profile_id,
                root_folder_id: req.root_folder_id,
                tags: req.tags.clone(),
                search_on_add: false,
                added_by_request_id: Some(req.request_id),
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(ContentAdded {
            item_id: album.id,
            parent_id: Some(album.artist_id),
        })
    }

    async fn content_search(
        &self,
        kind: ContentRequestKind,
        ids: &[i64],
        formats: &[BookFormat],
    ) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        match kind {
            ContentRequestKind::Book => {
                // One search per requested format (or every monitored format).
                let formats: Vec<Option<BookFormat>> =
                    if formats.is_empty() || formats.len() == BookFormat::ALL.len() {
                        vec![None]
                    } else {
                        formats.iter().copied().map(Some).collect()
                    };
                for format in formats {
                    self.books
                        .request_search(BookSearchRequest {
                            kind: ContentSearchKind::Item,
                            ids: ids.to_vec(),
                            format,
                        })
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                }
            }
            ContentRequestKind::Music => {
                self.music
                    .request_search(AlbumSearchRequest {
                        kind: ContentSearchKind::Item,
                        ids: ids.to_vec(),
                    })
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
            }
            ContentRequestKind::Radio => {
                // The shows' wanted episodes (none when the follow choice is "from now on").
                self.radio
                    .request_search(RadioSearchRequest {
                        kind: ContentSearchKind::Parent,
                        ids: ids.to_vec(),
                    })
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
            }
        }
        Ok(())
    }

    async fn content_profiles(&self, kind: ContentKind) -> anyhow::Result<Vec<NamedIdDto>> {
        if !matches!(
            kind,
            ContentKind::Ebook | ContentKind::Audiobook | ContentKind::Music | ContentKind::Radio
        ) {
            return Ok(Vec::new());
        }
        let profiles = self
            .db
            .quality_profiles()
            .list_family(family(kind))
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(profiles
            .into_iter()
            .map(|p| NamedIdDto {
                id: p.id,
                name: p.name,
            })
            .collect())
    }

    async fn content_root_folders(&self, kind: ContentKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
        if !matches!(
            kind,
            ContentKind::Ebook | ContentKind::Audiobook | ContentKind::Music | ContentKind::Radio
        ) {
            return Ok(Vec::new());
        }
        self.content_roots(kind).await
    }
}
