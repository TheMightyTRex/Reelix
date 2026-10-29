//! Public helpers the pipeline's import stage uses to place and record files.
//!
//! Everything hangs off [`LibraryService::importer`] so the pipeline gets one narrow surface:
//! targets (media + profile + existing files), scoring, destination paths from the naming
//! settings, recording imported files (replacing old rows), recycling replaced files and a
//! [`PathGuard`] over the root folders.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::Utc;
use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto};
use reelix_core::MediaKind;
use reelix_decision::QualityProfile;
use reelix_import::files::PathGuard;
use reelix_import::matching::{ExpectedEpisode, ExpectedMovie};
use reelix_import::mediainfo::MediaInfo;
use reelix_import::naming::{EpisodeNaming, FileNaming};
use reelix_parser::{Language, ParsedRelease, QualityModel};

use crate::LibraryService;
use crate::convert::{series_type, year_u16};
use crate::error::{LibraryError, Result};
use crate::movies::movie_naming;
use crate::repo::files::{self, FileRow, FileTable};
use crate::repo::{misc, movies, placeholders, series as repo};
use crate::series::series_naming;
use crate::service::ProfileCache;

/// A file already in the library for the import target.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingFile {
    pub id: i64,
    /// Absolute path.
    pub path: PathBuf,
    pub relative_path: String,
    pub size: u64,
    pub quality: QualityModel,
    pub format_score: i32,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    /// Episode files: the episodes linked to it.
    pub episode_ids: Vec<i64>,
}

/// A movie with its profile and current file.
#[derive(Debug, Clone)]
pub struct MovieTarget {
    pub movie: MovieDto,
    pub profile: Option<QualityProfile>,
    pub existing: Option<ExistingFile>,
}

/// A series with its episodes, profile and current files.
#[derive(Debug, Clone)]
pub struct SeriesTarget {
    pub series: SeriesDto,
    pub series_type: reelix_import::SeriesType,
    pub episodes: Vec<EpisodeDto>,
    pub profile: Option<QualityProfile>,
    pub files: Vec<ExistingFile>,
}

impl SeriesTarget {
    /// Episodes in the matcher's input shape.
    pub fn expected(&self) -> Vec<ExpectedEpisode> {
        self.episodes
            .iter()
            .map(|e| ExpectedEpisode {
                id: e.id,
                season: e.season_number,
                episode: e.episode_number,
                absolute: e.absolute_number,
                air_date: e.air_date,
                scene_season: e.scene_season_number,
                scene_episode: e.scene_episode_number,
                scene_absolute: e.scene_absolute_number,
            })
            .collect()
    }

    /// Existing files linked to any of `episode_ids`.
    pub fn files_for(&self, episode_ids: &[i64]) -> Vec<&ExistingFile> {
        self.files
            .iter()
            .filter(|f| f.episode_ids.iter().any(|e| episode_ids.contains(e)))
            .collect()
    }
}

/// Custom formats matched by a file and its score under a profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileScore {
    pub format_ids: Vec<i64>,
    /// `(id, name)` of the matched formats.
    pub formats: Vec<(i64, String)>,
    pub score: i32,
}

/// A file that was transferred into the library and should be recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedFile {
    /// Absolute destination path (inside the movie/series folder).
    pub path: PathBuf,
    pub size: u64,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    /// Release title the file came from.
    pub scene_name: Option<String>,
    /// Source path in the download folder.
    pub original_path: Option<String>,
    pub media_info: Option<MediaInfo>,
    pub score: FileScore,
}

/// Library-side movie/series candidates for mapping a release title.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaCandidate {
    pub kind: MediaKind,
    pub id: i64,
    pub expected: ExpectedMovie,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
}

/// `(title, year)` per media id.
pub type TitleMap = HashMap<i64, (String, Option<i32>)>;

/// See the module docs.
pub struct Importer<'a> {
    svc: &'a LibraryService,
}

impl LibraryService {
    /// Import helpers for the pipeline.
    pub fn importer(&self) -> Importer<'_> {
        Importer { svc: self }
    }
}

