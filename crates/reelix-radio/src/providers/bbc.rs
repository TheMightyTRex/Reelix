//! BBC programmes: brands, series and episodes from the BBC's programme pages.
//!
//! **Undocumented endpoints.** The BBC publishes no API for this. The provider reads the
//! public JSON behind its programme pages (`/programmes/{pid}.json`,
//! `/programmes/{pid}/children.json`, the A to Z `/programmes/a-z/by/{letter}/all.json`) and
//! BBC Sounds search (`rms.api.bbc.co.uk/v2/programmes/search/container`). Either can change
//! without notice: every read is checked, and a changed or missing answer fails with a plain
//! message; saved series and episodes are kept (a failed read changes nothing, and nothing is
//! ever deleted). Responses are cached for an hour (searches for five minutes), requests are
//! spaced 1.5 s apart and `Retry-After` is honoured.
//!
//! A show is a brand, or a series with no brand above it. Any pid inside a show (an episode,
//! a series) resolves to it by walking up to six parent levels. The brand's children are its
//! series and, sometimes, episodes listed straight under the brand: those become the
//! specials. A series inside a series is folded into its parent (and numbered by broadcast
//! order, since positions repeat across the folded series). Episode lengths need one request
//! each (the episode's `versions`), so at most [`ShowFetch::max_lookups`] are read per
//! refresh, newest first; settled series are not read again.
//!
//! Search: BBC Sounds leaves out many older shows (Cabin Pressure, Old Harry's Game), so when
//! none of its results has the name asked for, the BBC programmes A to Z is read too and its
//! radio brands and series whose title starts with the name come first. The A to Z lists
//! every title for one letter in pages of [`ATOZ_PAGE`], in order, so a binary search finds
//! the page in about ten requests (cached like every programme read).
//!
//! Discover lists (cached 30 minutes, only offered for `GB` or no country, like search):
//! `popular`, `station-{service id}` (Radio 4, Radio 4 Extra, …) and `category-{id}`
//! (Comedy, Drama and their sub-genres, …) from BBC Sounds' most played episodes
//! (`rms.api.bbc.co.uk/v2/programmes/playable?sort=popular&network=|category=`, also
//! undocumented), reduced to the programmes they belong to.
//!
//! The endpoints, the hierarchy walk, the link patterns, the A to Z search, the stations and
//! the categories are adapted from Storyarr's BBC adapter and radio Discover
//! (`services/app/adapters/bbc_programmes.py`, `services/app/domain/radio.py`,
//! `services/app/api/radio_discover.py` and `services/app/api/preferences.py` in
//! <https://github.com/TheMightyTRex/storyarr>), Copyright (c) 2026 Dewarr contributors,
//! used under the MIT License
//! (<https://github.com/TheMightyTRex/storyarr/blob/main/LICENSE>).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reelix_api_types::radio::RadioSeriesKind;
use reqwest::header::HeaderMap;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::provider::{
    DISCOVER_LIMIT, DISCOVER_TTL, DiscoverList, EpisodeData, ProviderError, RadioProvider, SearchPrefs,
    SeriesData, ShowData, ShowFetch, ShowHit, StationData, async_trait,
};
use crate::providers::http::{JsonReader, str_at};

pub const ID: &str = "bbc";

pub const PROGRAMMES_BASE: &str = "https://www.bbc.co.uk";
pub const SEARCH_BASE: &str = "https://rms.api.bbc.co.uk";
/// Where the BBC keeps its podcast feeds (RFX-95).
pub const PODCASTS_BASE: &str = "https://podcasts.files.bbci.co.uk";
/// Show artwork (square) by image pid.
pub const ARTWORK_URL: &str = "https://ichef.bbci.co.uk/images/ic/640x640/{pid}.jpg";
/// Station logos (SVG) by service id.
pub const LOGO_URL: &str = "https://sounds.files.bbci.co.uk/3.12.0/networks/{id}/blocks-colour_default.svg";

pub const REQUEST_INTERVAL: Duration = Duration::from_millis(1500);
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
pub const SEARCH_TTL: Duration = Duration::from_secs(300);

const PAGE_SIZE: u32 = 50;
/// Pages of children read per programme (1000 children).
const MAX_PAGES: u32 = 20;
const MAX_PARENT_LEVELS: usize = 6;
/// Nested series folded into their parent at most this deep.
const MAX_FOLD_DEPTH: u32 = 2;

