//! Indexer-facing domain types: releases, queries and capabilities.

use chrono::{DateTime, Utc};
use reelix_core::{BookFormat, ContentKind, Protocol};
use serde::{Deserialize, Serialize};

/// Flags an indexer attaches to a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexerFlag {
    Freeleech,
    Halfleech,
    DoubleUpload,
    Internal,
    Scene,
    Nuked,
}

impl IndexerFlag {
    /// Parses a Torznab `tag` value or a definition flag name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace(['-', '_', ' '], "")
                .as_str()
            {
                "freeleech" | "fl" => Self::Freeleech,
                "halfleech" => Self::Halfleech,
                "doubleupload" => Self::DoubleUpload,
                "internal" => Self::Internal,
                "scene" => Self::Scene,
                "nuked" => Self::Nuked,
                _ => return None,
            },
        )
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseInfo {
    /// Stable unique id from the indexer (falls back to the details/download URL).
    pub guid: String,
    pub title: String,
    /// `.torrent` / `.nzb` URL; fetch it with [`crate::Indexer::download`].
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    /// Human-facing details page.
    pub info_url: Option<String>,
    /// Lower-case hex BitTorrent info hash.
    pub info_hash: Option<String>,
    pub size: Option<u64>,
    pub publish_date: Option<DateTime<Utc>>,
    pub protocol: Protocol,
    pub indexer_id: i64,
    pub indexer: String,
    /// Newznab category ids (standard ids, plus site-specific ≥ 100000).
    pub categories: Vec<u32>,
    pub seeders: Option<u32>,
    /// Total peers (seeders + leechers).
    pub peers: Option<u32>,
    pub grabs: Option<u32>,
    pub files: Option<u32>,
    /// `tt`-prefixed IMDb id.
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    /// 0 = freeleech, 0.5 = half, 1 = normal.
    pub download_volume_factor: f64,
    pub upload_volume_factor: f64,
    pub minimum_ratio: Option<f64>,
    /// Seconds.
    pub minimum_seed_time: Option<u64>,
    pub indexer_flags: Vec<IndexerFlag>,
}

impl ReleaseInfo {
    /// An empty release for the given indexer; fields are filled in by the adapters.
    pub fn new(protocol: Protocol, indexer_id: i64, indexer: &str) -> Self {
        Self {
            guid: String::new(),
            title: String::new(),
            download_url: None,
            magnet_url: None,
            info_url: None,
            info_hash: None,
            size: None,
            publish_date: None,
            protocol,
            indexer_id,
            indexer: indexer.into(),
            categories: Vec::new(),
            seeders: None,
            peers: None,
            grabs: None,
            files: None,
            imdb_id: None,
            tmdb_id: None,
            tvdb_id: None,
            download_volume_factor: 1.0,
            upload_volume_factor: 1.0,
            minimum_ratio: None,
            minimum_seed_time: None,
            indexer_flags: Vec::new(),
        }
    }

    /// Derives freeleech/halfleech/double-upload flags from the volume factors.
    pub(crate) fn finish(&mut self) {
        let mut add = |f: IndexerFlag| {
            if !self.indexer_flags.contains(&f) {
                self.indexer_flags.push(f);
            }
        };
        if self.download_volume_factor == 0.0 {
            add(IndexerFlag::Freeleech);
        } else if self.download_volume_factor == 0.5 {
            add(IndexerFlag::Halfleech);
        }
        if self.upload_volume_factor >= 2.0 {
            add(IndexerFlag::DoubleUpload);
        }
        if let Some(h) = &mut self.info_hash {
            h.make_ascii_lowercase();
        }
        if self.guid.is_empty() {
            self.guid = self
                .info_url
                .clone()
                .or_else(|| self.download_url.clone())
                .or_else(|| self.magnet_url.clone())
                .unwrap_or_else(|| self.title.clone());
        }
        if let Some(id) = &self.imdb_id {
            self.imdb_id = normalize_imdb(id);
        }
    }
}

/// `"1234567"`, `"tt1234567"` → `Some("tt1234567")`; zero/garbage → `None`.
pub fn normalize_imdb(s: &str) -> Option<String> {
    let digits = s.trim().trim_start_matches("tt");
    let n: u64 = digits.parse().ok()?;
    (n > 0).then(|| format!("tt{n:07}"))
}

/// What kind of search to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchKind {
    #[default]
    Basic,
    Movie,
    Tv,
    /// Ebook or audiobook (`t=book`).
    Book,
    /// Album (`t=music`).
    Music,
}