fn existing(f: &FileRow, base: &str, episode_ids: Vec<i64>) -> ExistingFile {
    ExistingFile {
        id: f.id,
        path: Path::new(base).join(&f.relative_path),
        relative_path: f.relative_path.clone(),
        size: f.size,
        quality: f.quality,
        format_score: f.custom_format_score,
        scene_name: f.scene_name.clone(),
        original_path: f.original_path.clone(),
        episode_ids,
    }
}

/// What a destination file name is rendered from.
#[derive(Debug, Clone, Copy)]
pub struct NamingInput<'a> {
    pub parsed: &'a ParsedRelease,
    pub media_info: Option<&'a MediaInfo>,
    pub quality: QualityModel,
    pub score: &'a FileScore,
    /// Source file stem (used when renaming is off).
    pub original_file_name: Option<&'a str>,
    /// Extension without the dot.
    pub ext: &'a str,
}

fn file_naming(f: &NamingInput<'_>, cache: &ProfileCache) -> FileNaming {
    let mut n = FileNaming::from_parsed(f.parsed, f.media_info);
    n.quality = f.quality;
    n.custom_formats = f
        .score
        .format_ids
        .iter()
        .filter(|id| {
            cache
                .formats
                .iter()
                .any(|cf| cf.id == **id && cf.include_in_rename)
        })
        .filter_map(|id| cache.format_names.get(id).cloned())
        .collect();
    n.original_file_name = f.original_file_name.map(str::to_owned);
    n
}

fn ext_str(ext: &str) -> &str {
    let e = ext.trim_start_matches('.');
    if e.is_empty() { "mkv" } else { e }
}

