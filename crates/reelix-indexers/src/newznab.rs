//! Newznab (usenet) and Torznab (torrent) API adapters: caps parsing, `t=search|movie|tvsearch|book|music`,
//! `newznab:attr`/`torznab:attr` attributes and API error codes.

use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{BookFormat, Protocol};
use serde_json::Value;
use tokio::sync::OnceCell;
use url::Url;

use crate::http::{AddressGuard, build_client, status_error};
use crate::model::{
    Category, IndexerCapabilities, ReleaseInfo, SearchKind, SearchMode, SearchParam, SearchQuery,
};
use crate::util::{SettingsExt, parse_date};
use crate::xml::{self, Node};
use crate::{BuildContext, Indexer, IndexerError, IndexerFlag, categories};

/// Which dialect of the API is spoken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Newznab,
    Torznab,
}

impl Flavor {
    fn protocol(self) -> Protocol {
        match self {
            Self::Newznab => Protocol::Usenet,
            Self::Torznab => Protocol::Torrent,
        }
    }
}

/// Settings schema for `newznab` / `torznab`.
pub fn schema(flavor: Flavor) -> ProviderSchema {
    let (implementation, name, cats) = match flavor {
        Flavor::Newznab => ("newznab", "Newznab", vec![2000, 5000]),
        Flavor::Torznab => ("torznab", "Torznab", vec![2000, 5000]),
    };
    let mut key = FieldSchema::new("api_key", "API key", FieldKind::Secret);
    if flavor == Flavor::Newznab {
        key = key.required();
    }
    ProviderSchema {
        implementation: implementation.into(),
        name: name.into(),
        description: Some(match flavor {
            Flavor::Newznab => "Any Newznab-compatible usenet indexer".into(),
            Flavor::Torznab => {
                "Any Torznab-compatible torrent indexer or proxy (Jackett, Prowlarr, …)".into()
            }
        }),
        info_url: None,
        fields: vec![
            FieldSchema::new("base_url", "URL", FieldKind::Url)
                .required()
                .placeholder("https://indexer.example"),
            FieldSchema::new("api_path", "API path", FieldKind::Text)
                .default_value("/api")
                .help("Path to the API, usually /api")
                .advanced(),
            key,
            FieldSchema::new("categories", "Categories", FieldKind::List)
                .default_value(cats)
                .help("Newznab category ids searched by default"),
            FieldSchema::new("book_categories", "Ebook categories", FieldKind::List)
                .default_value(categories::DEFAULT_EBOOK.to_vec())
                .help("Categories for ebook searches (7000 Books, 7020 EBook; add 7010 for magazines)")
                .advanced(),
            FieldSchema::new("audiobook_categories", "Audiobook categories", FieldKind::List)
                .default_value(categories::DEFAULT_AUDIOBOOK.to_vec())
                .help("Categories for audiobook searches (3030 Audio/Audiobook)")
                .advanced(),
            FieldSchema::new("music_categories", "Music categories", FieldKind::List)
                .default_value(categories::DEFAULT_MUSIC.to_vec())
                .help("Categories for music searches (3000 Audio, 3010 MP3, 3040 Lossless)")
                .advanced(),
            FieldSchema::new("additional_parameters", "Additional parameters", FieldKind::Text)
                .help("Appended to every API request, e.g. &attrs=poster")
                .advanced(),
        ],
    }
}

/// A Newznab or Torznab indexer.
pub struct NewznabIndexer {
    flavor: Flavor,
    indexer_id: i64,
    name: String,
    api: Url,
    api_key: Option<String>,
    categories: Vec<u32>,
    book_categories: Vec<u32>,
    audiobook_categories: Vec<u32>,
    music_categories: Vec<u32>,
    extra: Vec<(String, String)>,
    client: reqwest::Client,
    guard: AddressGuard,
    caps: OnceCell<IndexerCapabilities>,
}

