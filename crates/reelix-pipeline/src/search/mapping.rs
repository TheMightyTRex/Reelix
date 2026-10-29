//! Turning indexer results into decision candidates and mapping them to library media:
//! indexer ids (TMDB / IMDb / TVDB) first, then the parsed title (+ alternative titles and
//! year) and, for TV, the parsed episode numbering.

use std::collections::HashMap;

use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto, SeriesType};
use reelix_decision::{IndexerFlag, ReleaseCandidate};
use reelix_import::matching::{
    ExpectedEpisode, ExpectedMovie, best_movie_match, match_movie, match_parsed_episodes, match_title,
    normalize_title,
};
use reelix_indexers::ReleaseInfo;
use reelix_parser::{ParsedRelease, ReleaseKind};

/// Indexer flag → decision-engine flag.
pub fn decision_flag(f: reelix_indexers::IndexerFlag) -> IndexerFlag {
    use reelix_indexers::IndexerFlag as I;
    match f {
        I::Freeleech => IndexerFlag::Freeleech,
        I::Halfleech => IndexerFlag::Halfleech,
        I::DoubleUpload => IndexerFlag::DoubleUpload,
        I::Internal => IndexerFlag::Internal,
        I::Scene => IndexerFlag::Scene,
        I::Nuked => IndexerFlag::Nuked,
    }
}

