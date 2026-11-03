//! Movies: list/get/add/edit/delete, lookup and files.

use std::cmp::Ordering;
use std::path::Path;

use reelix_api_types::library::{
    AddMovieRequest, BulkDeleteRequest, CastDto, LookupResult, MovieBulkEdit, MovieDto, MovieEdit,
    MovieFileDto, MovieListQuery, MovieMonitor, Paged, SortDir, TagMode,
};
use reelix_core::{ContentKind, MediaKind};
use reelix_db::NewJob;
use reelix_import::naming::MovieNaming;
use reelix_metadata::{ExternalSource, MovieMetadata, SearchHit};

use crate::content_roots;
use crate::convert::{images, movie_status, ratings, year_u16};
use crate::error::{LibraryError, Result};
use crate::repo::files::{self, FileTable};
use crate::repo::misc::{self, CollectionPart, CollectionRow};
use crate::repo::movies;
use crate::{LibraryService, kinds};

/// Slices `items` into a page (`page_size` 0 = all).
pub(crate) fn paginate<T>(items: Vec<T>, page: Option<u32>, page_size: Option<u32>) -> Paged<T> {
    let total = items.len() as u64;
    let page = page.unwrap_or(1).max(1);
    let size = page_size.unwrap_or(0);
    let items = if size == 0 {
        items
    } else {
        items
            .into_iter()
            .skip(((page - 1) as usize).saturating_mul(size as usize))
            .take(size as usize)
            .collect()
    };
    Paged {
        items,
        total,
        page: if size == 0 { 1 } else { page },
        page_size: size,
    }
}

pub(crate) fn apply_tags(current: &mut Vec<i64>, tags: &[i64], mode: TagMode) {
    match mode {
        TagMode::Replace => *current = tags.to_vec(),
        TagMode::Add => {
            for t in tags {
                if !current.contains(t) {
                    current.push(*t);
                }
            }
        }
        TagMode::Remove => current.retain(|t| !tags.contains(t)),
    }
    current.sort_unstable();
    current.dedup();
}

/// Copies TMDB metadata onto a movie (everything except user settings).
pub(crate) fn apply_movie_metadata(m: &mut MovieDto, meta: &MovieMetadata, today: chrono::NaiveDate) {
    m.tmdb_id = meta.tmdb_id;
    m.imdb_id = meta
        .imdb_id
        .clone()
        .or(meta.external_ids.imdb.clone())
        .or(m.imdb_id.take());
    m.title = meta.title.clone();
    m.original_title = Some(meta.original_title.clone()).filter(|t| !t.is_empty() && *t != meta.title);
    m.sort_title = if meta.sort_title.is_empty() {
        reelix_metadata::sort_title(&meta.title)
    } else {
        meta.sort_title.clone()
    };
    m.year = meta.year;
    m.overview = meta.overview.clone();
    m.tagline = meta.tagline.clone();
    m.status = movie_status(meta, today);
    m.runtime_minutes = meta.runtime_minutes.filter(|r| *r > 0);
    m.studio = meta.studios.first().map(|s| s.name.clone());
    m.genres = meta.genres.iter().map(|g| g.name.clone()).collect();
    m.certification = meta.certification.clone();
    m.original_language = meta.original_language.clone();
    m.images = images(&meta.images);
    m.ratings = ratings(&meta.ratings);
    m.in_cinemas = meta.release_dates.cinema.or(meta.release_date);
    m.digital_release = meta.release_dates.digital;
    m.physical_release = meta.release_dates.physical;
    m.youtube_trailer = meta
        .videos
        .iter()
        .filter(|v| v.site.eq_ignore_ascii_case("youtube"))
        .find(|v| v.kind == "Trailer")
        .or_else(|| {
            meta.videos
                .iter()
                .find(|v| v.site.eq_ignore_ascii_case("youtube"))
        })
        .map(|v| v.key.clone());
    m.homepage = meta.homepage.clone().filter(|h| !h.is_empty());
    m.is_available = crate::convert::is_available(m.status, m.minimum_availability);
    m.alternative_titles = meta.alternative_titles.iter().map(|a| a.title.clone()).collect();
}

pub(crate) fn cast_of(credits: &reelix_metadata::Credits) -> Vec<CastDto> {
    credits
        .cast
        .iter()
        .take(20)
        .map(|c| CastDto {
            tmdb_id: c.tmdb_id,
            name: c.name.clone(),
            character: c.character.clone(),
            profile_url: c.profile_url.clone(),
        })
        .collect()
}