impl NewznabIndexer {
    /// Validates `settings` against [`schema`] and builds the adapter.
    pub fn from_settings(flavor: Flavor, settings: &Value, ctx: &BuildContext) -> Result<Self, IndexerError> {
        let s = schema(flavor).validate(settings).map_err(IndexerError::Config)?;
        let base = s.str_req("base_url")?;
        let mut api = Url::parse(&base).map_err(|e| IndexerError::Config(format!("URL: {e}")))?;
        let path = s.str_opt("api_path").unwrap_or_else(|| "/api".into());
        let joined = format!(
            "{}/{}",
            api.path().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        api.set_path(&joined);
        let guard = ctx.guard();
        guard.check_literal(&api)?;
        let extra = s
            .str_opt("additional_parameters")
            .map(|p| {
                url::form_urlencoded::parse(p.trim_start_matches(['&', '?']).as_bytes())
                    .into_owned()
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            flavor,
            indexer_id: ctx.indexer_id,
            name: ctx.name.clone(),
            api,
            api_key: s.str_opt("api_key"),
            categories: s.u32_list("categories"),
            book_categories: s.u32_list("book_categories"),
            audiobook_categories: s.u32_list("audiobook_categories"),
            music_categories: s.u32_list("music_categories"),
            extra,
            client: build_client(&ctx.http_options(), true)?,
            guard,
            caps: OnceCell::new(),
        })
    }

    fn url(&self, params: &[(&str, String)]) -> Url {
        let mut u = self.api.clone();
        {
            let mut q = u.query_pairs_mut();
            for (k, v) in params {
                q.append_pair(k, v);
            }
            if let Some(k) = &self.api_key {
                q.append_pair("apikey", k);
            }
            for (k, v) in &self.extra {
                q.append_pair(k, v);
            }
        }
        u
    }

    async fn get_xml(&self, url: Url) -> Result<Node, IndexerError> {
        self.guard.check(&url).await?;
        let resp = self.client.get(url).send().await?;
        let status_err = status_error(&resp);
        let body = resp.text().await?;
        // Many indexers send a Newznab <error> document with a non-200 status; prefer it.
        if let Ok(root) = xml::parse(&body) {
            check_error(&root)?;
            if let Some(e) = status_err {
                return Err(e);
            }
            return Ok(root);
        }
        Err(status_err.unwrap_or_else(|| IndexerError::Parse("response is not XML".into())))
    }

    /// Categories searched for a query: the query's own, else the per-kind setting.
    pub(crate) fn categories_for(&self, q: &SearchQuery) -> Vec<u32> {
        if !q.categories.is_empty() {
            return q.categories.clone();
        }
        let mut v = match (q.kind, q.book_format) {
            (SearchKind::Book, Some(BookFormat::Ebook)) => self.book_categories.clone(),
            (SearchKind::Book, Some(BookFormat::Audiobook)) => self.audiobook_categories.clone(),
            (SearchKind::Book, None) => {
                let mut v = self.book_categories.clone();
                v.extend(&self.audiobook_categories);
                v
            }
            (SearchKind::Music, _) => self.music_categories.clone(),
            _ => return self.categories.clone(),
        };
        if v.is_empty() {
            v = categories::defaults_for(q.content_kind()).to_vec();
        }
        let mut seen = Vec::new();
        v.retain(|c| {
            let new = !seen.contains(c);
            seen.push(*c);
            new
        });
        v
    }

    /// Builds the API parameters for a query, or `None` when the indexer can't serve it.
    fn params(&self, q: &SearchQuery, caps: &IndexerCapabilities) -> Option<Vec<(&'static str, String)>> {
        let mode = caps.mode(q.kind);
        let structured = q.kind != SearchKind::Basic && mode.available;
        let mut p: Vec<(&'static str, String)> = Vec::new();
        let mut has_criteria = false;
        if structured && matches!(q.kind, SearchKind::Book | SearchKind::Music) {
            let (t, fields) = if q.kind == SearchKind::Book {
                (
                    "book",
                    [
                        (SearchParam::Author, "author", &q.author),
                        (SearchParam::Title, "title", &q.title),
                    ],
                )
            } else {
                (
                    "music",
                    [
                        (SearchParam::Artist, "artist", &q.artist),
                        (SearchParam::Album, "album", &q.album),
                    ],
                )
            };
            p.push(("t", t.into()));
            // Parts the indexer can't take as parameters go into `q`.
            let mut leftover: Vec<&Option<String>> = Vec::new();
            for (param, key, v) in fields {
                match v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    Some(v) if mode.supports(param) => {
                        p.push((key, v.to_string()));
                        has_criteria = true;
                    }
                    Some(_) => leftover.push(v),
                    None => {}
                }
            }
            if q.kind == SearchKind::Music
                && mode.supports(SearchParam::Year)
                && let Some(y) = q.year
            {
                p.push(("year", y.to_string()));
            }
            let term = if leftover.is_empty() {
                if has_criteria {
                    q.term.clone().unwrap_or_default()
                } else {
                    q.keywords()
                }
            } else {
                q.joined(&leftover)
            };
            if !term.trim().is_empty() && (mode.supports(SearchParam::Q) || !has_criteria) {
                p.push(("q", term.trim().to_string()));
                has_criteria = true;
            }
        } else if structured {
            p.push((
                "t",
                if q.kind == SearchKind::Movie {
                    "movie"
                } else {
                    "tvsearch"
                }
                .into(),
            ));
            let mut add = |param: SearchParam, key: &'static str, v: Option<String>| {
                if let Some(v) = v.filter(|_| mode.supports(param)) {
                    p.push((key, v));
                    true
                } else {
                    false
                }
            };
            has_criteria |= add(SearchParam::ImdbId, "imdbid", q.imdb_digits());
            has_criteria |= add(SearchParam::TmdbId, "tmdbid", q.tmdb_id.map(|v| v.to_string()));
            has_criteria |= add(SearchParam::TvdbId, "tvdbid", q.tvdb_id.map(|v| v.to_string()));
            let structured_se = mode.supports(SearchParam::Season);
            if q.kind == SearchKind::Tv && structured_se {
                add(SearchParam::Season, "season", q.season.map(|v| v.to_string()));
                add(SearchParam::Ep, "ep", q.episode.clone());
            }
            if q.kind == SearchKind::Movie {
                add(SearchParam::Year, "year", q.year.map(|v| v.to_string()));
            }
            let term = if q.kind == SearchKind::Tv && !structured_se {
                q.keywords()
            } else {
                q.term.clone().unwrap_or_default()
            };
            if !term.trim().is_empty() && (mode.supports(SearchParam::Q) || !has_criteria) {
                p.push(("q", term.trim().to_string()));
                has_criteria = true;
            }
        } else {
            p.push(("t", "search".into()));
            let mut term = q.keywords();
            if q.kind == SearchKind::Movie
                && let Some(y) = q.year
                && !term.is_empty()
            {
                term = format!("{term} {y}");
            }
            if !term.is_empty() {
                p.push(("q", term));
                has_criteria = true;
            }
        }
        if !has_criteria && q.kind != SearchKind::Basic {
            return None;
        }
        let cats = self.categories_for(q);
        if !cats.is_empty() {
            p.push((
                "cat",
                cats.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
            ));
        }
        p.push(("extended", "1".into()));
        let limit = q
            .limit
            .or(caps.limit_default)
            .map(|l| caps.limit_max.map_or(l, |m| l.min(m)));
        if let Some(l) = limit {
            p.push(("limit", l.to_string()));
        }
        if let Some(o) = q.offset {
            p.push(("offset", o.to_string()));
        }
        Some(p)
    }