/// BBC Sounds categories offered as Discover lists, by heading: `(heading, [(category id,
/// name)])`. Sub-genres (`drama-scifiandfantasy`) are categories of their own; BBC Sounds has
/// no separate science fiction category.
pub const CATEGORY_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Comedy",
        &[
            ("comedy", "All comedy"),
            ("comedy-sitcoms", "Sitcoms"),
            ("comedy-satire", "Satire"),
            ("comedy-standup", "Stand-up"),
            ("comedy-sketch", "Sketches"),
            ("comedy-chat", "Chat"),
            ("gamesandquizzes", "Panel games and quizzes"),
        ],
    ),
    (
        "Drama",
        &[
            ("drama", "All drama"),
            ("drama-scifiandfantasy", "Science fiction and fantasy"),
            ("drama-crime", "Crime"),
            ("drama-thriller", "Thrillers"),
            ("drama-horrorandsupernatural", "Horror and supernatural"),
            ("drama-historical", "Historical"),
            ("drama-classicandperiod", "Classic and period"),
            ("drama-relationshipsandromance", "Relationships and romance"),
            ("drama-soaps", "Soaps"),
            ("readings", "Readings"),
            ("audiobooks", "Audiobooks"),
        ],
    ),
    (
        "Factual",
        &[
            ("news", "News"),
            ("documentaries", "Documentaries"),
            ("factual-history", "History"),
            ("factual-scienceandnature", "Science and nature"),
            (
                "factual-scienceandnature-scienceandtechnology",
                "Science and technology",
            ),
            ("factual-politics", "Politics"),
            ("factual-crimeandjustice", "Crime and justice"),
            ("factual-crimeandjustice-truecrime", "True crime"),
            ("factual-artscultureandthemedia", "Arts, culture and media"),
            ("factual-healthandwellbeing", "Health and wellbeing"),
            ("factual-foodanddrink", "Food and drink"),
            ("discussionandtalk", "Discussion and talk"),
        ],
    ),
    ("Sport and music", &[("sport", "Sport"), ("music", "Music")]),
];

/// Every category id in [`CATEGORY_GROUPS`].
pub fn categories() -> impl Iterator<Item = &'static str> {
    CATEGORY_GROUPS
        .iter()
        .flat_map(|(_, cats)| cats.iter().map(|(id, _)| *id))
}

/// BBC stations offered as Discover rows of their most played programmes: `(service id,
/// name)`.
pub const STATIONS: &[(&str, &str)] = &[
    ("bbc_radio_four", "Radio 4"),
    ("bbc_radio_four_extra", "Radio 4 Extra"),
    ("bbc_radio_two", "Radio 2"),
    ("bbc_radio_five_live", "Radio 5 Live"),
    ("bbc_world_service", "World Service"),
];

/// Every BBC radio network BBC Sounds lists (`rms.api.bbc.co.uk/v2/networks`, read 12 November 2026), as
/// `(network id, name, group)`: national, the nations and local. Each is a Discover list of its most played
/// programmes (`network-{id}`) and a station Starboard can show with its logo (RFX-94).
pub const NETWORKS: &[(&str, &str, &str)] = &[
    ("bbc_radio_one", "BBC Radio 1", "National"),
    ("bbc_radio_one_anthems", "BBC Radio 1 Anthems", "National"),
    ("bbc_radio_one_dance", "BBC Radio 1 Dance", "National"),
    ("bbc_1xtra", "BBC Radio 1Xtra", "National"),
    ("bbc_radio_two", "BBC Radio 2", "National"),
    ("bbc_radio_three", "BBC Radio 3", "National"),
    ("bbc_radio_three_unwind", "BBC Radio 3 Unwind", "National"),
    ("bbc_radio_four", "BBC Radio 4", "National"),
    ("bbc_radio_four_extra", "BBC Radio 4 Extra", "National"),
    ("bbc_radio_five_live", "BBC Radio 5 Live", "National"),
    (
        "bbc_radio_five_live_sports_extra",
        "BBC Radio 5 Sports Extra",
        "National",
    ),
    (
        "bbc_radio_five_sports_extra_2",
        "BBC Radio 5 Sports Extra 2",
        "National",
    ),
    (
        "bbc_radio_five_sports_extra_3",
        "BBC Radio 5 Sports Extra 3",
        "National",
    ),
    ("bbc_6music", "BBC Radio 6 Music", "National"),
    (
        "bbc_radio_six_indie_forever",
        "BBC Radio 6 Indie Forever",
        "National",
    ),
    ("bbc_asian_network", "BBC Asian Network", "National"),
    ("bbc_world_service", "BBC World Service", "National"),
    ("bbc_sounds_news", "Live News", "National"),
    ("cbeebies_radio", "CBeebies Radio", "National"),
    ("bbc_radio_scotland", "BBC Radio Scotland", "Nations"),
    ("bbc_radio_nan_gaidheal", "BBC Radio nan Gàidheal", "Nations"),
    ("bbc_radio_ulster", "BBC Radio Ulster", "Nations"),
    ("bbc_radio_foyle", "BBC Radio Foyle", "Nations"),
    ("bbc_radio_wales", "BBC Radio Wales", "Nations"),
    ("bbc_radio_cymru", "BBC Radio Cymru", "Nations"),
    ("bbc_radio_cymru_2", "BBC Radio Cymru 2", "Nations"),
    ("bbc_radio_berkshire", "BBC Radio Berkshire", "Local"),
    ("bbc_radio_bristol", "BBC Radio Bristol", "Local"),
    ("bbc_radio_cambridge", "BBC Radio Cambridgeshire", "Local"),
    ("bbc_radio_cornwall", "BBC Radio Cornwall", "Local"),
    ("bbc_radio_coventry_warwickshire", "BBC CWR", "Local"),
    ("bbc_radio_cumbria", "BBC Radio Cumbria", "Local"),
    ("bbc_radio_derby", "BBC Radio Derby", "Local"),
    ("bbc_radio_devon", "BBC Radio Devon", "Local"),
    ("bbc_radio_essex", "BBC Essex", "Local"),
    ("bbc_radio_gloucestershire", "BBC Radio Gloucestershire", "Local"),
    ("bbc_radio_guernsey", "BBC Radio Guernsey", "Local"),
    (
        "bbc_radio_hereford_worcester",
        "BBC Hereford & Worcester",
        "Local",
    ),
    ("bbc_radio_humberside", "BBC Radio Humberside", "Local"),
    ("bbc_radio_jersey", "BBC Radio Jersey", "Local"),
    ("bbc_radio_kent", "BBC Radio Kent", "Local"),
    ("bbc_radio_lancashire", "BBC Radio Lancashire", "Local"),
    ("bbc_radio_leeds", "BBC Radio Leeds", "Local"),
    ("bbc_radio_leicester", "BBC Radio Leicester", "Local"),
    ("bbc_radio_lincolnshire", "BBC Radio Lincolnshire", "Local"),
    ("bbc_london", "BBC Radio London", "Local"),
    ("bbc_radio_manchester", "BBC Radio Manchester", "Local"),
    ("bbc_radio_merseyside", "BBC Radio Merseyside", "Local"),
    ("bbc_radio_newcastle", "BBC Radio Newcastle", "Local"),
    ("bbc_radio_norfolk", "BBC Radio Norfolk", "Local"),
    ("bbc_radio_northampton", "BBC Radio Northampton", "Local"),
    ("bbc_radio_nottingham", "BBC Radio Nottingham", "Local"),
    ("bbc_radio_oxford", "BBC Radio Oxford", "Local"),
    ("bbc_radio_sheffield", "BBC Radio Sheffield", "Local"),
    ("bbc_radio_shropshire", "BBC Radio Shropshire", "Local"),
    ("bbc_radio_solent", "BBC Radio Solent", "Local"),
    ("bbc_radio_somerset_sound", "BBC Radio Somerset", "Local"),
    ("bbc_radio_stoke", "BBC Radio Stoke", "Local"),
    ("bbc_radio_suffolk", "BBC Radio Suffolk", "Local"),
    ("bbc_radio_surrey", "BBC Radio Surrey", "Local"),
    ("bbc_radio_sussex", "BBC Radio Sussex", "Local"),
    ("bbc_tees", "BBC Radio Tees", "Local"),
    ("bbc_three_counties_radio", "BBC Three Counties Radio", "Local"),
    ("bbc_radio_wiltshire", "BBC Radio Wiltshire", "Local"),
    ("bbc_wm", "BBC Radio WM", "Local"),
    ("bbc_radio_york", "BBC Radio York", "Local"),
];