pub(crate) fn movie_naming(m: &MovieDto) -> MovieNaming {
    MovieNaming {
        title: m.title.clone(),
        original_title: m.original_title.clone(),
        year: year_u16(m.year),
        tmdb_id: Some(m.tmdb_id),
        imdb_id: m.imdb_id.clone(),
        collection: m.collection.as_ref().map(|c| c.title.clone()),
        certification: m.certification.clone(),
    }
}

pub(crate) fn hit_to_lookup(h: &SearchHit, library_id: Option<i64>) -> LookupResult {
    LookupResult {
        kind: h.kind,
        tmdb_id: h.tmdb_id,
        tvdb_id: h.tvdb_id,
        imdb_id: h.imdb_id.clone(),
        title: h.title.clone(),
        original_title: h.original_title.clone(),
        year: h.year,
        overview: h.overview.clone(),
        poster: h.poster.clone(),
        backdrop: h.backdrop.clone(),
        vote_average: h.vote_average,
        popularity: h.popularity,
        in_library: library_id.is_some(),
        library_id,
    }
}

fn blank_movie() -> MovieDto {
    MovieDto {
        id: 0,
        tmdb_id: 0,
        imdb_id: None,
        title: String::new(),
        original_title: None,
        sort_title: String::new(),
        year: None,
        overview: None,
        tagline: None,
        status: reelix_api_types::library::MovieStatus::Tba,
        runtime_minutes: None,
        studio: None,
        genres: Vec::new(),
        certification: None,
        original_language: None,
        images: Default::default(),
        ratings: Default::default(),
        alternative_titles: Vec::new(),
        in_cinemas: None,
        digital_release: None,
        physical_release: None,
        youtube_trailer: None,
        homepage: None,
        monitored: true,
        minimum_availability: Default::default(),
        is_available: false,
        quality_profile_id: 0,
        root_folder_id: 0,
        path: String::new(),
        collection: None,
        tags: Vec::new(),
        added_by_request_id: None,
        has_file: false,
        size_on_disk: 0,
        movie_file: None,
        cast: Vec::new(),
        added: chrono::Utc::now(),
        last_refreshed: None,
    }
}

impl LibraryService {
    pub async fn list_movies(&self, q: &MovieListQuery) -> Result<Paged<MovieDto>> {
        let profiles = self.profiles().await?;
        // Filter, sort and page on light rows; only the page returned is fully loaded. The
        // quality filter needs file details, so it loads everything.
        let light = q.quality.is_none();
        let mut items = if light {
            movies::load_light(&self.db).await?
        } else {
            movies::load(&self.db, None, &profiles).await?
        };
        let needle = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
        items.retain(|m| {
            needle.as_ref().is_none_or(|n| {
                m.title.to_lowercase().contains(n)
                    || m.original_title
                        .as_ref()
                        .is_some_and(|t| t.to_lowercase().contains(n))
                    || m.alternative_titles.iter().any(|t| t.to_lowercase().contains(n))
            }) && q.monitored.is_none_or(|v| m.monitored == v)
                && q.has_file.is_none_or(|v| m.has_file == v)
                && q.missing
                    .is_none_or(|v| (m.monitored && m.is_available && !m.has_file) == v)
                && q.quality_profile_id.is_none_or(|v| m.quality_profile_id == v)
                && q.quality.is_none_or(|v| {
                    m.movie_file
                        .as_ref()
                        .is_some_and(|f| f.file.quality.quality.id() == v)
                })
                && q.tag.is_none_or(|t| m.tags.contains(&t))
                && q.genre
                    .as_ref()
                    .is_none_or(|g| m.genres.iter().any(|x| x.eq_ignore_ascii_case(g)))
                && q.year.is_none_or(|y| m.year == Some(y))
                && q.status.is_none_or(|s| m.status == s)
                && q.collection_id
                    .is_none_or(|c| m.collection.as_ref().is_some_and(|x| x.id == c))
        });
        let sort = q.sort.as_deref().unwrap_or("title");
        items.sort_by(|a, b| {
            let o = match sort {
                "year" => a.year.cmp(&b.year),
                "added" => a.added.cmp(&b.added),
                "size" => a.size_on_disk.cmp(&b.size_on_disk),
                "release" => a
                    .digital_release
                    .or(a.physical_release)
                    .or(a.in_cinemas)
                    .cmp(&b.digital_release.or(b.physical_release).or(b.in_cinemas)),
                "status" => crate::convert::enum_str(&a.status).cmp(&crate::convert::enum_str(&b.status)),
                "rating" => a
                    .ratings
                    .vote_average
                    .partial_cmp(&b.ratings.vote_average)
                    .unwrap_or(Ordering::Equal),
                "runtime" => a.runtime_minutes.cmp(&b.runtime_minutes),
                _ => Ordering::Equal,
            };
            o.then_with(|| a.sort_title.cmp(&b.sort_title))
        });
        if q.dir == Some(SortDir::Desc) {
            items.reverse();
        }
        let mut page = paginate(items, q.page, q.page_size);
        if light {
            page.items = movies::hydrate(&self.db, page.items, &profiles).await?;
        }
        Ok(page)
    }

