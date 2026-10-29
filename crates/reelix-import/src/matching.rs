//! Match parsed releases / file names to the expected movie or episodes.
//!
//! Uses a local title normaliser ([`normalize_title`]); it is intentionally independent
//! of `reelix_parser::clean_title` so the two can evolve separately.

use chrono::NaiveDate;
use reelix_parser::model::{EpisodeInfo, ParsedRelease};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::SeriesType;

const STOP_WORDS: &[&str] = &["the", "a", "an", "and"];

/// Comparison key for titles: diacritics stripped, lowercase, `&` → `and`, apostrophes
/// dropped, articles/`and` removed, everything but letters and digits removed.
/// `"The Lord of the Rings: The Return of the King"` → `"lordofringsreturnofking"`.
pub fn normalize_title(title: &str) -> String {
    let folded: String = title
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
        .replace('&', " and ")
        .replace(['\'', '’', '`', '´'], "");
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let kept: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| !STOP_WORDS.contains(w))
        .collect();
    if kept.is_empty() {
        words.concat()
    } else {
        kept.concat()
    }
}

/// Normalised Levenshtein similarity of two titles' keys (1.0 = identical).
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (
        normalize_title(a).chars().collect(),
        normalize_title(b).chars().collect(),
    );
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - levenshtein(&a, &b) as f64 / longest as f64
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Minimum [`similarity`] for a fuzzy title match.
pub const FUZZY_THRESHOLD: f64 = 0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleMatch {
    Exact,
    Alternative,
    Fuzzy,
}

/// How a title matched against the expected title and aliases.
pub fn match_title(found: &str, title: &str, alternatives: &[String]) -> Option<(TitleMatch, f64)> {
    let key = normalize_title(found);
    if key.is_empty() {
        return None;
    }
    if key == normalize_title(title) {
        return Some((TitleMatch::Exact, 1.0));
    }
    if alternatives.iter().any(|a| normalize_title(a) == key) {
        return Some((TitleMatch::Alternative, 1.0));
    }
    std::iter::once(title)
        .chain(alternatives.iter().map(String::as_str))
        .map(|t| similarity(found, t))
        .fold(None, |best: Option<f64>, s| Some(best.map_or(s, |b| b.max(s))))
        .filter(|s| *s >= FUZZY_THRESHOLD)
        .map(|s| (TitleMatch::Fuzzy, s))
}

/// The movie an import is expected to be.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExpectedMovie {
    pub title: String,
    pub alternative_titles: Vec<String>,
    pub year: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MovieMatch {
    pub title: TitleMatch,
    /// `Some(true)` exact year, `Some(false)` off by one, `None` no year in the name.
    pub year_exact: Option<bool>,
    /// 0..=1 confidence for ranking candidates.
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum MovieMismatch {
    #[error("release title {found:?} does not match {expected:?}")]
    Title { found: String, expected: String },
    #[error("release year {found} does not match {expected}")]
    Year { found: u16, expected: u16 },
}

/// Match a parsed release to an expected movie: title (exact, alias or fuzzy) and
/// year (±1 tolerated, since release and premiere years often differ).
pub fn match_movie(parsed: &ParsedRelease, expected: &ExpectedMovie) -> Result<MovieMatch, MovieMismatch> {
    let Some((title, sim)) = match_title(&parsed.title, &expected.title, &expected.alternative_titles) else {
        return Err(MovieMismatch::Title {
            found: parsed.title.clone(),
            expected: expected.title.clone(),
        });
    };
    let year_exact = match (parsed.year, expected.year) {
        (Some(f), Some(e)) if f == e => Some(true),
        (Some(f), Some(e)) if f.abs_diff(e) == 1 => Some(false),
        (Some(f), Some(e)) => {
            return Err(MovieMismatch::Year {
                found: f,
                expected: e,
            });
        }
        _ => None,
    };
    let year_factor = match year_exact {
        Some(true) => 1.0,
        Some(false) => 0.9,
        None => 0.8,
    };
    let title_factor = match title {
        TitleMatch::Exact => 1.0,
        TitleMatch::Alternative => 0.95,
        TitleMatch::Fuzzy => 0.85 * sim,
    };
    Ok(MovieMatch {
        title,
        year_exact,
        score: title_factor * year_factor,
    })
}

/// Best matching candidate (index + match), if any.
pub fn best_movie_match(parsed: &ParsedRelease, candidates: &[ExpectedMovie]) -> Option<(usize, MovieMatch)> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match_movie(parsed, c).ok().map(|m| (i, m)))
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
}

