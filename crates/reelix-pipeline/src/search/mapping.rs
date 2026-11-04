//! Turning indexer results into decision candidates and mapping them to library media:
//! indexer ids (TMDB / IMDb / TVDB) first, then the parsed title (+ alternative titles and
//! year) and, for TV, the parsed episode numbering.

use std::collections::HashMap;

use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto, SeriesType};
use reelix_books::BookTarget;
use reelix_core::{BookFormat, ContentKind};
use reelix_decision::{IndexerFlag, ReleaseCandidate};
use reelix_import::matching::similarity;
use reelix_import::matching::{
    ExpectedEpisode, ExpectedMovie, best_movie_match, match_movie, match_parsed_episodes, match_title,
    normalize_title,
};
use reelix_indexers::ReleaseInfo;
use reelix_indexers::categories::content_kind_of;
use reelix_music::AlbumTarget;
use reelix_parser::{ParsedBook, ParsedMusic, ParsedRelease, QualityFamily, ReleaseKind};

use super::context::Target;

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
    fill(ReleaseCandidate::new(&r.title, r.protocol), r, priority)
}

/// A candidate parsed for the target's content: book releases with the book parser (quality of
/// the format's family), album releases with the music parser, everything else as [`candidate`].
pub fn candidate_for(r: &ReleaseInfo, priority: i32, t: &Target) -> ReleaseCandidate {
    let base = match t {
        Target::Book(b) => ReleaseCandidate::for_book(&r.title, r.protocol, b.format),
        Target::Album(_) => ReleaseCandidate::for_album(&r.title, r.protocol),
        _ => return candidate(r, priority),
    };
    fill(base, r, priority)
}

fn fill(mut c: ReleaseCandidate, r: &ReleaseInfo, priority: i32) -> ReleaseCandidate {
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

// ---------------------------------------------------------------------------------------------
// Books and albums
// ---------------------------------------------------------------------------------------------

fn kind_label(k: ContentKind) -> &'static str {
    match k {
        ContentKind::Movie => "a movie",
        ContentKind::Series => "a TV",
        ContentKind::Ebook => "an ebook",
        ContentKind::Audiobook => "an audiobook",
        ContentKind::Music => "a music",
        ContentKind::Radio => "a radio",
    }
}

/// The content kind of a release from its indexer categories (`None` = unknown / no categories).
pub fn release_kind(r: &ReleaseInfo) -> Option<ContentKind> {
    content_kind_of(&r.categories)
}

/// Whether a book release is `t` (one format of a book): category and format must fit, then
/// ISBN/ASIN, else author + title.
pub fn map_to_book(r: &ReleaseInfo, parsed: &ParsedBook, t: &BookTarget) -> Result<(), String> {
    let want = t.format.content_kind();
    if let Some(k) = release_kind(r).filter(|k| *k != want) {
        return Err(format!("Release is in {} category", kind_label(k)));
    }
    let other = match t.format {
        BookFormat::Ebook => BookFormat::Audiobook,
        BookFormat::Audiobook => BookFormat::Ebook,
    };
    if parsed.format == Some(other) || parsed.quality.quality.family() == QualityFamily::of_book(other) {
        return Err(format!(
            "Release is {}, not {}",
            kind_label(other.content_kind()),
            kind_label(want)
        ));
    }
    let id_eq = |a: Option<&str>, b: Option<&str>| {
        let n = |s: &str| {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase()
        };
        matches!((a, b), (Some(a), Some(b)) if !n(a).is_empty() && n(a) == n(b))
    };
    if id_eq(parsed.isbn.as_deref(), t.book.isbn13.as_deref())
        || id_eq(parsed.asin.as_deref(), t.book.asin.as_deref())
    {
        return Ok(());
    }
    let book = normalize_title(&t.book.title);
    let author = normalize_title(&t.author.name);
    let title = normalize_title(&parsed.title);
    let raw = normalize_title(&parsed.raw_title);
    let author_ok = match parsed
        .author
        .as_deref()
        .map(normalize_title)
        .filter(|a| !a.is_empty())
    {
        Some(a) => {
            a == author
                || a.contains(&author)
                || (author.contains(&a) && a.len() >= 4)
                || similarity(&a, &author) >= 0.9
        }
        None => author.is_empty() || raw.contains(&author),
    };
    if !author_ok {
        return Err(format!(
            "Wrong author: {:?} is not by {}",
            parsed.raw_title, t.author.name
        ));
    }
    let title_ok = !book.is_empty()
        && if parsed.author.is_some() {
            // The parser split author and title: the title must be the book's.
            title == book || similarity(&title, &book) >= 0.9
        } else {
            title == book || (book.len() >= 4 && raw.contains(&book))
        };
    if !title_ok {
        return Err(format!(
            "Wrong book: {:?} is not {:?}",
            parsed.title, t.book.title
        ));
    }
    if parsed.is_collection {
        return Err("Release is a collection of several books".into());
    }
    Ok(())
}

/// Whether a music release is album `t`: category, then artist and album title (editions such as
/// "Deluxe" are tolerated); discography packs are rejected.
pub fn map_to_album(r: &ReleaseInfo, parsed: &ParsedMusic, t: &AlbumTarget) -> Result<(), String> {
    if let Some(k) = release_kind(r).filter(|k| *k != ContentKind::Music) {
        return Err(format!("Release is in {} category", kind_label(k)));
    }
    if parsed.is_discography {
        return Err("Discography packs are not grabbed for a single album".into());
    }
    let artist = normalize_title(&t.artist.name);
    let raw = normalize_title(&parsed.raw_title);
    let artist_ok = match parsed
        .artist
        .as_deref()
        .map(normalize_title)
        .filter(|a| !a.is_empty())
    {
        Some(a) => a == artist || similarity(&a, &artist) >= 0.9,
        None => !artist.is_empty() && raw.contains(&artist),
    };
    if !artist_ok {
        return Err(format!(
            "Wrong artist: {:?} is not by {}",
            parsed.raw_title, t.artist.name
        ));
    }
    let album = normalize_title(&t.album.title);
    let found = parsed.album.as_deref().unwrap_or(parsed.raw_title.as_str());
    let mut keys = vec![normalize_title(found)];
    if let Some(e) = &parsed.edition {
        keys.push(normalize_title(&found.replace(e.as_str(), "")));
    }
    let album_ok = !album.is_empty()
        && keys.iter().any(|k| {
            *k == album
                || similarity(k, &album) >= 0.85
                || (parsed.album.is_none() && album.len() >= 4 && k.contains(&album))
        });
    if !album_ok {
        return Err(format!("Wrong album: {found:?} is not {:?}", t.album.title));
    }
    Ok(())
}
