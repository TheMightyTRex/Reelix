//! Films and series added with the owner's own details (RFX-90): no TMDB match, a typed title,
//! year, overview and picture, and a folder that already exists in a root folder. The rows are
//! ordinary movies and series with `custom = 1` (see migration 0020): refreshes leave them alone
//! ([`crate::refresh`]) and a series' scan makes its seasons and episodes from the file names
//! ([`LibraryService::custom_episodes_from_files`]).

use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use reelix_api_types::custom::{CustomDetails, CustomDetailsChange};
use reelix_api_types::library::{
    EpisodeDto, ImagesDto, MovieDto, MovieStatus, SeasonDto, SeriesDto, SeriesStatus,
};
use reelix_core::MediaKind;
use reelix_metadata::{ExternalSource, MetaError, SeriesExternalId};
use reelix_parser::{EpisodeInfo, ReleaseKind, parse_path};

use crate::LibraryService;
use crate::content_roots;
use crate::error::{LibraryError, Result};
use crate::movies::blank_movie;
use crate::repo::{movies, series as series_repo};
use crate::series::blank_series;

/// A custom item's folder, checked: absolute, inside `root`, and an existing folder. Returns
/// it normalised (no trailing separator).
pub fn custom_folder(root: &str, path: &str) -> Result<String> {
    let p = content_roots::normalize(path);
    if p.is_empty() {
        return Err(LibraryError::validation("choose a folder"));
    }
    content_roots::ensure_inside(&p, [root])?;
    if !Path::new(&p).is_dir() {
        return Err(LibraryError::validation(format!("{p} is not a folder")));
    }
    Ok(p)
}

/// The first of `taken` (`(id, title, folder)`) whose folder is `path`, holds it or sits
/// inside it: the folder already belongs to that item.
pub fn folder_owner<'a>(
    path: &str,
    taken: impl IntoIterator<Item = (i64, &'a str, &'a str)>,
) -> Option<(i64, String)> {
    let p = Path::new(path);
    taken.into_iter().find_map(|(id, title, folder)| {
        let f = content_roots::normalize(folder);
        let f = Path::new(&f);
        (!f.as_os_str().is_empty() && (p.starts_with(f) || f.starts_with(p))).then(|| (id, title.to_owned()))
    })
}

/// The `Conflict` for a folder that belongs to another item.
pub fn folder_taken(path: &str, what: &str, id: i64, title: &str) -> LibraryError {
    LibraryError::Conflict(format!("{path} already belongs to {title} ({what} {id})"))
}

/// A film or series as another app (Radarr, Sonarr) records it, for one no metadata source knows
/// (fork issue #2). It is added as a custom item that keeps the app's details and outside ids, and
/// a later refresh matches it once a source knows one of those ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceRecord {
    pub details: CustomDetails,
    /// A poster address the web app may load (TMDB's or TVDB's image hosts), or `None`.
    pub poster: Option<String>,
    pub imdb_id: Option<String>,
    /// Series only.
    pub tvdb_id: Option<u32>,
    pub monitored: bool,
}

/// One episode of a series as another app records it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceEpisode {
    pub season_number: u32,
    pub episode_number: u32,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub air_date_utc: Option<DateTime<Utc>>,
    pub monitored: bool,
}

/// Image hosts the web app's content security policy allows, so a poster from another app shows.
const POSTER_HOSTS: [&str; 2] = ["https://image.tmdb.org/", "https://artworks.thetvdb.com/"];

/// `address` when it is on one of the [`POSTER_HOSTS`], else `None`: another app's own copy
/// (`/MediaCover/…`) needs its key and would not load.
pub fn usable_poster(address: &str) -> Option<String> {
    let a = address.trim();
    POSTER_HOSTS
        .iter()
        .any(|h| a.starts_with(h) && a.len() > h.len())
        .then(|| a.to_owned())
}

/// Sort title of a typed title (the same rule as TMDB titles).
fn sort_of(title: &str) -> String {
    reelix_metadata::sort_title(title)
}

impl LibraryService {
    /// The quality profile of a new custom film or series: the one asked for, or the first
    /// video profile.
    async fn custom_profile(&self, asked: Option<i64>) -> Result<i64> {
        match asked {
            Some(id) => {
                self.check_profile(id).await?;
                Ok(id)
            }
            None => self
                .db
                .quality_profiles()
                .list()
                .await?
                .first()
                .map(|p| p.id)
                .ok_or_else(|| LibraryError::validation("there is no quality profile for films and series")),
        }
    }