impl Importer<'_> {
    pub async fn movie_target(&self, movie_id: i64) -> Result<MovieTarget> {
        let svc = self.svc;
        let cache = svc.profiles().await?;
        let movie = movies::get(&svc.db, movie_id, &cache)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", movie_id))?;
        let rows = files::list(&svc.db, FileTable::Movie, Some(&[movie_id])).await?;
        let existing = rows
            .iter()
            .max_by_key(|f| f.size)
            .map(|f| existing(f, &movie.path, Vec::new()));
        Ok(MovieTarget {
            profile: cache.profile(movie.quality_profile_id).cloned(),
            movie,
            existing,
        })
    }

    pub async fn series_target(&self, series_id: i64) -> Result<SeriesTarget> {
        let svc = self.svc;
        let cache = svc.profiles().await?;
        let series = svc.series_row(series_id).await?;
        let episodes = repo::episodes(&svc.db, series_id, None).await?;
        let map = repo::file_episode_map(&svc.db, Some(series_id)).await?;
        let files = files::list(&svc.db, FileTable::Episode, Some(&[series_id]))
            .await?
            .iter()
            .map(|f| existing(f, &series.path, map.get(&f.id).cloned().unwrap_or_default()))
            .collect();
        Ok(SeriesTarget {
            profile: cache.profile(series.quality_profile_id).cloned(),
            series_type: series_type(series.series_type),
            series,
            episodes,
            files,
        })
    }

    /// Custom formats + score of a parsed file under `profile_id`.
    pub async fn score(&self, parsed: &ParsedRelease, size: u64, profile_id: i64) -> Result<FileScore> {
        let cache = self.svc.profiles().await?;
        let (ids, score) = crate::scan::score(parsed, size, cache.profile(profile_id), &cache);
        Ok(FileScore {
            formats: ids
                .iter()
                .filter_map(|id| cache.format_names.get(id).map(|n| (*id, n.clone())))
                .collect(),
            format_ids: ids,
            score,
        })
    }

    /// `<movie folder>/<file name>.<ext>` from the naming settings.
    pub async fn movie_destination(&self, movie: &MovieDto, file: &NamingInput<'_>) -> Result<PathBuf> {
        let cache = self.svc.profiles().await?;
        let naming = self.svc.naming_config().await?;
        let fnm = file_naming(file, &cache);
        let ext = file.ext;
        let name = naming.movie_file_name(&movie_naming(movie), &fnm)?;
        Ok(Path::new(&movie.path).join(format!("{name}.{}", ext_str(ext))))
    }

    /// `<series folder>/<season folder>/<file name>.<ext>` (no season folder when the series
    /// doesn't use them).
    pub async fn episode_destination(
        &self,
        series: &SeriesDto,
        episodes: &[&EpisodeDto],
        file: &NamingInput<'_>,
    ) -> Result<PathBuf> {
        let ext = file.ext;
        if episodes.is_empty() {
            return Err(LibraryError::validation("no episodes to name the file after"));
        }
        let cache = self.svc.profiles().await?;
        let naming = self.svc.naming_config().await?;
        let fnm = file_naming(file, &cache);
        let mut eps: Vec<&EpisodeDto> = episodes.to_vec();
        eps.sort_by_key(|e| (e.season_number, e.episode_number));
        let en: Vec<EpisodeNaming> = eps
            .iter()
            .map(|e| EpisodeNaming {
                season: e.season_number,
                episode: e.episode_number,
                absolute: e.absolute_number,
                title: e.title.clone().unwrap_or_default(),
                air_date: e.air_date,
            })
            .collect();
        let sn = series_naming(series);
        let full = naming.episode_path(Path::new(""), &sn, &en, &fnm, ext_str(ext))?;
        // `episode_path` = <series folder>/<season folder>/<file>; the series folder is `series.path`.
        let mut comps: Vec<String> = full
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if !comps.is_empty() {
            comps.remove(0);
        }
        if !series.season_folder && comps.len() > 1 {
            comps.drain(..comps.len() - 1);
        }
        let mut out = PathBuf::from(&series.path);
        for c in comps {
            out.push(c);
        }
        Ok(out)
    }

    /// Records the movie's (new) file and drops rows of files it replaced. Returns the file id.
    pub async fn record_movie_file(&self, movie_id: i64, f: &ImportedFile) -> Result<i64> {
        let svc = self.svc;
        let cache = svc.profiles().await?;
        let movie = movies::get(&svc.db, movie_id, &cache)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", movie_id))?;
        let rel = relative(&f.path, &movie.path)?;
        let fid = files::upsert(&svc.db, FileTable::Movie, &row(movie_id, None, rel, f)).await?;
        for old in files::list(&svc.db, FileTable::Movie, Some(&[movie_id])).await? {
            if old.id != fid {
                files::delete(&svc.db, FileTable::Movie, old.id).await?;
                svc.emit("movie_file", "deleted", old.id);
            }
        }
        svc.emit("movie_file", "created", fid);
        svc.emit("movie", "updated", movie_id);
        Ok(fid)
    }

    /// Records an episode file, links it to `episode_ids` and drops rows of files no episode
    /// references any more. Returns the file id.
    pub async fn record_episode_file(
        &self,
        series_id: i64,
        episode_ids: &[i64],
        f: &ImportedFile,
    ) -> Result<i64> {
        let svc = self.svc;
        if episode_ids.is_empty() {
            return Err(LibraryError::validation("no episodes for the file"));
        }
        let series = svc.series_row(series_id).await?;
        let eps = repo::episodes_by_ids(&svc.db, episode_ids).await?;
        if eps.len() != episode_ids.len() || eps.iter().any(|e| e.series_id != series_id) {
            return Err(LibraryError::validation("episodes do not belong to the series"));
        }
        let season = eps.iter().map(|e| e.season_number).min().unwrap_or(0);
        let rel = relative(&f.path, &series.path)?;
        let before: BTreeMap<i64, Vec<i64>> = repo::file_episode_map(&svc.db, Some(series_id)).await?;
        let fid = files::upsert(&svc.db, FileTable::Episode, &row(series_id, Some(season), rel, f)).await?;
        repo::set_episode_file(&svc.db, episode_ids, Some(fid)).await?;
        let after = repo::file_episode_map(&svc.db, Some(series_id)).await?;
        for old in before.keys() {
            if *old != fid && !after.contains_key(old) {
                files::delete(&svc.db, FileTable::Episode, *old).await?;
                svc.emit("episode_file", "deleted", *old);
            }
        }
        svc.emit("episode_file", "created", fid);
        for e in episode_ids {
            svc.emit("episode", "updated", *e);
        }
        svc.emit("series", "updated", series_id);
        Ok(fid)
    }

    /// Deletes a replaced file: into the recycle bin when configured, else permanently.
    /// Returns the recycle-bin location, if any.
    pub async fn remove_file(&self, path: &Path) -> Result<Option<PathBuf>> {
        self.svc.remove_path(path).await
    }

    /// Guard over every existing root folder plus `extra` (e.g. download folders).
    pub async fn guard(&self, extra: &[PathBuf]) -> Result<PathGuard> {
        let mut g = self.svc.path_guard().await?;
        for p in extra {
            if p.exists() {
                g.add_root(p)?;
            }
        }
        Ok(g)
    }

    /// Every library movie and series as title-matching candidates.
    pub async fn candidates(&self) -> Result<Vec<MediaCandidate>> {
        let svc = self.svc;
        let mut out = Vec::new();
        let alt = misc::alt_titles_of(&svc.db, MediaKind::Movie).await?;
        for r in sqlx::query_as::<_, (i64, String, Option<i64>, Option<i64>, Option<String>)>(
            "SELECT id, title, year, tmdb_id, imdb_id FROM movies",
        )
        .fetch_all(svc.db.pool())
        .await?
        {
            out.push(MediaCandidate {
                kind: MediaKind::Movie,
                id: r.0,
                expected: ExpectedMovie {
                    title: r.1,
                    alternative_titles: alt.get(&r.0).cloned().unwrap_or_default(),
                    year: year_u16(r.2.map(|y| y as i32)),
                },
                tmdb_id: r.3.map(|v| v as u32),
                tvdb_id: None,
                imdb_id: r.4,
            });
        }
        let alt = misc::alt_titles_of(&svc.db, MediaKind::Series).await?;
        for r in sqlx::query_as::<_, (i64, String, Option<i64>, Option<i64>, Option<i64>, Option<String>)>(
            "SELECT id, title, year, tmdb_id, tvdb_id, imdb_id FROM series",
        )
        .fetch_all(svc.db.pool())
        .await?
        {
            out.push(MediaCandidate {
                kind: MediaKind::Series,
                id: r.0,
                expected: ExpectedMovie {
                    title: r.1,
                    alternative_titles: alt.get(&r.0).cloned().unwrap_or_default(),
                    year: year_u16(r.2.map(|y| y as i32)),
                },
                tmdb_id: r.3.map(|v| v as u32),
                tvdb_id: r.4.map(|v| v as u32),
                imdb_id: r.5,
            });
        }
        Ok(out)
    }

    /// Titles and years of movies / series, by id (batch lookup for list views).
    pub async fn titles(&self, kind: MediaKind, ids: &[i64]) -> Result<TitleMap> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let table = match kind {
            MediaKind::Movie => "movies",
            MediaKind::Series => "series",
        };
        let sql = format!(
            "SELECT id, title, year FROM {table} WHERE id IN ({})",
            placeholders(ids.len())
        );
        let mut q = sqlx::query_as::<_, (i64, String, Option<i64>)>(&sql);
        for id in ids {
            q = q.bind(id);
        }
        Ok(q.fetch_all(self.svc.db.pool())
            .await?
            .into_iter()
            .map(|(id, t, y)| (id, (t, y.map(|y| y as i32))))
            .collect())
    }

    /// Episodes by id (any series).
    pub async fn episodes(&self, ids: &[i64]) -> Result<Vec<EpisodeDto>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        repo::episodes_by_ids(&self.svc.db, ids).await
    }
}

fn relative(path: &Path, base: &str) -> Result<String> {
    path.strip_prefix(base)
        .map(|r| r.to_string_lossy().into_owned())
        .map_err(|_| {
            LibraryError::validation(format!(
                "{} is not inside the media folder {base}",
                path.display()
            ))
        })
}

fn row(owner: i64, season: Option<u32>, rel: String, f: &ImportedFile) -> FileRow {
    FileRow {
        id: 0,
        owner_id: owner,
        season_number: season,
        relative_path: rel,
        size: f.size,
        quality: f.quality,
        languages: f.languages.clone(),
        release_group: f.release_group.clone(),
        edition: f.edition.clone(),
        scene_name: f.scene_name.clone(),
        original_path: f.original_path.clone(),
        media_info: f.media_info.clone(),
        custom_format_ids: f.score.format_ids.clone(),
        custom_format_score: f.score.score,
        date_added: Utc::now(),
    }
}