    async fn fetch(&self, params: Vec<(&'static str, String)>) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let root = self.get_xml(self.url(&params)).await?;
        parse_rss(&root, self.flavor, self.indexer_id, &self.name)
    }
}

/// Maps a Newznab `<error code=".." description=".."/>` root to a typed error.
fn check_error(root: &Node) -> Result<(), IndexerError> {
    if root.local() != "error" {
        return Ok(());
    }
    let code: u32 = root.attr("code").and_then(|c| c.parse().ok()).unwrap_or(900);
    let description = root.attr("description").unwrap_or("unknown error").to_string();
    Err(match code {
        100..=102 => IndexerError::Auth(description),
        500 | 501 => IndexerError::RequestLimit { retry_after: None },
        _ => IndexerError::Api { code, description },
    })
}

/// Parses a `t=caps` document.
pub(crate) fn parse_caps(root: &Node) -> Result<IndexerCapabilities, IndexerError> {
    check_error(root)?;
    if root.local() != "caps" {
        return Err(IndexerError::Parse(format!(
            "expected <caps>, got <{}>",
            root.name
        )));
    }
    let mut caps = IndexerCapabilities::default();
    if let Some(l) = root.child("limits") {
        caps.limit_max = l.attr("max").and_then(|v| v.parse().ok());
        caps.limit_default = l.attr("default").and_then(|v| v.parse().ok());
    }
    if let Some(s) = root.child("searching") {
        let mode = |n: &str| {
            s.child(n).map_or_else(SearchMode::default, |m| SearchMode {
                available: m.attr("available").is_some_and(|a| a.eq_ignore_ascii_case("yes")),
                params: m
                    .attr("supportedParams")
                    .map(SearchParam::parse_list)
                    .unwrap_or_else(|| vec![SearchParam::Q]),
            })
        };
        caps.search = mode("search");
        caps.tv_search = mode("tv-search");
        caps.movie_search = mode("movie-search");
        caps.book_search = mode("book-search");
        caps.audio_search = mode("audio-search");
        if !caps.audio_search.available {
            // Some indexers (and Jackett/Prowlarr) call it `music-search`.
            let m = mode("music-search");
            if m.available {
                caps.audio_search = m;
            }
        }
    }
    if let Some(c) = root.child("categories") {
        for cat in c.children_named("category") {
            let Some(id) = cat.attr("id").and_then(|v| v.parse().ok()) else {
                continue;
            };
            let mut node = Category::new(id, cat.attr("name").unwrap_or_default());
            for sub in cat.children_named("subcat") {
                if let Some(sid) = sub.attr("id").and_then(|v| v.parse().ok()) {
                    node.subcategories
                        .push(Category::new(sid, sub.attr("name").unwrap_or_default()));
                }
            }
            caps.categories.push(node);
        }
    }
    Ok(caps)
}

/// Parses an RSS result document into releases.
pub(crate) fn parse_rss(
    root: &Node,
    flavor: Flavor,
    indexer_id: i64,
    indexer: &str,
) -> Result<Vec<ReleaseInfo>, IndexerError> {
    check_error(root)?;
    let channel = root
        .child("channel")
        .ok_or_else(|| IndexerError::Parse("missing <channel>".into()))?;
    let now = Utc::now();
    let mut out = Vec::new();
    for item in channel.children_named("item") {
        let mut r = ReleaseInfo::new(flavor.protocol(), indexer_id, indexer);
        r.title = item.child_text("title").unwrap_or_default().to_string();
        if r.title.is_empty() {
            continue;
        }
        r.guid = item.child_text("guid").unwrap_or_default().to_string();
        r.info_url = item
            .child_text("comments")
            .map(|c| c.trim_end_matches("#comments").to_string());
        r.publish_date = item.child_text("pubDate").and_then(|d| parse_date(d, now));
        r.size = item.child_text("size").and_then(|s| s.parse().ok());
        let enclosure = item.child("enclosure");
        let link = enclosure
            .and_then(|e| e.attr("url"))
            .or_else(|| item.child_text("link"))
            .map(str::to_string);
        if r.size.is_none() {
            r.size = enclosure
                .and_then(|e| e.attr("length"))
                .and_then(|l| l.parse().ok())
                .filter(|&l: &u64| l > 0);
        }
        for c in item.children_named("category") {
            if let Ok(id) = c.text.trim().parse() {
                r.categories.push(id);
            } else if let Some(id) = categories::by_name(c.text.trim()) {
                r.categories.push(id);
            }
        }
        let mut leechers = None;
        let mut attr_cats = Vec::new();
        for a in item.children_named("attr") {
            let (Some(name), Some(v)) = (a.attr("name"), a.attr("value")) else {
                continue;
            };
            let num = || v.trim().parse::<u32>().ok();
            match name.to_ascii_lowercase().as_str() {
                "size" => r.size = v.parse().ok().or(r.size),
                "category" => attr_cats.extend(num()),
                "seeders" => r.seeders = num(),
                "peers" => r.peers = num(),
                "leechers" => leechers = num(),
                "grabs" => r.grabs = num(),
                "files" => r.files = num(),
                "infohash" => r.info_hash = Some(v.to_string()),
                "magneturl" => r.magnet_url = Some(v.to_string()),
                "imdb" | "imdbid" => r.imdb_id = Some(v.to_string()),
                "tmdbid" | "tmdb" => r.tmdb_id = num().filter(|&n| n > 0),
                "tvdbid" => r.tvdb_id = num().filter(|&n| n > 0),
                "downloadvolumefactor" => r.download_volume_factor = v.parse().unwrap_or(1.0),
                "uploadvolumefactor" => r.upload_volume_factor = v.parse().unwrap_or(1.0),
                "minimumratio" => r.minimum_ratio = v.parse().ok(),
                "minimumseedtime" => r.minimum_seed_time = v.parse().ok(),
                "usenetdate" => r.publish_date = parse_date(v, now).or(r.publish_date),
                "tag" => r
                    .indexer_flags
                    .extend(v.split(',').filter_map(IndexerFlag::parse)),
                _ => {}
            }
        }
        if !attr_cats.is_empty() {
            r.categories = attr_cats;
        }
        r.categories.dedup();
        if r.peers.is_none() {
            r.peers = match (r.seeders, leechers) {
                (Some(s), Some(l)) => Some(s + l),
                (None, l) => l,
                (s, None) => s,
            };
        }
        match link {
            Some(l) if l.starts_with("magnet:") => r.magnet_url = r.magnet_url.or(Some(l)),
            l => r.download_url = l,
        }
        if flavor == Flavor::Torznab && r.info_hash.is_none() {
            r.info_hash = r.magnet_url.as_deref().and_then(magnet_hash);
        }
        r.finish();
        out.push(r);
    }
    Ok(out)
}

/// Extracts the hex `btih` hash from a magnet link.
pub(crate) fn magnet_hash(m: &str) -> Option<String> {
    let i = m.find("urn:btih:")? + 9;
    let h: String = m[i..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (h.len() == 40).then(|| h.to_ascii_lowercase())
}

#[async_trait]
impl Indexer for NewznabIndexer {
    fn protocol(&self) -> Protocol {
        self.flavor.protocol()
    }

    async fn capabilities(&self) -> Result<IndexerCapabilities, IndexerError> {
        self.caps
            .get_or_try_init(|| async {
                let root = self.get_xml(self.url(&[("t", "caps".into())])).await?;
                parse_caps(&root)
            })
            .await
            .cloned()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let caps = self.capabilities().await?;
        match self.params(query, &caps) {
            Some(p) => {
                let mut out = self.fetch(p).await?;
                categories::filter_excluded(query, &self.categories_for(query), &mut out);
                Ok(out)
            }
            None => Ok(Vec::new()),
        }
    }

    async fn rss(&self, categories: &[u32]) -> Result<Vec<ReleaseInfo>, IndexerError> {
        self.search(&SearchQuery {
            categories: categories.to_vec(),
            ..SearchQuery::default()
        })
        .await
    }

    async fn test(&self) -> Result<(), IndexerError> {
        self.capabilities().await?;
        self.rss(&[]).await.map(|_| ())
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, IndexerError> {
        let url = self
            .api
            .join(url)
            .map_err(|e| IndexerError::Config(format!("download url: {e}")))?;
        self.guard.check(&url).await?;
        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .unwrap_or_default();
            if loc.starts_with("magnet:") {
                return Err(IndexerError::MagnetRedirect(loc.to_string()));
            }
            return Err(IndexerError::Blocked(format!("redirect to {loc}")));
        }
        if let Some(e) = status_error(&resp) {
            return Err(e);
        }
        let bytes = resp.bytes().await?.to_vec();
        // Newznab indexers report quota/auth problems on the download endpoint as XML.
        if bytes.len() < 4096
            && let Ok(text) = std::str::from_utf8(&bytes)
            && let Ok(root) = xml::parse(text)
        {
            check_error(&root)?;
        }
        if bytes.is_empty() {
            return Err(IndexerError::Parse("empty download".into()));
        }
        Ok(bytes)
    }
}