/// Flag name as stored in [`crate::GrabbedRelease::indexer_flags`].
pub fn flag_name(f: IndexerFlag) -> String {
    serde_json::to_value(f)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn parse_flag(s: &str) -> Option<IndexerFlag> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

/// A decision candidate for an indexer result (`priority`: the indexer row's priority).
pub fn candidate(r: &ReleaseInfo, priority: i32) -> ReleaseCandidate {
    let mut c = ReleaseCandidate::new(&r.title, r.protocol);
    c.size = r.size.unwrap_or(0);
    c.indexer_id = Some(r.indexer_id);
    c.indexer = r.indexer.clone();
    c.indexer_priority = priority;
    c.seeders = r.seeders;
    c.peers = r.peers;
    c.publish_date = r.publish_date;
    c.indexer_flags = r.indexer_flags.iter().copied().map(decision_flag).collect();
    if r.download_volume_factor == 0.0 && !c.indexer_flags.contains(&IndexerFlag::Freeleech) {
        c.indexer_flags.push(IndexerFlag::Freeleech);
    }
    c.download_url = r
        .download_url
        .clone()
        .or_else(|| r.magnet_url.clone())
        .unwrap_or_default();
    c.guid = if r.guid.is_empty() {
        c.download_url.clone()
    } else {
        r.guid.clone()
    };
    c
}

fn imdb_eq(a: &str, b: &str) -> bool {
    let n = |s: &str| {
        s.trim()
            .trim_start_matches("tt")
            .trim_start_matches('0')
            .to_owned()
    };
    n(a) == n(b)
}

fn movie_expected(m: &MovieDto) -> ExpectedMovie {
    let mut alts = m.alternative_titles.clone();
    if let Some(o) = m.original_title.as_ref().filter(|o| **o != m.title) {
        alts.push(o.clone());
    }
    ExpectedMovie {
        title: m.title.clone(),
        alternative_titles: alts,
        year: m.year.and_then(|y| u16::try_from(y).ok()),
    }
}

/// Whether the release is `m`: indexer ids decide when present, else title + year.
pub fn map_to_movie(r: &ReleaseInfo, parsed: &ParsedRelease, m: &MovieDto) -> Result<(), String> {
    if parsed.episode().is_some() {
        return Err("Release is a TV episode, not a movie".into());
    }
    if let Some(t) = r.tmdb_id.filter(|t| *t > 0) {
        return if t == m.tmdb_id {
            Ok(())
        } else {
            Err(format!("Wrong movie: release is TMDB {t}"))
        };
    }
    if let (Some(a), Some(b)) = (r.imdb_id.as_deref(), m.imdb_id.as_deref()) {
        return if imdb_eq(a, b) {
            Ok(())
        } else {
            Err(format!("Wrong movie: release is IMDb {a}"))
        };
    }
    match_movie(parsed, &movie_expected(m))
        .map(|_| ())
        .map_err(|e| format!("Wrong movie: {e}"))
}

pub fn series_type(t: SeriesType) -> reelix_import::SeriesType {
    match t {
        SeriesType::Standard => reelix_import::SeriesType::Standard,
        SeriesType::Daily => reelix_import::SeriesType::Daily,
        SeriesType::Anime => reelix_import::SeriesType::Anime,
    }
}

pub fn expected_episodes(eps: &[EpisodeDto]) -> Vec<ExpectedEpisode> {
    eps.iter()
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

fn series_ids_match(r: &ReleaseInfo, s: &SeriesDto) -> Option<bool> {
    if let (Some(a), Some(b)) = (r.tvdb_id.filter(|v| *v > 0), s.tvdb_id) {
        return Some(a == b);
    }
    if let (Some(a), Some(b)) = (r.imdb_id.as_deref(), s.imdb_id.as_deref()) {
        return Some(imdb_eq(a, b));
    }
    None
}

/// Whether the release is series `s`; returns the episode ids it contains (in release order).
pub fn map_to_series(
    r: &ReleaseInfo,
    parsed: &ParsedRelease,
    s: &SeriesDto,
    episodes: &[ExpectedEpisode],
) -> Result<Vec<i64>, String> {
    if parsed.episode().is_none() {
        return Err("Release is not a TV episode or season".into());
    }
    match series_ids_match(r, s) {
        Some(true) => {}
        Some(false) => return Err("Wrong series (indexer ids differ)".into()),
        None => {
            if match_title(&parsed.title, &s.title, &s.alternative_titles).is_none() {
                return Err(format!("Wrong series: {:?} is not {:?}", parsed.title, s.title));
            }
        }
    }
    let m = match_parsed_episodes(parsed, series_type(s.series_type), episodes)
        .map_err(|e| format!("Unknown episode: {e}"))?;
    if !m.is_complete() {
        return Err(format!("Unknown episode(s): {}", m.unmatched.join(", ")));
    }
    Ok(m.episode_ids)
}

/// Where an RSS release maps in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    Movie(i64),
    Episodes { series_id: i64, episode_ids: Vec<i64> },
}

/// Lookup tables over monitored library media, for RSS sync.
#[derive(Default)]
pub struct MediaIndex {
    pub movies: Vec<MovieDto>,
    pub series: Vec<SeriesDto>,
    /// Series id → expected episodes.
    pub episodes: HashMap<i64, Vec<ExpectedEpisode>>,
    movie_tmdb: HashMap<u32, usize>,
    movie_imdb: HashMap<String, usize>,
    movie_title: HashMap<String, Vec<usize>>,
    series_tvdb: HashMap<u32, usize>,
    series_imdb: HashMap<String, usize>,
    series_title: HashMap<String, usize>,
}

fn imdb_key(s: &str) -> String {
    s.trim()
        .trim_start_matches("tt")
        .trim_start_matches('0')
        .to_owned()
}

impl MediaIndex {
    pub fn new(
        movies: Vec<MovieDto>,
        series: Vec<SeriesDto>,
        episodes: HashMap<i64, Vec<ExpectedEpisode>>,
    ) -> Self {
        let mut ix = Self {
            movies,
            series,
            episodes,
            ..Self::default()
        };
        for (i, m) in ix.movies.iter().enumerate() {
            ix.movie_tmdb.insert(m.tmdb_id, i);
            if let Some(id) = &m.imdb_id {
                ix.movie_imdb.insert(imdb_key(id), i);
            }
            let e = movie_expected(m);
            for t in std::iter::once(&e.title).chain(&e.alternative_titles) {
                let k = normalize_title(t);
                if !k.is_empty() {
                    let v = ix.movie_title.entry(k).or_default();
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
            }
        }
        for (i, s) in ix.series.iter().enumerate() {
            if let Some(t) = s.tvdb_id {
                ix.series_tvdb.insert(t, i);
            }
            if let Some(id) = &s.imdb_id {
                ix.series_imdb.insert(imdb_key(id), i);
            }
            for t in std::iter::once(&s.title).chain(&s.alternative_titles) {
                let k = normalize_title(t);
                if !k.is_empty() {
                    ix.series_title.entry(k).or_insert(i);
                }
            }
        }
        ix
    }

    pub fn movie(&self, id: i64) -> Option<&MovieDto> {
        self.movies.iter().find(|m| m.id == id)
    }

    pub fn series_by_id(&self, id: i64) -> Option<&SeriesDto> {
        self.series.iter().find(|s| s.id == id)
    }

    /// Maps a release to a library movie or episodes; `None` when nothing matches.
    pub fn map(&self, r: &ReleaseInfo, parsed: &ParsedRelease) -> Option<Mapped> {
        match &parsed.kind {
            ReleaseKind::Episode(_) => {
                let idx = r
                    .tvdb_id
                    .and_then(|t| self.series_tvdb.get(&t))
                    .or_else(|| {
                        r.imdb_id
                            .as_deref()
                            .and_then(|i| self.series_imdb.get(&imdb_key(i)))
                    })
                    .or_else(|| self.series_title.get(&normalize_title(&parsed.title)))?;
                let s = &self.series[*idx];
                let eps = self.episodes.get(&s.id).map(Vec::as_slice).unwrap_or_default();
                let ids = map_to_series(r, parsed, s, eps).ok()?;
                Some(Mapped::Episodes {
                    series_id: s.id,
                    episode_ids: ids,
                })
            }
            _ => {
                if let Some(i) = r.tmdb_id.and_then(|t| self.movie_tmdb.get(&t)).or_else(|| {
                    r.imdb_id
                        .as_deref()
                        .and_then(|i| self.movie_imdb.get(&imdb_key(i)))
                }) {
                    return Some(Mapped::Movie(self.movies[*i].id));
                }
                let cands = self.movie_title.get(&normalize_title(&parsed.title))?;
                let expected: Vec<ExpectedMovie> =
                    cands.iter().map(|i| movie_expected(&self.movies[*i])).collect();
                let (best, _) = best_movie_match(parsed, &expected)?;
                Some(Mapped::Movie(self.movies[cands[best]].id))
            }
        }
    }
}