    /// Folders of every movie and series: `(id, title, path)`.
    async fn taken_folders(&self, table: &str) -> Result<Vec<(i64, String, String)>> {
        let sql = format!("SELECT id, title, path FROM {table}");
        Ok(sqlx::query_as(&sql).fetch_all(self.db.pool()).await?)
    }

    /// Checks the root folder and the folder of a new custom film or series; returns the root
    /// folder id and the normalised folder. `Conflict` when a film or series has the folder.
    async fn custom_place(&self, kind: MediaKind, root_folder_id: i64, path: &str) -> Result<(i64, String)> {
        let root = self.require_root(root_folder_id, kind).await?;
        let folder = custom_folder(&root.path, path)?;
        for (table, what) in [("movies", "film"), ("series", "series")] {
            let taken = self.taken_folders(table).await?;
            if let Some((id, title)) = folder_owner(
                &folder,
                taken.iter().map(|(i, t, p)| (*i, t.as_str(), p.as_str())),
            ) {
                return Err(folder_taken(&folder, what, id, &title));
            }
        }
        Ok((root.id, folder))
    }

    /// Adds a film with typed details from an existing folder; queues a disk scan. Nothing is
    /// fetched from TMDB, now or on refresh.
    pub async fn add_custom_movie(
        &self,
        d: &CustomDetails,
        root_folder_id: i64,
        path: &str,
        quality_profile_id: Option<i64>,
        monitored: bool,
    ) -> Result<MovieDto> {
        let profile = self.custom_profile(quality_profile_id).await?;
        let (root, folder) = self.custom_place(MediaKind::Movie, root_folder_id, path).await?;
        let mut m = blank_movie();
        m.custom = true;
        m.title = d.title.clone();
        m.sort_title = sort_of(&d.title);
        m.year = d.year;
        m.overview = d.overview.clone();
        m.status = MovieStatus::Released;
        m.monitored = monitored;
        m.quality_profile_id = profile;
        m.root_folder_id = root;
        m.path = folder;
        m.last_refreshed = Some(chrono::Utc::now());
        let id = movies::insert(&self.db, &m).await?;
        self.emit("movie", "created", id);
        tracing::info!(id, title = %m.title, "custom film added");
        self.queue_scan(&[id], &[]).await?;
        self.get_movie(id).await
    }

    /// Adds a series with typed details from an existing folder; queues a disk scan, which
    /// makes the seasons and episodes from the file names.
    pub async fn add_custom_series(
        &self,
        d: &CustomDetails,
        root_folder_id: i64,
        path: &str,
        quality_profile_id: Option<i64>,
        monitored: bool,
    ) -> Result<SeriesDto> {
        let profile = self.custom_profile(quality_profile_id).await?;
        let (root, folder) = self.custom_place(MediaKind::Series, root_folder_id, path).await?;
        let mut s = blank_series();
        s.custom = true;
        s.title = d.title.clone();
        s.sort_title = sort_of(&d.title);
        s.year = d.year;
        s.overview = d.overview.clone();
        s.status = SeriesStatus::Ended;
        s.monitored = monitored;
        s.quality_profile_id = profile;
        s.root_folder_id = root;
        s.path = folder;
        s.last_refreshed = Some(chrono::Utc::now());
        let id = series_repo::insert(&self.db, &s).await?;
        self.emit("series", "created", id);
        tracing::info!(id, title = %s.title, "custom series added");
        self.queue_scan(&[], &[id]).await?;
        self.get_series(id).await
    }

    /// Adds a film no metadata source knows with another app's details (fork issue #2): a custom
    /// film, as [`Self::add_custom_movie`], that also keeps the app's poster and IMDb id. The
    /// folder must exist. A refresh matches it once TMDB or open data knows the IMDb id.
    pub async fn add_movie_from_source(
        &self,
        r: &SourceRecord,
        root_folder_id: i64,
        path: &str,
        quality_profile_id: Option<i64>,
    ) -> Result<MovieDto> {
        let m = self
            .add_custom_movie(&r.details, root_folder_id, path, quality_profile_id, r.monitored)
            .await?;
        let mut m = self.custom_movie(m.id).await?;
        m.imdb_id = r.imdb_id.clone().filter(|i| i.starts_with("tt"));
        m.images = ImagesDto {
            poster: r.poster.clone(),
            ..m.images
        };
        movies::save(&self.db, m.id, &m).await?;
        self.emit("movie", "updated", m.id);
        self.get_movie(m.id).await
    }

