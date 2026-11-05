//! Indexer queries per search target.

use chrono::Datelike;
use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto, SeriesType};
use reelix_indexers::SearchQuery;

/// Primary movie query: ids + title + year (indexers use what they support).
pub fn movie(m: &MovieDto) -> SearchQuery {
    let mut q = SearchQuery::movie();
    q.imdb_id = m.imdb_id.clone();
    q.tmdb_id = Some(m.tmdb_id);
    q.term = Some(m.title.clone());
    q.year = m.year.and_then(|y| u16::try_from(y).ok());
    q
}

/// Title-only fallbacks (alternative / original titles), used when the primary query finds nothing.
pub fn movie_fallbacks(m: &MovieDto) -> Vec<SearchQuery> {
    let mut titles: Vec<String> = Vec::new();
    for t in m.original_title.iter().chain(&m.alternative_titles) {
        let key = reelix_parser::clean_title(t);
        if *t != m.title
            && !key.is_empty()
            && key != reelix_parser::clean_title(&m.title)
            && !titles.iter().any(|x| reelix_parser::clean_title(x) == key)
        {
            titles.push(t.clone());
        }
    }
    titles
        .into_iter()
        .take(3)
        .map(|t| {
            let mut q = SearchQuery::movie();
            q.term = Some(t);
            q.year = m.year.and_then(|y| u16::try_from(y).ok());
            q
        })
        .collect()
}

fn tv_base(s: &SeriesDto) -> SearchQuery {
    let mut q = SearchQuery::tv();
    q.tvdb_id = s.tvdb_id;
    q.tmdb_id = s.tmdb_id;
    q.imdb_id = s.imdb_id.clone();
    q.term = Some(s.title.clone());
    q
}

/// Season-pack query.
pub fn season(s: &SeriesDto, season: u32, eps: &[EpisodeDto]) -> SearchQuery {
    let mut q = tv_base(s);
    // Scene numbering: use the scene season when every episode of the season maps to one.
    let scene: Vec<u32> = eps
        .iter()
        .filter(|e| e.season_number == season)
        .filter_map(|e| e.scene_season_number)
        .collect();
    q.season = Some(match scene.first() {
        Some(first) if scene.iter().all(|x| x == first) => *first,
        _ => season,
    });
    q
}

/// Queries for one episode: structured season/episode (scene numbering when present), air date
/// for daily shows, plus an absolute-number term search for anime.
pub fn episode(s: &SeriesDto, e: &EpisodeDto) -> Vec<SearchQuery> {
    let mut out = Vec::new();
    let mut q = tv_base(s);
    match (s.series_type, e.air_date) {
        (SeriesType::Daily, Some(d)) => {
            q.season = Some(d.year() as u32);
            q.episode = Some(format!("{:02}/{:02}", d.month(), d.day()));
        }
        _ => {
            q.season = Some(e.scene_season_number.unwrap_or(e.season_number));
            q.episode = Some(e.scene_episode_number.unwrap_or(e.episode_number).to_string());
        }
    }
    out.push(q);
    if s.series_type == SeriesType::Anime
        && let Some(abs) = e.scene_absolute_number.or(e.absolute_number)
    {
        out.push(SearchQuery::basic(format!("{} {abs:02}", s.title)));
    }
    out
}

/// Book query for one format: author + title (+ year).
pub fn book(t: &reelix_books::BookTarget) -> SearchQuery {
    let mut q = SearchQuery::book(t.author.name.clone(), t.book.title.clone());
    q.book_format = Some(t.format);
    q.year = t.book.year.and_then(|y| u16::try_from(y).ok());
    q
}

/// Title-only book query (used when author + title finds nothing).
pub fn book_fallback(t: &reelix_books::BookTarget) -> SearchQuery {
    let mut q = SearchQuery::book(String::new(), t.book.title.clone());
    q.book_format = Some(t.format);
    q
}

/// Album query: artist + album title (+ year).
pub fn album(t: &reelix_music::AlbumTarget) -> SearchQuery {
    let mut q = SearchQuery::music(t.artist.name.clone(), t.album.title.clone());
    q.year = t.album.release_date.and_then(|d| u16::try_from(d.year()).ok());
    q
}

/// Radio episode queries in the radio categories (Audio/Other): `Show S20E03`, the broadcast
/// date and the episode title ([`reelix_radio::RadioTarget::search_terms`]).
pub fn radio(t: &reelix_radio::RadioTarget) -> Vec<SearchQuery> {
    t.search_terms()
        .into_iter()
        .map(|term| {
            let mut q = SearchQuery::basic(term);
            q.categories = reelix_indexers::categories::DEFAULT_RADIO.to_vec();
            q
        })
        .collect()
}