/// An episode of the series being imported (ids are opaque to this crate).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExpectedEpisode {
    pub id: i64,
    pub season: u32,
    pub episode: u32,
    pub absolute: Option<u32>,
    pub air_date: Option<NaiveDate>,
    /// Scene (XEM) numbering, when it differs from the metadata numbering.
    pub scene_season: Option<u32>,
    pub scene_episode: Option<u32>,
    pub scene_absolute: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeMatchMethod {
    SeasonEpisode,
    SceneSeasonEpisode,
    Absolute,
    SceneAbsolute,
    AirDate,
    FullSeason,
    SeasonRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeMatch {
    /// Matched episode ids in release order.
    pub episode_ids: Vec<i64>,
    pub method: EpisodeMatchMethod,
    /// Numbers named in the release that matched no episode (`S01E05`, `abs 13`).
    pub unmatched: Vec<String>,
}

impl EpisodeMatch {
    /// Every number in the release resolved to an episode.
    pub fn is_complete(&self) -> bool {
        self.unmatched.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum EpisodeMismatch {
    #[error("release is not a TV episode")]
    NotAnEpisode,
    #[error("release has no usable episode numbering")]
    NoNumbering,
    #[error("no episode matches {0}")]
    NotFound(String),
}

/// Match a parsed release to episodes. Order: air date (daily), absolute numbers
/// (scene first; anime or absolute-only names), season ranges, full seasons, then
/// season/episode (scene numbering first).
pub fn match_parsed_episodes(
    parsed: &ParsedRelease,
    series_type: SeriesType,
    episodes: &[ExpectedEpisode],
) -> Result<EpisodeMatch, EpisodeMismatch> {
    match parsed.episode() {
        Some(info) => match_episodes(info, series_type, episodes),
        None => Err(EpisodeMismatch::NotAnEpisode),
    }
}

pub fn match_episodes(
    info: &EpisodeInfo,
    series_type: SeriesType,
    episodes: &[ExpectedEpisode],
) -> Result<EpisodeMatch, EpisodeMismatch> {
    let ok = |ids: Vec<i64>, method, unmatched: Vec<String>| EpisodeMatch {
        episode_ids: ids,
        method,
        unmatched,
    };

    if let Some(date) = info.air_date {
        let ids: Vec<i64> = episodes
            .iter()
            .filter(|e| e.air_date == Some(date))
            .map(|e| e.id)
            .collect();
        if !ids.is_empty() {
            return Ok(ok(ids, EpisodeMatchMethod::AirDate, vec![]));
        }
        if series_type == SeriesType::Daily || info.season.is_none() {
            return Err(EpisodeMismatch::NotFound(date.to_string()));
        }
    }

    let try_absolute = !info.absolute_episodes.is_empty()
        && (series_type == SeriesType::Anime || info.season.is_none() || info.episodes.is_empty());
    if try_absolute {
        let mut ids = Vec::new();
        let mut unmatched = Vec::new();
        let mut used_scene = false;
        for abs in &info.absolute_episodes {
            if let Some(e) = episodes.iter().find(|e| e.scene_absolute == Some(*abs)) {
                used_scene = true;
                ids.push(e.id);
            } else if let Some(e) = episodes
                .iter()
                .find(|e| e.absolute == Some(*abs) && e.scene_absolute.is_none())
            {
                ids.push(e.id);
            } else {
                unmatched.push(format!("abs {abs}"));
            }
        }
        if !ids.is_empty() {
            let method = if used_scene {
                EpisodeMatchMethod::SceneAbsolute
            } else {
                EpisodeMatchMethod::Absolute
            };
            return Ok(ok(ids, method, unmatched));
        }
        if info.season.is_none() || info.episodes.is_empty() {
            return Err(EpisodeMismatch::NotFound(unmatched.join(", ")));
        }
    }

    if let Some((from, to)) = info.season_range {
        let ids: Vec<i64> = episodes
            .iter()
            .filter(|e| e.season >= from.max(1) && e.season <= to)
            .map(|e| e.id)
            .collect();
        return if ids.is_empty() {
            Err(EpisodeMismatch::NotFound(format!("S{from:02}-S{to:02}")))
        } else {
            Ok(ok(ids, EpisodeMatchMethod::SeasonRange, vec![]))
        };
    }

    let Some(season) = info.season else {
        return Err(EpisodeMismatch::NoNumbering);
    };

    if info.full_season && info.episodes.is_empty() {
        let ids: Vec<i64> = episodes
            .iter()
            .filter(|e| season_of(e) == season)
            .map(|e| e.id)
            .collect();
        return if ids.is_empty() {
            Err(EpisodeMismatch::NotFound(format!("S{season:02}")))
        } else {
            Ok(ok(ids, EpisodeMatchMethod::FullSeason, vec![]))
        };
    }
    if info.episodes.is_empty() {
        return Err(EpisodeMismatch::NoNumbering);
    }

    let mut ids = Vec::new();
    let mut unmatched = Vec::new();
    let mut used_scene = false;
    for ep in &info.episodes {
        let scene = episodes
            .iter()
            .find(|e| e.scene_season == Some(season) && e.scene_episode == Some(*ep));
        // Episodes with their own scene mapping are only addressable via that mapping.
        let direct = || {
            episodes
                .iter()
                .find(|e| e.season == season && e.episode == *ep && e.scene_season.is_none())
        };
        match scene.map(|e| (e, true)).or_else(|| direct().map(|e| (e, false))) {
            Some((e, is_scene)) => {
                used_scene |= is_scene;
                ids.push(e.id);
            }
            None => unmatched.push(format!("S{season:02}E{ep:02}")),
        }
    }
    if ids.is_empty() {
        return Err(EpisodeMismatch::NotFound(unmatched.join(", ")));
    }
    let method = if used_scene {
        EpisodeMatchMethod::SceneSeasonEpisode
    } else {
        EpisodeMatchMethod::SeasonEpisode
    };
    Ok(ok(ids, method, unmatched))
}

fn season_of(e: &ExpectedEpisode) -> u32 {
    e.scene_season.unwrap_or(e.season)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliser() {
        assert_eq!(
            normalize_title("The Lord of the Rings: The Return of the King"),
            "lordofringsreturnofking"
        );
        assert_eq!(normalize_title("Amélie"), "amelie");
        assert_eq!(normalize_title("Law & Order"), normalize_title("Law and Order"));
        assert_eq!(normalize_title("Schindler's List"), "schindlerslist");
        assert_eq!(normalize_title("The"), "the");
        assert_eq!(
            normalize_title("Spider-Man: No Way Home"),
            normalize_title("Spider Man No Way Home")
        );
    }

    #[test]
    fn similarity_bounds() {
        assert_eq!(similarity("Inception", "inception"), 1.0);
        assert!(similarity("The Shawshank Redemption", "Shawshank Redemtion") >= FUZZY_THRESHOLD);
        assert!(similarity("Alien", "Aliens") < FUZZY_THRESHOLD);
    }
}