    /// Adds a series no metadata source knows with another app's details (fork issue #2): a
    /// custom series, as [`Self::add_custom_series`], that also keeps the app's poster, TVDB and
    /// IMDb ids, seasons and episodes with their titles and air dates. Files are linked to the
    /// episodes by the scan. A refresh matches it once a source knows the TVDB id.
    pub async fn add_series_from_source(
        &self,
        r: &SourceRecord,
        root_folder_id: i64,
        path: &str,
        quality_profile_id: Option<i64>,
        episodes: &[SourceEpisode],
    ) -> Result<SeriesDto> {
        let s = self
            .add_custom_series(&r.details, root_folder_id, path, quality_profile_id, r.monitored)
            .await?;
        let mut s = self.custom_series(s.id).await?;
        s.imdb_id = r.imdb_id.clone().filter(|i| i.starts_with("tt"));
        s.tvdb_id = r.tvdb_id.filter(|t| *t > 0);
        s.images = ImagesDto {
            poster: r.poster.clone(),
            ..s.images
        };
        series_repo::save(&self.db, s.id, &s).await?;
        self.custom_episodes_from_source(&s, episodes).await?;
        self.emit("series", "updated", s.id);
        self.get_series(s.id).await
    }

    /// Seasons and episodes of a custom series from another app's record of them. Episodes the
    /// series has already (from a scan) get the titles and air dates; their monitoring stays.
    async fn custom_episodes_from_source(&self, s: &SeriesDto, episodes: &[SourceEpisode]) -> Result<()> {
        let seasons: std::collections::BTreeSet<u32> = episodes.iter().map(|e| e.season_number).collect();
        for n in seasons {
            let air_date = episodes
                .iter()
                .filter(|e| e.season_number == n)
                .filter_map(|e| e.air_date)
                .min();
            series_repo::upsert_season(
                &self.db,
                s.id,
                &SeasonDto {
                    season_number: n,
                    monitored: s.monitored && n > 0,
                    title: None,
                    overview: None,
                    poster: None,
                    air_date,
                    statistics: Default::default(),
                },
            )
            .await?;
        }
        for e in episodes {
            let (id, new) = series_repo::upsert_episode(
                &self.db,
                &EpisodeDto {
                    id: 0,
                    series_id: s.id,
                    season_number: e.season_number,
                    episode_number: e.episode_number,
                    absolute_number: None,
                    scene_season_number: None,
                    scene_episode_number: None,
                    scene_absolute_number: None,
                    title: e.title.clone(),
                    overview: e.overview.clone(),
                    air_date: e.air_date,
                    air_date_utc: e.air_date_utc,
                    runtime_minutes: None,
                    still: None,
                    finale: None,
                    monitored: s.monitored && e.monitored && e.season_number > 0,
                    has_file: false,
                    episode_file_id: None,
                    episode_file: None,
                },
            )
            .await?;
            self.emit("episode", if new { "created" } else { "updated" }, id);
        }
        Ok(())
    }

    /// For a custom film added from another app with its IMDb id: the TMDB id a metadata source
    /// now has for it, if any. Errors from the source count as no match, and it is tried again next time.
    async fn source_match_for_movie(&self, m: &MovieDto) -> Option<u32> {
        let imdb = m.imdb_id.as_deref().filter(|i| i.starts_with("tt"))?;
        match self
            .metadata
            .find(ExternalSource::Imdb, imdb, MediaKind::Movie)
            .await
        {
            Ok(hits) => hits.into_iter().find_map(|h| h.tmdb_id).filter(|t| *t > 0),
            Err(e) => {
                tracing::debug!(id = m.id, imdb, error = %e, "custom film: no match by IMDb id yet");
                None
            }
        }
    }