/// Titles per page of the BBC programmes A to Z.
pub const ATOZ_PAGE: u64 = 30;
/// Most A to Z shows put before the BBC Sounds results.
const ATOZ_LIMIT: usize = 10;

/// Most played episodes read per list (they are reduced to their programmes).
const POPULAR_EPISODES: u32 = 100;

const CHANGED: &str =
    "the BBC programme data has changed, so Reelix cannot read it; saved series and episodes are kept";

pub struct BbcProvider {
    programmes: JsonReader,
    search: JsonReader,
    base: String,
    search_base: String,
    /// The BBC's podcast feeds (RFX-95): a programme with a podcast has one at `{podcasts_base}/{pid}.rss`.
    feeds: crate::providers::RssProvider,
    podcasts_base: String,
}

impl BbcProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            programmes: JsonReader::new(http.clone(), "the BBC", REQUEST_INTERVAL, CACHE_TTL),
            search: JsonReader::new(http.clone(), "BBC Sounds search", REQUEST_INTERVAL, SEARCH_TTL),
            base: PROGRAMMES_BASE.to_owned(),
            search_base: SEARCH_BASE.to_owned(),
            feeds: crate::providers::RssProvider::new(http),
            podcasts_base: PODCASTS_BASE.to_owned(),
        }
    }

    /// Another host for the podcast feeds (tests).
    pub fn with_podcasts_url(mut self, base: &str) -> Self {
        self.podcasts_base = base.trim_end_matches('/').to_owned();
        self
    }

    /// RFX-95: when the programme has a BBC podcast, its feed is the show's feed and each episode it holds
    /// takes its download link from it (the feed names each episode `urn:bbc:podcast:{pid}`). A programme with
    /// no podcast (404) is left as it was; a feed that cannot be read is left out without failing the show.
    async fn with_podcast(&self, mut show: ShowData) -> ShowData {
        let url = format!("{}/{}.rss", self.podcasts_base, show.external_id);
        let feed = match self.feeds.read(&url).await {
            Ok(Some(feed)) => feed,
            Ok(None) | Err(ProviderError::NotFound(_)) => return show,
            Err(e) => {
                tracing::debug!(pid = %show.external_id, error = %e, "BBC podcast feed not read");
                return show;
            }
        };
        let links: HashMap<String, String> = feed
            .episodes
            .iter()
            .filter_map(|e| {
                let pid = e.guid.as_deref()?.strip_prefix("urn:bbc:podcast:")?;
                Some((pid.to_owned(), e.enclosure_url.clone()?))
            })
            .collect();
        for e in &mut show.episodes {
            if e.enclosure_url.is_none()
                && let Some(link) = links.get(&e.external_id)
            {
                e.enclosure_url = Some(link.clone());
            }
        }
        show.feed_url = Some(url);
        show
    }

    /// Other hosts for the programme pages and the search (tests).
    pub fn with_base_urls(mut self, programmes: &str, search: &str) -> Self {
        self.base = programmes.trim_end_matches('/').to_owned();
        self.search_base = search.trim_end_matches('/').to_owned();
        self
    }

    /// Spacing between requests (tests use zero).
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.programmes = self.programmes.with_interval(interval);
        self.search = self.search.with_interval(interval);
        self
    }

    /// The `programme` object of `{pid}.json`, checked.
    async fn programme(&self, pid: &str) -> Result<Value, ProviderError> {
        let url = format!("{}/programmes/{pid}.json", self.base);
        let data = self.programmes.get(&url, HeaderMap::new()).await?;
        let item = data.get("programme").cloned().unwrap_or(Value::Null);
        let kind = str_at(&item, "type");
        if str_at(&item, "pid") != Some(pid) || !matches!(kind, Some("brand" | "series" | "episode" | "clip"))
        {
            return Err(ProviderError::Parse(CHANGED.into()));
        }
        Ok(item)
    }

    /// Every child of `pid` (pages of [`PAGE_SIZE`]).
    async fn children(&self, pid: &str) -> Result<Vec<Value>, ProviderError> {
        let mut found = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut url = format!("{}/programmes/{pid}/children.json?limit={PAGE_SIZE}", self.base);
            if page > 1 {
                url.push_str(&format!("&page={page}"));
            }
            let data = self.programmes.get(&url, HeaderMap::new()).await?;
            let listing = data.get("children");
            let items = listing
                .and_then(|l| l.get("programmes"))
                .and_then(Value::as_array);
            let total = listing.and_then(|l| l.get("total")).and_then(Value::as_u64);
            let (Some(items), Some(total)) = (items, total) else {
                return Err(ProviderError::Parse(CHANGED.into()));
            };
            found.extend(
                items
                    .iter()
                    .filter(|i| str_at(i, "pid").is_some_and(is_pid))
                    .cloned(),
            );
            if items.is_empty() || found.len() as u64 >= total {
                break;
            }
        }
        Ok(found)
    }

    /// The show's top programme for any pid in it (an episode resolves to its brand).
    async fn resolve(&self, pid: &str) -> Result<Value, ProviderError> {
        let item = self.programme(pid).await?;
        if str_at(&item, "type") == Some("brand") {
            return Ok(item);
        }
        match top_pid(&item) {
            Some(top) if top != pid => self.programme(&top).await,
            _ if str_at(&item, "type") == Some("series") => Ok(item),
            _ => Err(ProviderError::NotFound(
                "this is a single BBC programme, not part of a show; paste a link to the show instead".into(),
            )),
        }
    }

    /// Episodes of a series; a series inside it is folded in (positions dropped).
    async fn series_episodes(&self, pid: &str) -> Result<Vec<Value>, ProviderError> {
        let mut found = Vec::new();
        let mut nested = false;
        let mut todo = vec![(pid.to_owned(), 0u32)];
        while let Some((pid, depth)) = todo.pop() {
            for item in self.children(&pid).await? {
                match str_at(&item, "type") {
                    Some("episode") => found.push(item),
                    Some("series") if depth < MAX_FOLD_DEPTH => {
                        nested = true;
                        if let Some(p) = str_at(&item, "pid") {
                            todo.push((p.to_owned(), depth + 1));
                        }
                    }
                    _ => {}
                }
            }
        }
        if nested {
            for item in &mut found {
                if let Some(o) = item.as_object_mut() {
                    o.remove("position");
                }
            }
        }
        Ok(found)
    }

    /// One page of the A to Z for `letter`: the total titles and the page's titles.
    async fn atoz_page(&self, letter: char, page: u64) -> Result<(u64, Vec<Value>), ProviderError> {
        let url = format!("{}/programmes/a-z/by/{letter}/all.json?page={page}", self.base);
        let data = self.programmes.get(&url, HeaderMap::new()).await?;
        atoz_titles(&data)
    }

    /// Radio brands and series whose title starts with `title` in the BBC programmes A to Z
    /// (at most `limit`). Its pages hold [`ATOZ_PAGE`] titles in order, so a binary search on
    /// each page's first title finds the right page.
    async fn atoz(&self, title: &str, limit: usize) -> Result<Vec<ShowHit>, ProviderError> {
        let key = atoz_key(title);
        let Some(first) = key.chars().next().filter(char::is_ascii_alphanumeric) else {
            return Ok(Vec::new());
        };
        let letter = if first.is_ascii_alphabetic() { first } else { '@' };
        let (total, titles) = self.atoz_page(letter, 1).await?;
        let mut pages = HashMap::from([(1, titles)]);
        let last = total.div_ceil(ATOZ_PAGE).max(1);
        let (mut low, mut high) = (1, last);
        while low < high {
            let middle = (low + high).div_ceil(2);
            if let Entry::Vacant(e) = pages.entry(middle) {
                e.insert(self.atoz_page(letter, middle).await?.1);
            }
            let head = pages[&middle].first().and_then(|t| str_at(t, "title"));
            if head.is_some_and(|h| atoz_key(h) <= key) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let mut hits = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for page in low.saturating_sub(1).max(1)..=(low + 1).min(last) {
            if let Entry::Vacant(e) = pages.entry(page) {
                e.insert(self.atoz_page(letter, page).await?.1);
            }
            hits.extend(
                pages[&page]
                    .iter()
                    .filter(|t| str_at(t, "title").is_some_and(|n| atoz_key(n).starts_with(&key)))
                    .filter_map(|t| atoz_hit(t.get("programme")?))
                    .filter(|h| seen.insert(h.external_id.clone())),
            );
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// Reads the show whose top programme is `top`.
    async fn read(&self, top: &Value, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let mut show = show_base(top)?;
        let top_pid = show.external_id.clone();
        // (series external id, pid to read) newest first.
        let mut to_read: Vec<(String, String, Option<DateTime<Utc>>)> = Vec::new();
        if str_at(top, "type") == Some("series") {
            show.series.push(series_data(top));
            to_read.push((
                top_pid.clone(),
                top_pid.clone(),
                when(top, "first_broadcast_date"),
            ));
        } else {
            let kids = self.children(&top_pid).await?;
            let mut loose = Vec::new();
            for k in &kids {
                match str_at(k, "type") {
                    Some("series") => {
                        let s = series_data(k);
                        to_read.push((
                            s.external_id.clone(),
                            s.external_id.clone(),
                            when(k, "first_broadcast_date"),
                        ));
                        show.series.push(s);
                    }
                    Some("episode") => loose.push(k),
                    _ => {}
                }
            }
            if !loose.is_empty() {
                show.series.push(SeriesData {
                    external_id: SPECIALS.into(),
                    kind: RadioSeriesKind::Specials,
                    title: Some("Specials".into()),
                    ..SeriesData::default()
                });
                show.episodes
                    .extend(loose.into_iter().filter_map(|e| episode_data(e, SPECIALS)));
            }
        }
        // Settled series are left out entirely (not a removal).
        show.series
            .retain(|s| !fetch.skip_series.contains(&s.external_id));
        to_read.retain(|(sid, _, _)| !fetch.skip_series.contains(sid));
        to_read.sort_by_key(|(_, _, first)| std::cmp::Reverse(*first));
        if fetch.quick {
            // A first look: the newest series only; the service reads the rest.
            to_read.truncate(1);
            show.complete = false;
        }
        for (sid, pid, _) in &to_read {
            match self.series_episodes(pid).await {
                Ok(items) => show
                    .episodes
                    .extend(items.iter().filter_map(|e| episode_data(e, sid))),
                Err(ProviderError::RateLimited { .. }) => {
                    // Keep what was read; nothing is marked removed on a partial read.
                    tracing::info!(show = %show.title, "BBC asked Reelix to slow down; the rest is read next time");
                    show.complete = false;
                    break;
                }
                Err(ProviderError::NotFound(_)) => show.complete = false,
                Err(e) => return Err(e),
            }
        }
        if !fetch.quick {
            self.lengths(&mut show, fetch).await;
        }
        Ok(show)
    }

    /// Reads up to `max_lookups` episode lengths, newest broadcast first, for episodes whose
    /// length is not saved yet. Lengths are a nicety: a missing page is skipped, a rate limit
    /// stops the lookups.
    async fn lengths(&self, show: &mut ShowData, fetch: &ShowFetch) {
        let now = Utc::now();
        let mut due: Vec<(DateTime<Utc>, usize)> = show
            .episodes
            .iter()
            .enumerate()
            .filter(|(_, e)| !fetch.known_episodes.contains(&e.external_id))
            .filter_map(|(i, e)| e.first_broadcast.filter(|d| *d <= now).map(|d| (d, i)))
            .collect();
        due.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
        for (_, i) in due.into_iter().take(fetch.max_lookups as usize) {
            let pid = show.episodes[i].external_id.clone();
            match self.programme(&pid).await {
                Ok(item) => show.episodes[i].duration_secs = duration(&item),
                Err(ProviderError::RateLimited { .. }) => break,
                Err(e) => tracing::debug!(pid, error = %e, "BBC episode length not read"),
            }
        }
    }
}

/// External id of the series that holds episodes listed straight under a brand.
const SPECIALS: &str = "specials";

/// BBC pids: digits and consonants, 8 to 15 characters (`b006qpgr`, `m002vm5p`).
pub fn is_pid(s: &str) -> bool {
    (8..=15).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && !b"aeiou".contains(&b)))
}