/// A search request. An empty basic query is an RSS/latest query.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub kind: SearchKind,
    pub term: Option<String>,
    /// With or without the `tt` prefix.
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub year: Option<u16>,
    pub season: Option<u32>,
    /// Episode number, or `MM/DD` for daily shows.
    pub episode: Option<String>,
    /// Newznab category ids; empty means "indexer default".
    pub categories: Vec<u32>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    /// Book search: author name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Book search: book title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Music search: artist name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Music search: album title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Book search: selects the indexer's ebook or audiobook categories (`None` = both).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book_format: Option<BookFormat>,
}

impl SearchQuery {
    /// Free-text search.
    pub fn basic(term: impl Into<String>) -> Self {
        Self {
            term: Some(term.into()),
            ..Self::default()
        }
    }

    /// Movie search; set ids/year/term on the returned value.
    pub fn movie() -> Self {
        Self {
            kind: SearchKind::Movie,
            ..Self::default()
        }
    }

    /// TV search; set ids/season/episode/term on the returned value.
    pub fn tv() -> Self {
        Self {
            kind: SearchKind::Tv,
            ..Self::default()
        }
    }

    /// Book search by author and title (either may be empty); set `book_format`/`year` on the
    /// returned value.
    pub fn book(author: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: SearchKind::Book,
            author: non_empty(author.into()),
            title: non_empty(title.into()),
            ..Self::default()
        }
    }

    /// Album search by artist and album (either may be empty); set `year` on the returned value.
    pub fn music(artist: impl Into<String>, album: impl Into<String>) -> Self {
        Self {
            kind: SearchKind::Music,
            artist: non_empty(artist.into()),
            album: non_empty(album.into()),
            ..Self::default()
        }
    }

    /// The content this query looks for (`None` for basic searches and format-less book searches).
    pub fn content_kind(&self) -> Option<ContentKind> {
        match self.kind {
            SearchKind::Basic => None,
            SearchKind::Movie => Some(ContentKind::Movie),
            SearchKind::Tv => Some(ContentKind::Series),
            SearchKind::Book => self.book_format.map(BookFormat::content_kind),
            SearchKind::Music => Some(ContentKind::Music),
        }
    }

    /// The IMDb id without `tt` and leading zeros preserved (`0133093`).
    pub fn imdb_digits(&self) -> Option<String> {
        normalize_imdb(self.imdb_id.as_deref()?).map(|s| s[2..].to_string())
    }

    /// Term plus a scene-style episode token (`S01E02`, `S01`), used where the indexer
    /// has no structured season/episode parameters. Book searches give `"{author} {title}"`,
    /// music searches `"{artist} {album}"` (each falling back to the term when both are empty).
    pub fn keywords(&self) -> String {
        match self.kind {
            SearchKind::Book => return self.joined(&[&self.author, &self.title]),
            SearchKind::Music => return self.joined(&[&self.artist, &self.album]),
            _ => {}
        }
        let mut k = self.term.clone().unwrap_or_default().trim().to_string();
        if self.kind == SearchKind::Tv {
            let token = match (self.season, &self.episode) {
                (Some(s), Some(e)) if e.contains('/') => format!("{s} {}", e.replace('/', " ")),
                (Some(s), Some(e)) => match e.parse::<u32>() {
                    Ok(n) => format!("S{s:02}E{n:02}"),
                    Err(_) => format!("S{s:02}E{e}"),
                },
                (Some(s), None) => format!("S{s:02}"),
                _ => String::new(),
            };
            if !token.is_empty() {
                if !k.is_empty() {
                    k.push(' ');
                }
                k.push_str(&token);
            }
        }
        k
    }

    /// Non-empty parts joined by a space; the term when every part is empty.
    pub(crate) fn joined(&self, parts: &[&Option<String>]) -> String {
        let v: Vec<&str> = parts
            .iter()
            .filter_map(|p| p.as_deref().map(str::trim))
            .filter(|p| !p.is_empty())
            .collect();
        if v.is_empty() {
            self.term.as_deref().unwrap_or_default().trim().to_string()
        } else {
            v.join(" ")
        }
    }

    /// True for an RSS-style "give me the latest" query.
    pub fn is_rss(&self) -> bool {
        self.kind == SearchKind::Basic && self.term.as_deref().is_none_or(|t| t.trim().is_empty())
    }
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A parameter a search mode accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchParam {
    Q,
    ImdbId,
    TmdbId,
    TvdbId,
    TvMazeId,
    Season,
    Ep,
    Year,
    Genre,
    Author,
    Title,
    Artist,
    Album,
}

impl SearchParam {
    /// Parses a Newznab `supportedParams` token.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "q" => Self::Q,
            "imdbid" => Self::ImdbId,
            "tmdbid" => Self::TmdbId,
            "tvdbid" => Self::TvdbId,
            "tvmazeid" => Self::TvMazeId,
            "season" => Self::Season,
            "ep" => Self::Ep,
            "year" => Self::Year,
            "genre" => Self::Genre,
            "author" => Self::Author,
            "title" => Self::Title,
            "artist" => Self::Artist,
            "album" => Self::Album,
            _ => return None,
        })
    }

    /// Parses a comma-separated list, ignoring unknown tokens.
    pub fn parse_list(s: &str) -> Vec<Self> {
        let mut v: Vec<Self> = s.split(',').filter_map(Self::parse).collect();
        v.dedup();
        v
    }
}