    /// Points a custom film added from another app at its TMDB entry once a source knows its
    /// IMDb id (fork issue #2), so the refresh that called it carries on as for any film and the
    /// source's details replace the app's. `false` leaves the film as it is: a film added by hand
    /// has no IMDb id, and a TMDB id another film has is not taken.
    pub(crate) async fn adopt_source_match_movie(&self, m: &mut MovieDto) -> Result<bool> {
        let Some(tmdb_id) = self.source_match_for_movie(m).await else {
            return Ok(false);
        };
        if let Some(other) = movies::id_by_tmdb(&self.db, tmdb_id).await?
            && other != m.id
        {
            tracing::info!(
                id = m.id,
                tmdb_id,
                other,
                "custom film: its TMDB entry is another film's"
            );
            return Ok(false);
        }
        sqlx::query("UPDATE movies SET tmdb_id = ?, custom = 0 WHERE id = ?")
            .bind(i64::from(tmdb_id))
            .bind(m.id)
            .execute(self.db.pool())
            .await?;
        tracing::info!(id = m.id, tmdb_id, "custom film from another app matched");
        m.tmdb_id = tmdb_id;
        m.custom = false;
        self.queue_scan(&[m.id], &[]).await?;
        Ok(true)
    }

    /// Points a custom series added from another app at its TMDB entry once a source knows its
    /// TVDB id (fork issue #2), so the refresh that called it carries on as for any series.
    /// `false` leaves the series as it is: a series added by hand has no TVDB id.
    pub(crate) async fn adopt_source_match_series(&self, s: &mut SeriesDto) -> Result<bool> {
        let Some(tvdb) = s.tvdb_id.filter(|t| *t > 0) else {
            return Ok(false);
        };
        let meta = match crate::series::series_metadata(
            self.metadata.as_ref(),
            SeriesExternalId::Tvdb(tvdb),
            Some(tvdb),
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(id = s.id, tvdb, error = %e, "custom series: no match by TVDB id yet");
                return Ok(false);
            }
        };
        let Some(tmdb) = meta.tmdb_id.filter(|t| *t > 0) else {
            return Ok(false);
        };
        if let Some(other) = series_repo::id_by_tmdb(&self.db, tmdb).await?
            && other != s.id
        {
            tracing::info!(
                id = s.id,
                tmdb_id = tmdb,
                other,
                "custom series: its TMDB entry is another series'"
            );
            return Ok(false);
        }
        sqlx::query("UPDATE series SET tmdb_id = ?, custom = 0 WHERE id = ?")
            .bind(i64::from(tmdb))
            .bind(s.id)
            .execute(self.db.pool())
            .await?;
        tracing::info!(
            id = s.id,
            tmdb_id = tmdb,
            "custom series from another app matched"
        );
        s.tmdb_id = Some(tmdb);
        s.custom = false;
        self.queue_scan(&[], &[s.id]).await?;
        Ok(true)
    }

    async fn custom_movie(&self, id: i64) -> Result<MovieDto> {
        let m = self.get_movie(id).await?;
        if !m.custom {
            return Err(LibraryError::validation(format!(
                "film {id} has a TMDB match; only films added with their own details can be changed here"
            )));
        }
        Ok(m)
    }

    async fn custom_series(&self, id: i64) -> Result<SeriesDto> {
        let s = self.series_row(id).await?;
        if !s.custom {
            return Err(LibraryError::validation(format!(
                "series {id} has a TMDB match; only series added with their own details can be changed here"
            )));
        }
        Ok(s)
    }

    /// Changes the typed details of a custom film.
    pub async fn change_custom_movie(&self, id: i64, c: &CustomDetailsChange) -> Result<MovieDto> {
        let mut m = self.custom_movie(id).await?;
        if let Some(t) = &c.title {
            m.title = t.clone();
            m.sort_title = sort_of(t);
        }
        if let Some(y) = c.year {
            m.year = y;
        }
        if let Some(o) = &c.overview {
            m.overview = o.clone();
        }
        movies::save(&self.db, id, &m).await?;
        self.emit("movie", "updated", id);
        self.get_movie(id).await
    }

    /// Changes the typed details of a custom series.
    pub async fn change_custom_series(&self, id: i64, c: &CustomDetailsChange) -> Result<SeriesDto> {
        let mut s = self.custom_series(id).await?;
        if let Some(t) = &c.title {
            s.title = t.clone();
            s.sort_title = sort_of(t);
        }
        if let Some(y) = c.year {
            s.year = y;
        }
        if let Some(o) = &c.overview {
            s.overview = o.clone();
        }
        series_repo::save(&self.db, id, &s).await?;
        self.emit("series", "updated", id);
        self.get_series(id).await
    }

    /// Sets (or with `None` removes) the poster of a custom film: the address Reelix serves
    /// its own copy at.
    pub async fn set_custom_movie_poster(&self, id: i64, poster: Option<String>) -> Result<()> {
        let mut m = self.custom_movie(id).await?;
        m.images = ImagesDto { poster, ..m.images };
        movies::save(&self.db, id, &m).await?;
        self.emit("movie", "updated", id);
        Ok(())
    }

    /// Sets (or removes) the poster of a custom series.
    pub async fn set_custom_series_poster(&self, id: i64, poster: Option<String>) -> Result<()> {
        let mut s = self.custom_series(id).await?;
        s.images = ImagesDto { poster, ..s.images };
        series_repo::save(&self.db, id, &s).await?;
        self.emit("series", "updated", id);
        Ok(())
    }

    /// Matches a custom film to its TMDB entry ("match now"): the typed details give way to
    /// TMDB's, as for any film, and later refreshes follow TMDB. The folder, files, quality
    /// profile and monitoring stay. `Conflict` when another film has that TMDB id; `Validation`
    /// when TMDB has no such film (nothing is changed then).
    pub async fn match_custom_movie(&self, id: i64, tmdb_id: u32) -> Result<MovieDto> {
        self.custom_movie(id).await?;
        if tmdb_id == 0 {
            return Err(LibraryError::validation("tmdb_id is required"));
        }
        if let Some(other) = movies::id_by_tmdb(&self.db, tmdb_id).await?
            && other != id
        {
            return Err(LibraryError::Conflict(format!(
                "film with TMDB id {tmdb_id} is already in the library (film {other})"
            )));
        }
        // Checked before anything changes; the refresh below reads it from the client's cache.
        self.metadata
            .movie(tmdb_id)
            .await
            .map_err(|e| no_match(e, "TMDB", "film", tmdb_id))?;
        sqlx::query("UPDATE movies SET tmdb_id = ?, custom = 0 WHERE id = ?")
            .bind(i64::from(tmdb_id))
            .bind(id)
            .execute(self.db.pool())
            .await?;
        tracing::info!(id, tmdb_id, "custom film matched");
        self.refresh_movie(id).await?;
        self.queue_scan(&[id], &[]).await?;
        self.get_movie(id).await
    }

    /// Matches a custom series to its TMDB entry (by TMDB id, or by TVDB id): TMDB's details,
    /// seasons and episodes replace the typed ones, and later refreshes follow TMDB. The folder
    /// and files stay; a scan is queued to link the files to TMDB's episodes. Episodes made from
    /// file names that TMDB does not have are kept while they have a file.
    pub async fn match_custom_series(
        &self,
        id: i64,
        tmdb_id: Option<u32>,
        tvdb_id: Option<u32>,
    ) -> Result<SeriesDto> {
        self.custom_series(id).await?;
        let (ext, label) = match (tmdb_id.filter(|t| *t > 0), tvdb_id.filter(|t| *t > 0)) {
            (Some(t), _) => (SeriesExternalId::Tmdb(t), ("TMDB", t)),
            (None, Some(t)) => (SeriesExternalId::Tvdb(t), ("TVDB", t)),
            (None, None) => return Err(LibraryError::validation("tmdb_id or tvdb_id is required")),
        };
        let meta = crate::series::series_metadata(self.metadata.as_ref(), ext, tvdb_id)
            .await
            .map_err(|e| no_match(e, label.0, "series", label.1))?;
        let Some(tmdb) = meta.tmdb_id else {
            return Err(LibraryError::validation("the series found has no TMDB id"));
        };
        if let Some(other) = series_repo::id_by_tmdb(&self.db, tmdb).await?
            && other != id
        {
            return Err(LibraryError::Conflict(format!(
                "series with TMDB id {tmdb} is already in the library (series {other})"
            )));
        }
        let tvdb = meta.tvdb_id.or(meta.external_ids.tvdb).or(tvdb_id);
        if let Some(t) = tvdb
            && let Some(other) = series_repo::id_by_tvdb(&self.db, t).await?
            && other != id
        {
            return Err(LibraryError::Conflict(format!(
                "series with TVDB id {t} is already in the library (series {other})"
            )));
        }
        sqlx::query("UPDATE series SET tmdb_id = ?, tvdb_id = ?, custom = 0 WHERE id = ?")
            .bind(i64::from(tmdb))
            .bind(tvdb.map(i64::from))
            .bind(id)
            .execute(self.db.pool())
            .await?;
        tracing::info!(id, tmdb_id = tmdb, "custom series matched");
        self.refresh_series(id).await?;
        self.queue_scan(&[], &[id]).await?;
        self.get_series(id).await
    }

    /// Seasons and episodes of a custom series from its video files: each file's season and
    /// episode numbers (`S01E02`, `1x02`, a `Season 1` folder). Files without both numbers stay
    /// unmatched. Existing episodes are kept; new ones follow the series' monitoring.
    pub(crate) async fn custom_episodes_from_files(
        &self,
        s: &SeriesDto,
        files: &[std::path::PathBuf],
    ) -> Result<()> {
        let known: std::collections::HashSet<(u32, u32)> = series_repo::episodes(&self.db, s.id, None)
            .await?
            .into_iter()
            .map(|e| (e.season_number, e.episode_number))
            .collect();
        let mut seasons = std::collections::BTreeSet::new();
        let mut new = std::collections::BTreeSet::new();
        for f in files {
            for (season, episode) in numbers_of(&f.to_string_lossy()) {
                seasons.insert(season);
                if !known.contains(&(season, episode)) {
                    new.insert((season, episode));
                }
            }
        }
        let have: std::collections::HashSet<u32> = s.seasons.iter().map(|x| x.season_number).collect();
        for n in seasons.into_iter().filter(|n| !have.contains(n)) {
            series_repo::upsert_season(
                &self.db,
                s.id,
                &SeasonDto {
                    season_number: n,
                    monitored: s.monitored && n > 0,
                    title: None,
                    overview: None,
                    poster: None,
                    air_date: None,
                    statistics: Default::default(),
                },
            )
            .await?;
        }
        for (season, episode) in new {
            let (id, _) = series_repo::upsert_episode(
                &self.db,
                &EpisodeDto {
                    id: 0,
                    series_id: s.id,
                    season_number: season,
                    episode_number: episode,
                    absolute_number: None,
                    scene_season_number: None,
                    scene_episode_number: None,
                    scene_absolute_number: None,
                    title: None,
                    overview: None,
                    air_date: None,
                    air_date_utc: None,
                    runtime_minutes: None,
                    still: None,
                    finale: None,
                    monitored: s.monitored && season > 0,
                    has_file: false,
                    episode_file_id: None,
                    episode_file: None,
                },
            )
            .await?;
            self.emit("episode", "created", id);
        }
        Ok(())
    }
}