fn is_service_id(s: &str) -> bool {
    (2..=60).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The pid in a BBC programmes or BBC Sounds link (or a bare pid); `None` for other links.
pub fn parse_link(link: &str) -> Option<String> {
    let link = link.trim();
    if is_pid(link) {
        return Some(link.to_owned());
    }
    let u = url::Url::parse(link).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if !matches!(u.scheme(), "http" | "https")
        || !matches!(
            host.as_str(),
            "bbc.co.uk" | "www.bbc.co.uk" | "bbc.com" | "www.bbc.com"
        )
    {
        return None;
    }
    let seg: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let pid = match seg.as_slice() {
        ["programmes", pid, ..] => pid.trim_end_matches(".json"),
        ["sounds", "brand" | "series" | "play" | "episode", pid, ..] => pid,
        _ => return None,
    };
    is_pid(pid).then(|| pid.to_owned())
}

/// The brand above an episode or series, or its topmost series when there is no brand.
fn top_pid(item: &Value) -> Option<String> {
    let mut top = None;
    let mut node = item;
    for _ in 0..MAX_PARENT_LEVELS {
        let Some(parent) = node.get("parent").and_then(|p| p.get("programme")) else {
            break;
        };
        if matches!(str_at(parent, "type"), Some("brand" | "series"))
            && let Some(pid) = str_at(parent, "pid").filter(|p| is_pid(p))
        {
            top = Some(pid.to_owned());
        }
        node = parent;
    }
    top
}

fn when(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = str_at(v, key)?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn synopsis(v: &Value) -> Option<String> {
    ["long_synopsis", "medium_synopsis", "short_synopsis"]
        .iter()
        .find_map(|k| str_at(v, k))
        .map(str::to_owned)
}

fn position(v: &Value) -> Option<u32> {
    v.get("position")?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn image_pid(v: &Value) -> Option<&str> {
    v.get("image")
        .and_then(|i| str_at(i, "pid"))
        .filter(|p| is_pid(p))
}

fn station(v: &Value) -> Option<StationData> {
    let service = v.get("ownership")?.get("service")?;
    let id = str_at(service, "id").filter(|i| is_service_id(i))?;
    let name = str_at(service, "title")?;
    Some(StationData {
        external_id: id.to_owned(),
        name: name.to_owned(),
        logo_url: Some(LOGO_URL.replace("{id}", id)),
    })
}

fn show_base(top: &Value) -> Result<ShowData, ProviderError> {
    let pid = str_at(top, "pid").ok_or_else(|| ProviderError::Parse(CHANGED.into()))?;
    let title = str_at(top, "title").ok_or_else(|| ProviderError::Parse(CHANGED.into()))?;
    Ok(ShowData {
        external_id: pid.to_owned(),
        title: title.to_owned(),
        synopsis: synopsis(top),
        author: None,
        web_url: Some(format!("https://www.bbc.co.uk/programmes/{pid}")),
        feed_url: None,
        artwork_url: image_pid(top).map(|p| ARTWORK_URL.replace("{pid}", p)),
        first_broadcast: when(top, "first_broadcast_date"),
        station: station(top),
        complete: true,
        ..ShowData::default()
    })
}

fn series_data(v: &Value) -> SeriesData {
    SeriesData {
        external_id: str_at(v, "pid").unwrap_or_default().to_owned(),
        kind: RadioSeriesKind::Series,
        number: position(v),
        title: str_at(v, "title").map(str::to_owned),
        synopsis: synopsis(v),
        expected_episodes: v
            .get("expected_child_count")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
    }
}

fn episode_data(v: &Value, series: &str) -> Option<EpisodeData> {
    let pid = str_at(v, "pid").filter(|p| is_pid(p))?;
    Some(EpisodeData {
        external_id: pid.to_owned(),
        series_external_id: series.to_owned(),
        number: position(v),
        title: str_at(v, "title").unwrap_or("Untitled episode").to_owned(),
        synopsis: synopsis(v),
        first_broadcast: when(v, "first_broadcast_date"),
        duration_secs: None,
        enclosure_url: None,
        guid: None,
    })
}

/// The episode's length from its `versions` (the canonical version first).
fn duration(item: &Value) -> Option<u32> {
    let mut versions: Vec<&Value> = item.get("versions")?.as_array()?.iter().collect();
    versions.sort_by_key(|v| v.get("canonical").and_then(Value::as_u64) != Some(1));
    versions
        .iter()
        .filter_map(|v| v.get("duration").and_then(Value::as_u64))
        .find(|s| (1..24 * 3600).contains(s))
        .and_then(|s| u32::try_from(s).ok())
}

/// Programmes of a BBC Sounds list of playable episodes, in order, once each.
pub fn popular_hits(data: &Value) -> Result<Vec<ShowHit>, ProviderError> {
    let items = data
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Parse("the BBC Sounds lists have changed".into()))?;
    let mut seen = std::collections::HashSet::new();
    Ok(items
        .iter()
        .filter_map(|i| {
            let c = i.get("container")?;
            if !matches!(str_at(c, "type"), Some("brand" | "series")) {
                return None;
            }
            let pid = str_at(c, "id").filter(|p| is_pid(p))?;
            if !seen.insert(pid.to_owned()) {
                return None;
            }
            let synopses = c.get("synopses");
            Some(ShowHit {
                provider: ID.to_owned(),
                external_id: pid.to_owned(),
                title: str_at(c, "title")?.to_owned(),
                synopsis: synopses
                    .and_then(|s| str_at(s, "short").or_else(|| str_at(s, "medium")))
                    .map(str::to_owned),
                author: None,
                station_name: i
                    .get("network")
                    .and_then(|n| str_at(n, "short_title"))
                    .map(str::to_owned),
                artwork_url: str_at(i, "image_url")
                    .filter(|u| u.starts_with("https://ichef.bbci.co.uk/"))
                    .map(|u| u.replace("{recipe}", "640x640")),
                web_url: Some(format!("https://www.bbc.co.uk/programmes/{pid}")),
                feed_url: None,
                episode_count: None,
                show_id: None,
                broadcaster: None,
            })
        })
        .take(DISCOVER_LIMIT)
        .collect())
}

/// How the BBC A to Z sorts titles: no case, accents or punctuation, and no leading "The"
/// (the A to Z lists "The Archers" as "Archers, The").
pub fn atoz_key(value: &str) -> String {
    let folded: String = value
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c.is_whitespace())
        .collect();
    let spaced = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    match spaced.strip_prefix("the ") {
        Some(rest) => rest.to_owned(),
        None => spaced,
    }
}

/// The total and the titles of one A to Z page.
pub fn atoz_titles(data: &Value) -> Result<(u64, Vec<Value>), ProviderError> {
    let atoz = data.get("atoz");
    let titles = atoz.and_then(|a| a.get("tleo_titles")).and_then(Value::as_array);
    let total = atoz.and_then(|a| a.get("total")).and_then(Value::as_u64);
    let (Some(titles), Some(total)) = (titles, total) else {
        return Err(ProviderError::Parse(CHANGED.into()));
    };
    Ok((total, titles.iter().filter(|t| t.is_object()).cloned().collect()))
}

/// A radio brand or series from the A to Z; `None` for episodes, clips and television.
fn atoz_hit(programme: &Value) -> Option<ShowHit> {
    if !matches!(str_at(programme, "type"), Some("brand" | "series")) {
        return None;
    }
    let service = programme.get("ownership").and_then(|o| o.get("service"));
    if service.and_then(|s| str_at(s, "type")) != Some("radio") {
        return None;
    }
    let pid = str_at(programme, "pid").filter(|p| is_pid(p))?;
    Some(ShowHit {
        provider: ID.to_owned(),
        external_id: pid.to_owned(),
        title: str_at(programme, "title")?.to_owned(),
        synopsis: str_at(programme, "short_synopsis").map(str::to_owned),
        author: None,
        station_name: service.and_then(|s| str_at(s, "title")).map(str::to_owned),
        artwork_url: image_pid(programme).map(|p| ARTWORK_URL.replace("{pid}", p)),
        web_url: Some(format!("https://www.bbc.co.uk/programmes/{pid}")),
        feed_url: None,
        episode_count: None,
        show_id: None,
        broadcaster: None,
    })
}

/// Hits of BBC Sounds container search.
fn search_hits(data: &Value) -> Result<Vec<ShowHit>, ProviderError> {
    let items = data.get("data").and_then(Value::as_array).ok_or_else(|| {
        ProviderError::Parse("BBC Sounds search has changed; paste a link to the show instead".into())
    })?;
    Ok(items
        .iter()
        .filter(|i| str_at(i, "type") == Some("container_item"))
        .filter_map(|i| {
            let pid = str_at(i, "id").filter(|p| is_pid(p))?;
            let title = i.get("titles").and_then(|t| str_at(t, "primary"))?;
            let artwork_url = str_at(i, "image_url")
                .filter(|u| u.starts_with("https://ichef.bbci.co.uk/"))
                .map(|u| u.replace("{recipe}", "640x640"));
            Some(ShowHit {
                provider: ID.to_owned(),
                external_id: pid.to_owned(),
                title: title.to_owned(),
                synopsis: i
                    .get("synopses")
                    .and_then(|s| str_at(s, "short"))
                    .map(str::to_owned),
                author: None,
                station_name: i
                    .get("network")
                    .and_then(|n| str_at(n, "short_title"))
                    .map(str::to_owned),
                artwork_url,
                web_url: Some(format!("https://www.bbc.co.uk/programmes/{pid}")),
                feed_url: None,
                episode_count: None,
                show_id: None,
                broadcaster: None,
            })
        })
        .collect())
}

#[async_trait]
impl RadioProvider for BbcProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn episodes_arrive(&self) -> &'static str {
        "Programme pages; audio from the programme's podcast feed or your download sites"
    }

    fn stations(&self) -> Vec<reelix_api_types::radio::RadioProviderStationDto> {
        NETWORKS
            .iter()
            .map(
                |(id, name, group)| reelix_api_types::radio::RadioProviderStationDto {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                    group: Some((*group).to_owned()),
                    logo_url: Some(LOGO_URL.replace("{id}", id)),
                },
            )
            .collect()
    }

    fn name(&self) -> &'static str {
        "BBC"
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn countries(&self) -> &'static [&'static str] {
        &["GB"]
    }

    fn languages(&self) -> &'static [&'static str] {
        &["en", "cy", "gd"]
    }

    fn discover_lists(&self) -> Vec<DiscoverList> {
        let stations = STATIONS
            .iter()
            .map(|(id, name)| DiscoverList::row(&format!("station-{id}"), &format!("Popular on {name}")));
        let categories = CATEGORY_GROUPS.iter().flat_map(|(group, cats)| {
            cats.iter()
                .map(|(id, name)| DiscoverList::category(&format!("category-{id}"), name).in_group(group))
        });
        // Every network as a category of its own, by group (RFX-94).
        let networks = NETWORKS.iter().map(|(id, name, group)| {
            DiscoverList::category(&format!("network-{id}"), name)
                .in_group(&format!("BBC stations: {}", group.to_lowercase()))
        });
        std::iter::once(DiscoverList::row("popular", "Popular on BBC Sounds"))
            .chain(stations)
            .chain(categories)
            .chain(networks)
            .collect()
    }

    async fn discover(&self, list: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let unknown = || ProviderError::NotFound(format!("the BBC has no Discover list {list}"));
        let limit = POPULAR_EPISODES.to_string();
        let mut params = vec![("sort", "popular"), ("limit", limit.as_str())];
        if let Some(station) = list.strip_prefix("station-") {
            let station = STATIONS
                .iter()
                .map(|(id, _)| *id)
                .find(|id| *id == station)
                .ok_or_else(unknown)?;
            params.push(("network", station));
        } else if let Some(network) = list.strip_prefix("network-") {
            let network = NETWORKS
                .iter()
                .map(|(id, _, _)| *id)
                .find(|id| *id == network)
                .ok_or_else(unknown)?;
            params.push(("network", network));
        } else if let Some(category) = list.strip_prefix("category-") {
            let category = categories().find(|id| *id == category).ok_or_else(unknown)?;
            params.push(("category", category));
        } else if list != "popular" {
            return Err(unknown());
        }
        let url =
            url::Url::parse_with_params(&format!("{}/v2/programmes/playable", self.search_base), &params)
                .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let data = self
            .search
            .get_for(url.as_str(), HeaderMap::new(), DISCOVER_TTL)
            .await?;
        popular_hits(&data)
    }

    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let url = url::Url::parse_with_params(
            &format!("{}/v2/programmes/search/container", self.search_base),
            &[("q", query.trim())],
        )
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let data = self.search.get(url.as_str(), HeaderMap::new()).await?;
        let found = search_hits(&data)?;
        // BBC Sounds leaves out many older shows: when none of its results has the name asked
        // for, the A to Z's radio shows come first.
        let key = atoz_key(query);
        if found.iter().any(|h| atoz_key(&h.title) == key) {
            return Ok(found);
        }
        let mut listed = match self.atoz(query, ATOZ_LIMIT).await {
            Ok(listed) => listed,
            Err(e) => {
                tracing::debug!(error = %e, "BBC A to Z not read");
                return Ok(found);
            }
        };
        listed.extend(
            found
                .into_iter()
                .filter(|h| !listed.iter().any(|l| l.external_id == h.external_id))
                .collect::<Vec<_>>(),
        );
        Ok(listed)
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(pid) = parse_link(link) else {
            return Ok(None);
        };
        let top = self.resolve(&pid).await?;
        let fetch = ShowFetch {
            quick: true,
            ..ShowFetch::default()
        };
        self.read(&top, &fetch).await.map(Some)
    }

    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let pid = external_id.trim();
        if !is_pid(pid) {
            return Err(ProviderError::NotFound(format!(
                "{pid} is not a BBC programme id"
            )));
        }
        let top = self.resolve(pid).await?;
        let show = self.read(&top, fetch).await?;
        // A quick look (Discover's preview) does not read the feed; following and refreshing do.
        if fetch.quick {
            return Ok(show);
        }
        Ok(self.with_podcast(show).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pids_and_links() {
        assert!(is_pid("b006s5dp"));
        assert!(is_pid("m002qrx3"));
        assert!(!is_pid("b006a5dp"), "vowels are not used");
        assert!(!is_pid("short"));
        assert_eq!(
            parse_link("https://www.bbc.co.uk/programmes/b006s5dp").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/programmes/b006s5dp/episodes/guide").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("http://bbc.co.uk/programmes/b006s5dp.json").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/sounds/brand/b006s5dp").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/sounds/series/m002qrx3").as_deref(),
            Some("m002qrx3")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/sounds/play/m002rd20").as_deref(),
            Some("m002rd20")
        );
        assert_eq!(parse_link("b006s5dp").as_deref(), Some("b006s5dp"));
        assert_eq!(parse_link("https://evil.example/programmes/b006s5dp"), None);
        assert_eq!(parse_link("https://www.bbc.co.uk/news"), None);
        assert_eq!(parse_link("https://www.bbc.co.uk/sounds/brand/nope"), None);
    }

    #[test]
    fn a_to_z_keys() {
        assert_eq!(atoz_key("Cabin Pressure"), "cabin pressure");
        assert_eq!(atoz_key("  Old Harry's   Game "), "old harrys game");
        assert_eq!(atoz_key("The Archers"), "archers");
        assert_eq!(atoz_key("Archers, The"), "archers the");
        assert_eq!(atoz_key("Café Mozart"), "cafe mozart");
        assert_eq!(atoz_key("C.A.N. 2024"), "can 2024");
        assert_eq!(atoz_key("!!!"), "");
        assert!(atoz_key("The Archers").starts_with(&atoz_key("the arch")));
        assert!(matches!(
            atoz_titles(&serde_json::json!({"atoz": {"total": 3}})),
            Err(ProviderError::Parse(_))
        ));
    }

    #[test]
    fn lengths_prefer_the_canonical_version() {
        let v = serde_json::json!({"versions": [
            {"canonical": 0, "duration": 1720},
            {"canonical": 1, "duration": 1680},
        ]});
        assert_eq!(duration(&v), Some(1680));
        assert_eq!(duration(&serde_json::json!({"versions": []})), None);
        assert_eq!(duration(&serde_json::json!({})), None);
    }

    #[test]
    fn parents() {
        let ep = serde_json::json!({"type": "episode", "pid": "m002rd20", "parent": {"programme": {
            "type": "series", "pid": "m002qrx3", "parent": {"programme": {"type": "brand", "pid": "b006s5dp"}}
        }}});
        assert_eq!(top_pid(&ep).as_deref(), Some("b006s5dp"));
        let lone = serde_json::json!({"type": "episode", "pid": "m002rd20"});
        assert_eq!(top_pid(&lone), None);
    }
}