/// Availability and parameters of one search mode.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchMode {
    pub available: bool,
    pub params: Vec<SearchParam>,
}

impl SearchMode {
    pub fn new(params: &[SearchParam]) -> Self {
        Self {
            available: true,
            params: params.to_vec(),
        }
    }

    pub fn supports(&self, p: SearchParam) -> bool {
        self.available && self.params.contains(&p)
    }
}

/// A category node (standard Newznab or site-specific).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcategories: Vec<Category>,
}

impl Category {
    pub fn new(id: u32, name: &str) -> Self {
        Self {
            id,
            name: name.into(),
            subcategories: Vec::new(),
        }
    }
}

/// What an indexer can do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexerCapabilities {
    pub search: SearchMode,
    pub tv_search: SearchMode,
    pub movie_search: SearchMode,
    /// `<book-search>` (Cardigann `book-search`).
    #[serde(default)]
    pub book_search: SearchMode,
    /// `<audio-search>` / `<music-search>` (Cardigann `music-search`).
    #[serde(default)]
    pub audio_search: SearchMode,
    pub categories: Vec<Category>,
    pub limit_default: Option<u32>,
    pub limit_max: Option<u32>,
}

impl Default for IndexerCapabilities {
    fn default() -> Self {
        Self {
            search: SearchMode::new(&[SearchParam::Q]),
            tv_search: SearchMode::default(),
            movie_search: SearchMode::default(),
            book_search: SearchMode::default(),
            audio_search: SearchMode::default(),
            categories: Vec::new(),
            limit_default: None,
            limit_max: None,
        }
    }
}

impl IndexerCapabilities {
    /// Mode used for a given query kind.
    pub fn mode(&self, kind: SearchKind) -> &SearchMode {
        match kind {
            SearchKind::Basic => &self.search,
            SearchKind::Movie => &self.movie_search,
            SearchKind::Tv => &self.tv_search,
            SearchKind::Book => &self.book_search,
            SearchKind::Music => &self.audio_search,
        }
    }

    /// All category ids, flattened.
    pub fn category_ids(&self) -> Vec<u32> {
        fn walk(c: &[Category], out: &mut Vec<u32>) {
            for c in c {
                out.push(c.id);
                walk(&c.subcategories, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.categories, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_imdb() {
        let q = SearchQuery {
            term: Some("Show".into()),
            season: Some(1),
            episode: Some("2".into()),
            ..SearchQuery::tv()
        };
        assert_eq!(q.keywords(), "Show S01E02");
        let q = SearchQuery {
            imdb_id: Some("133093".into()),
            ..SearchQuery::movie()
        };
        assert_eq!(q.imdb_digits().as_deref(), Some("0133093"));
        assert_eq!(normalize_imdb("tt0000000"), None);
        assert!(SearchQuery::default().is_rss());
        assert_eq!(
            SearchParam::parse_list("q,imdbid, Season,foo"),
            [SearchParam::Q, SearchParam::ImdbId, SearchParam::Season]
        );
    }

    #[test]
    fn book_and_music_keywords() {
        let q = SearchQuery::book("Frank Herbert", " Dune ");
        assert_eq!(q.keywords(), "Frank Herbert Dune");
        assert_eq!(q.content_kind(), None);
        let q = SearchQuery {
            book_format: Some(BookFormat::Audiobook),
            ..SearchQuery::book("", "Dune")
        };
        assert_eq!((q.author.as_deref(), q.keywords().as_str()), (None, "Dune"));
        assert_eq!(q.content_kind(), Some(ContentKind::Audiobook));
        let q = SearchQuery::music("Radiohead", "OK Computer");
        assert_eq!(q.keywords(), "Radiohead OK Computer");
        assert_eq!(q.content_kind(), Some(ContentKind::Music));
        assert!(!q.is_rss());
        let q = SearchQuery {
            term: Some("fallback".into()),
            ..SearchQuery::music("", "")
        };
        assert_eq!(q.keywords(), "fallback");
        assert_eq!(
            SearchParam::parse_list("q,author,title,artist,album,year"),
            [
                SearchParam::Q,
                SearchParam::Author,
                SearchParam::Title,
                SearchParam::Artist,
                SearchParam::Album,
                SearchParam::Year
            ]
        );
        let caps = IndexerCapabilities::default();
        assert!(!caps.mode(SearchKind::Book).available && !caps.mode(SearchKind::Music).available);
    }
}