    /// One movie including cast.
    pub async fn get_movie(&self, id: i64) -> Result<MovieDto> {
        let profiles = self.profiles().await?;
        let mut m = movies::get(&self.db, id, &profiles)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", id))?;
        m.cast = misc::credits(&self.db, MediaKind::Movie, id).await?;
        Ok(m)
    }

    pub async fn movie_by_tmdb(&self, tmdb_id: u32) -> Result<Option<MovieDto>> {
        match movies::id_by_tmdb(&self.db, tmdb_id).await? {
            Some(id) => Ok(Some(self.get_movie(id).await?)),
            None => Ok(None),
        }
    }

    /// Adds a movie from TMDB; creates its collection, queues a disk scan and (optionally) a search.
    pub async fn add_movie(&self, req: AddMovieRequest) -> Result<MovieDto> {
        self.add_movie_inner(req, true).await
    }

    /// [`Self::add_movie`]; with `scan` false the caller queues the disk scan itself.
    pub(crate) async fn add_movie_inner(&self, req: AddMovieRequest, scan: bool) -> Result<MovieDto> {
        self.check_profile(req.quality_profile_id).await?;
        let root = self.require_root(req.root_folder_id, MediaKind::Movie).await?;
        if movies::id_by_tmdb(&self.db, req.tmdb_id).await?.is_some() {
            return Err(LibraryError::Conflict(format!(
                "movie with TMDB id {} is already in the library",
                req.tmdb_id
            )));
        }
        let meta = self.metadata.movie(req.tmdb_id).await?;
        let mut m = blank_movie();
        m.minimum_availability = req.minimum_availability;
        apply_movie_metadata(&mut m, &meta, self.today());
        m.monitored = req.monitor != MovieMonitor::None;
        m.quality_profile_id = req.quality_profile_id;
        m.root_folder_id = root.id;
        m.tags = req.tags.clone();
        m.tags.sort_unstable();
        m.tags.dedup();
        m.added_by_request_id = req.added_by_request_id;
        m.last_refreshed = Some(chrono::Utc::now());

        if let Some(c) = &meta.collection {
            let row = CollectionRow {
                id: 0,
                tmdb_id: c.tmdb_id,
                title: c.name.clone(),
                overview: None,
                images: reelix_api_types::library::ImagesDto {
                    poster: c.poster.clone(),
                    backdrop: c.backdrop.clone(),
                    logo: None,
                },
                parts: Vec::<CollectionPart>::new(),
                monitored: req.monitor == MovieMonitor::MovieAndCollection,
                quality_profile_id: req.quality_profile_id,
                root_folder_id: root.id,
                minimum_availability: req.minimum_availability,
                search_on_add: req.search_on_add,
                tags: m.tags.clone(),
                last_synced: None,
            };
            let cid = misc::upsert_collection(&self.db, &row).await?;
            if req.monitor == MovieMonitor::MovieAndCollection {
                misc::update_collection(
                    &self.db,
                    cid,
                    &reelix_api_types::library::CollectionEdit {
                        monitored: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
            }
            m.collection = Some(reelix_api_types::library::CollectionRefDto {
                id: cid,
                tmdb_id: c.tmdb_id,
                title: c.name.clone(),
            });
        }

        m.path = match req.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => {
                let p = crate::root_folders::normalize(p);
                content_roots::ensure_inside(&p, [root.path.as_str()])?;
                p
            }
            None => {
                let folder = self.naming_config().await?.movie_folder(&movie_naming(&m))?;
                Path::new(&root.path).join(folder).to_string_lossy().into_owned()
            }
        };
        let id = movies::insert(&self.db, &m).await?;
        let alts: Vec<(String, Option<String>)> = meta
            .alternative_titles
            .iter()
            .map(|a| (a.title.clone(), a.country.clone()))
            .collect();
        misc::set_alt_titles(&self.db, MediaKind::Movie, id, &alts).await?;
        misc::set_credits(&self.db, MediaKind::Movie, id, &cast_of(&meta.credits)).await?;
        self.emit("movie", "created", id);
        tracing::info!(id, tmdb_id = req.tmdb_id, title = %m.title, "movie added");

        if scan {
            self.queue_scan(&[id], &[]).await?;
        }
        if let (Some(c), MovieMonitor::MovieAndCollection) = (&m.collection, req.monitor) {
            self.enqueue(
                NewJob::new(
                    kinds::COLLECTION_SYNC,
                    serde_json::json!({ "collection_ids": [c.id] }),
                )
                .dedupe(format!("collection_sync:{}", c.id)),
            )
            .await?;
        }
        if req.search_on_add && m.monitored {
            self.request_search(reelix_api_types::library::SearchRequest {
                kind: reelix_api_types::library::LibrarySearchKind::Movie,
                ids: vec![id],
                season: None,
                media: None,
            })
            .await?;
        }
        self.get_movie(id).await
    }

    pub async fn update_movie(&self, id: i64, edit: MovieEdit) -> Result<MovieDto> {
        self.edit_movies(&[id], &edit, true).await?;
        self.get_movie(id).await
    }

    pub async fn bulk_update_movies(&self, req: MovieBulkEdit) -> Result<Vec<MovieDto>> {
        if req.ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut edit = req.edit;
        edit.path = None;
        self.edit_movies(&req.ids, &edit, false).await?;
        let profiles = self.profiles().await?;
        movies::load(&self.db, Some(&req.ids), &profiles).await
    }

    async fn edit_movies(&self, ids: &[i64], e: &MovieEdit, single: bool) -> Result<()> {
        if let Some(p) = e.quality_profile_id {
            self.check_profile(p).await?;
        }
        let new_root = match e.root_folder_id {
            Some(r) => Some(self.require_root(r, MediaKind::Movie).await?),
            None => None,
        };
        let profiles = self.profiles().await?;
        let list = movies::load(&self.db, Some(ids), &profiles).await?;
        if single && list.is_empty() {
            return Err(LibraryError::not_found("movie", ids[0]));
        }
        for mut m in list {
            let old_path = m.path.clone();
            if let Some(v) = e.monitored {
                m.monitored = v;
            }
            if let Some(v) = e.quality_profile_id {
                m.quality_profile_id = v;
            }
            if let Some(v) = e.minimum_availability {
                m.minimum_availability = v;
            }
            if let Some(r) = &new_root {
                m.root_folder_id = r.id;
                let name = Path::new(&old_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| m.title.clone());
                m.path = Path::new(&r.path).join(name).to_string_lossy().into_owned();
            }
            if let Some(p) = e.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                if !Path::new(p).is_absolute() {
                    return Err(LibraryError::validation("path must be absolute"));
                }
                m.path = crate::root_folders::normalize(p);
            }
            if let Some(t) = &e.tags {
                apply_tags(&mut m.tags, t, e.tag_mode);
            }
            if m.path != old_path {
                content_roots::ensure_inside_kinds(&self.db, &m.path, &[ContentKind::Movie]).await?;
            }
            if e.move_files && m.path != old_path {
                self.move_media_folder(Path::new(&old_path), Path::new(&m.path))
                    .await?;
            }
            movies::save(&self.db, m.id, &m).await?;
            self.emit("movie", "updated", m.id);
        }
        Ok(())
    }