/// A failed look-up for "match now": an id the source does not know is the caller's mistake.
fn no_match(e: MetaError, source: &str, what: &str, id: u32) -> LibraryError {
    match e {
        MetaError::NotFound => LibraryError::validation(format!("{source} has no {what} with id {id}")),
        e => e.into(),
    }
}

/// `(season, episode)` pairs a file name gives (several for a multi-episode file).
pub(crate) fn numbers_of(path: &str) -> Vec<(u32, u32)> {
    match parse_path(path).kind {
        ReleaseKind::Episode(EpisodeInfo {
            season: Some(season),
            episodes,
            ..
        }) => episodes.into_iter().map(|e| (season, e)).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_of_a_folder() {
        let taken = [(1, "Home videos", "/tv/Home videos/"), (2, "Other", "/tv/Other")];
        let t = || taken.iter().map(|(i, t, p)| (*i, *t, *p));
        assert_eq!(
            folder_owner("/tv/Home videos", t()),
            Some((1, "Home videos".into()))
        );
        assert_eq!(
            folder_owner("/tv/Home videos/Season 1", t()),
            Some((1, "Home videos".into()))
        );
        assert_eq!(folder_owner("/tv", t()), Some((1, "Home videos".into())));
        assert_eq!(folder_owner("/tv/Home", t()), None);
        assert_eq!(folder_owner("/tv/New", t()), None);
    }

    #[test]
    fn folder_checks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        std::fs::create_dir(dir.path().join("Lectures")).unwrap();
        let inside = format!("{root}/Lectures/");
        assert_eq!(custom_folder(&root, &inside).unwrap(), format!("{root}/Lectures"));
        assert!(custom_folder(&root, &format!("{root}/Missing")).is_err());
        assert!(custom_folder(&root, "/elsewhere/Lectures").is_err());
        assert!(custom_folder(&root, &root).is_err());
        assert!(custom_folder(&root, "").is_err());
    }

    #[test]
    fn episode_numbers_from_names() {
        assert_eq!(numbers_of("/tv/Home/Season 1/Home S01E02.mkv"), vec![(1, 2)]);
        assert_eq!(numbers_of("/tv/Home/Home.S02E03E04.mkv"), vec![(2, 3), (2, 4)]);
        assert!(numbers_of("/tv/Home/birthday.mkv").is_empty());
    }
}