    /// Moves a movie/series folder to `to` (inside the root folders).
    pub(crate) async fn move_media_folder(&self, from: &Path, to: &Path) -> Result<()> {
        if !from.exists() {
            return Ok(());
        }
        let guard = self.path_guard().await?;
        guard.check_existing(from)?;
        guard.check(to)?;
        if to.exists() {
            return Err(LibraryError::Conflict(format!("{} already exists", to.display())));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(from, to).is_err() {
            // Cross-filesystem: move file by file, then remove the old tree.
            for f in reelix_import::files::scan(
                from,
                &reelix_import::files::ScanOptions {
                    include_non_video: true,
                    max_depth: 16,
                },
            )? {
                let dst = to.join(&f.relative);
                reelix_import::files::transfer(&f.path, &dst, reelix_import::files::TransferMode::Move)?;
            }
            let _ = std::fs::remove_dir_all(from);
        }
        tracing::info!(from = %from.display(), to = %to.display(), "moved media folder");
        Ok(())
    }

    pub async fn delete_movie(&self, id: i64, delete_files: bool) -> Result<()> {
        let profiles = self.profiles().await?;
        let m = movies::get(&self.db, id, &profiles)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", id))?;
        if delete_files {
            self.remove_path(Path::new(&m.path)).await?;
        }
        movies::delete(&self.db, id).await?;
        self.emit("movie", "deleted", id);
        tracing::info!(id, title = %m.title, delete_files, "movie deleted");
        Ok(())
    }

    pub async fn bulk_delete_movies(&self, req: BulkDeleteRequest) -> Result<u32> {
        let mut n = 0;
        for id in req.ids {
            match self.delete_movie(id, req.delete_files).await {
                Ok(()) => n += 1,
                Err(LibraryError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    /// TMDB search with in-library flags. `tmdb:<id>` / `imdb:<tt…>` look up an id.
    pub async fn lookup_movies(&self, term: &str) -> Result<Vec<LookupResult>> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let lower = term.to_ascii_lowercase();
        let hits: Vec<SearchHit> =
            if let Some(id) = lower.strip_prefix("tmdb:").and_then(|s| s.trim().parse().ok()) {
                let m = self.metadata.movie(id).await?;
                vec![movie_hit(&m)]
            } else if let Some(imdb) = lower
                .strip_prefix("imdb:")
                .map(str::trim)
                .or_else(|| lower.starts_with("tt").then_some(lower.as_str()))
                .filter(|s| s.len() > 2 && s[2..].chars().all(|c| c.is_ascii_digit()))
            {
                self.metadata
                    .find(ExternalSource::Imdb, imdb, MediaKind::Movie)
                    .await?
            } else {
                let (title, year) = split_year(term);
                let hits = self.metadata.search(&title, MediaKind::Movie, year).await?;
                if hits.is_empty() && year.is_some() {
                    self.metadata.search(term, MediaKind::Movie, None).await?
                } else {
                    hits
                }
            };
        let index = movies::tmdb_index(&self.db).await?;
        Ok(hits
            .iter()
            .map(|h| hit_to_lookup(h, h.tmdb_id.and_then(|t| index.get(&t).copied())))
            .collect())
    }

    pub async fn movie_files(&self, movie_id: i64) -> Result<Vec<MovieFileDto>> {
        let profiles = self.profiles().await?;
        let m = movies::get(&self.db, movie_id, &profiles)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", movie_id))?;
        let rows = files::list(&self.db, FileTable::Movie, Some(&[movie_id])).await?;
        let profile = profiles.profile(m.quality_profile_id);
        Ok(rows
            .iter()
            .map(|f| MovieFileDto {
                movie_id,
                file: f.to_dto(&m.path, profile, &profiles.format_names),
            })
            .collect())
    }

    /// Deletes a movie file from disk (recycle bin when configured) and the database.
    pub async fn delete_movie_file(&self, file_id: i64) -> Result<()> {
        let f = files::get(&self.db, FileTable::Movie, file_id)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie file", file_id))?;
        let path: Option<String> = sqlx::query_scalar("SELECT path FROM movies WHERE id = ?")
            .bind(f.owner_id)
            .fetch_optional(self.db.pool())
            .await?;
        if let Some(p) = path {
            self.remove_path(&Path::new(&p).join(&f.relative_path)).await?;
        }
        files::delete(&self.db, FileTable::Movie, file_id).await?;
        self.emit("movie_file", "deleted", file_id);
        self.emit("movie", "updated", f.owner_id);
        Ok(())
    }

    /// Queues a metadata refresh for one movie.
    pub async fn queue_refresh_movie(&self, id: i64) -> Result<Option<i64>> {
        if movies::get(&self.db, id, &self.profiles().await?)
            .await?
            .is_none()
        {
            return Err(LibraryError::not_found("movie", id));
        }
        self.enqueue(
            NewJob::new(
                kinds::REFRESH_MOVIES,
                serde_json::json!({ "movie_ids": [id], "manual": true }),
            )
            .priority(10)
            .dedupe(format!("refresh_movies:{id}")),
        )
        .await
    }

    /// Queues a disk scan of the given movies/series.
    pub async fn queue_scan(&self, movie_ids: &[i64], series_ids: &[i64]) -> Result<Option<i64>> {
        self.queue_import_scan(movie_ids, series_ids, false).await
    }

    /// [`Self::queue_scan`]; with `search_missing`, the job then searches for the monitored
    /// items the scan found no files for.
    pub(crate) async fn queue_import_scan(
        &self,
        movie_ids: &[i64],
        series_ids: &[i64],
        search_missing: bool,
    ) -> Result<Option<i64>> {
        let key = format!(
            "library_scan:{}:{}",
            movie_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","),
            series_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        self.enqueue(
            NewJob::new(
                kinds::LIBRARY_SCAN,
                serde_json::json!({
                    "movie_ids": movie_ids,
                    "series_ids": series_ids,
                    "search_missing": search_missing,
                }),
            )
            .priority(5)
            .dedupe(if search_missing {
                format!("{key}:search")
            } else {
                key
            }),
        )
        .await
    }

    pub async fn queue_rescan_movie(&self, id: i64) -> Result<Option<i64>> {
        if movies::get(&self.db, id, &self.profiles().await?)
            .await?
            .is_none()
        {
            return Err(LibraryError::not_found("movie", id));
        }
        self.queue_scan(&[id], &[]).await
    }
}

pub(crate) fn movie_hit(m: &MovieMetadata) -> SearchHit {
    SearchHit {
        kind: MediaKind::Movie,
        tmdb_id: Some(m.tmdb_id),
        tvdb_id: None,
        imdb_id: m.imdb_id.clone(),
        title: m.title.clone(),
        original_title: Some(m.original_title.clone()),
        year: m.year,
        release_date: m.release_date,
        overview: m.overview.clone(),
        poster: m.images.poster.clone(),
        backdrop: m.images.backdrop.clone(),
        genre_ids: m.genres.iter().map(|g| g.id).collect(),
        original_language: m.original_language.clone(),
        vote_average: m.ratings.vote_average,
        vote_count: m.ratings.vote_count,
        popularity: m.ratings.popularity,
        adult: m.adult,
    }
}

/// `"Heat (1995)"` / `"Heat 1995"` → (`"Heat"`, 1995).
pub(crate) fn split_year(term: &str) -> (String, Option<i32>) {
    let t = term.trim();
    let stripped = t.trim_end_matches(')');
    if let Some(idx) = stripped.rfind([' ', '(']) {
        let (head, tail) = stripped.split_at(idx);
        let tail = tail.trim_start_matches([' ', '(']);
        if tail.len() == 4
            && let Ok(y) = tail.parse::<i32>()
            && (1880..=2100).contains(&y)
            && !head.trim().is_empty()
        {
            return (head.trim().trim_end_matches('(').trim().to_owned(), Some(y));
        }
    }
    (t.to_owned(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_year_variants() {
        assert_eq!(split_year("Heat (1995)"), ("Heat".into(), Some(1995)));
        assert_eq!(split_year("Heat 1995"), ("Heat".into(), Some(1995)));
        assert_eq!(split_year("1917"), ("1917".into(), None));
        assert_eq!(
            split_year("Blade Runner 2049"),
            ("Blade Runner".into(), Some(2049))
        );
    }

    #[test]
    fn paginate_slices() {
        let p = paginate((1..=25).collect::<Vec<_>>(), Some(2), Some(10));
        assert_eq!(p.items, (11..=20).collect::<Vec<_>>());
        assert_eq!(p.total, 25);
        let all = paginate(vec![1, 2, 3], None, None);
        assert_eq!(all.items.len(), 3);
        assert_eq!(all.page_size, 0);
    }

    #[test]
    fn tag_modes() {
        let mut t = vec![1, 2];
        apply_tags(&mut t, &[3, 2], TagMode::Add);
        assert_eq!(t, vec![1, 2, 3]);
        apply_tags(&mut t, &[1], TagMode::Remove);
        assert_eq!(t, vec![2, 3]);
        apply_tags(&mut t, &[9], TagMode::Replace);
        assert_eq!(t, vec![9]);
    }
}
