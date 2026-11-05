//! The provider seam: where shows, series and episodes come from.
//!
//! A [`RadioProvider`] finds shows by name ([`RadioProvider::search`], when
//! [`RadioProvider::supports_search`]), recognises links to its own pages
//! ([`RadioProvider::preview`]) and reads a show's full structure ([`RadioProvider::show`]).
//! Providers live in [`crate::providers`], one module each, and are registered in a
//! [`ProviderRegistry`] in the order they should be asked about a pasted link (specific
//! broadcasters first, the generic podcast feed last).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reelix_api_types::radio::{RadioProviderDto, RadioSeriesKind, RadioSettingsDto};

pub use async_trait::async_trait;
/// A search result (nothing saved). `show_id` is filled in by the service.
pub use reelix_api_types::radio::RadioShowHitDto as ShowHit;

/// Provider failures, worded for the UI.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("not found: {0}")]
    NotFound(String),
    /// The provider cannot do this (e.g. search on a feed-only provider).
    #[error("{0}")]
    Unsupported(String),
    #[error("could not reach the source: {0}")]
    Network(String),
    #[error("the source sent something Reelix could not read: {0}")]
    Parse(String),
    #[error("the source asked Reelix to slow down")]
    RateLimited { retry_after: Option<Duration> },
}

/// Country/language preferences for searches (from the radio settings).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchPrefs {
    /// ISO 3166 alpha-2, upper case (`GB`).
    pub country: Option<String>,
    /// ISO 639-1, lower case (`en`).
    pub language: Option<String>,
}

/// What a refresh already knows, so providers with per-item requests can skip work.
#[derive(Debug, Clone, Default)]
pub struct ShowFetch {
    /// External ids of settled series: do not read them again (keep them out of
    /// [`ShowData::series`]; that is not a removal).
    pub skip_series: HashSet<String>,
    /// External ids of episodes whose details (length, synopsis) are already saved.
    pub known_episodes: HashSet<String>,
    /// At most this many expensive per-episode lookups (e.g. one request per episode length).
    pub max_lookups: u32,
    /// The first read when a show is added: a provider that needs many requests for the full
    /// structure may return a partial view (`complete = false`); the service then queues a
    /// full refresh.
    pub quick: bool,
}

/// A curated list a provider offers for Discover (a chart, trending shows, a category).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverList {
    /// Unique within the provider (`top`, `genre-1303`, `category-comedy`).
    pub id: String,
    pub name: String,
    /// A category (shown as a chip) rather than a row of its own.
    pub category: bool,
}

impl DiscoverList {
    pub fn row(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            category: false,
        }
    }

    pub fn category(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            category: true,
        }
    }
}

/// How long Discover lists are cached by the providers.
pub const DISCOVER_TTL: Duration = Duration::from_secs(30 * 60);

/// Most shows in one Discover list.
pub const DISCOVER_LIMIT: usize = 50;

/// The broadcaster's service a show belongs to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StationData {
    pub external_id: String,
    pub name: String,
    pub logo_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeriesData {
    /// Unique within the show (feeds use `season:N` and `specials`).
    pub external_id: String,
    pub kind: RadioSeriesKind,
    /// `None` = the source has no number (Reelix numbers it by first broadcast).
    pub number: Option<u32>,
    pub title: Option<String>,
    pub synopsis: Option<String>,
    /// How many episodes the series has, when the source says (used to settle it).
    pub expected_episodes: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpisodeData {
    /// Unique within the show (feed guid, programme pid).
    pub external_id: String,
    /// [`SeriesData::external_id`] of its series (must be in [`ShowData::series`]).
    pub series_external_id: String,
    /// `None` = the source has no number (Reelix numbers it by first broadcast).
    pub number: Option<u32>,
    pub title: String,
    pub synopsis: Option<String>,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub duration_secs: Option<u32>,
    pub enclosure_url: Option<String>,
    pub guid: Option<String>,
}

/// A show as the provider sees it now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShowData {
    pub external_id: String,
    pub title: String,
    pub synopsis: Option<String>,
    pub author: Option<String>,
    pub web_url: Option<String>,
    pub feed_url: Option<String>,
    pub artwork_url: Option<String>,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub station: Option<StationData>,
    pub series: Vec<SeriesData>,
    pub episodes: Vec<EpisodeData>,
    /// The provider that owns the show when it is not the one asked: directories that only
    /// find feeds (Apple Podcasts) hand the show to `rss`, so it is saved and refreshed as a
    /// feed. `None` = the provider asked.
    pub provider: Option<String>,
    /// The lists are the provider's complete current view (series and episodes missing from
    /// them, other than skipped series, are marked removed — never deleted). `false` for a
    /// partial read, e.g. a lookup budget ran out.
    pub complete: bool,
}

impl ShowData {
    /// The search-hit view of the show.
    pub fn hit(&self, provider: &str) -> ShowHit {
        ShowHit {
            provider: provider.to_owned(),
            external_id: self.external_id.clone(),
            title: self.title.clone(),
            synopsis: self.synopsis.clone(),
            author: self.author.clone(),
            station_name: self.station.as_ref().map(|s| s.name.clone()),
            artwork_url: self.artwork_url.clone(),
            web_url: self.web_url.clone(),
            feed_url: self.feed_url.clone(),
            episode_count: u32::try_from(self.episodes.len()).ok(),
            show_id: None,
        }
    }
}

/// A source of radio shows (podcast feeds, a broadcaster's programme catalogue, a directory).
#[async_trait]
pub trait RadioProvider: Send + Sync + 'static {
    /// Stable id stored in `radio_shows.provider` (`rss`, `bbc`, `podcastindex`, …).
    fn id(&self) -> &'static str;

    /// Display name.
    fn name(&self) -> &'static str;

    /// Whether [`search`](Self::search) works (otherwise shows are added by link only).
    fn supports_search(&self) -> bool {
        false
    }

    /// Shows of this provider are podcast feeds, re-read hourly by `radio_feed_sync`.
    fn is_feed(&self) -> bool {
        false
    }

    /// Countries (ISO 3166 alpha-2, upper case) the provider is for; empty = everywhere.
    /// Name searches leave it out when the settings name another country.
    fn countries(&self) -> &'static [&'static str] {
        &[]
    }

    /// Languages (ISO 639-1, lower case) of the provider's shows; empty = any. Name searches
    /// leave it out when the settings name another language.
    fn languages(&self) -> &'static [&'static str] {
        &[]
    }

    /// Takes the current radio settings (credentials). Called whenever the service reads its
    /// settings, before the provider is used.
    fn configure(&self, _settings: &RadioSettingsDto) {}

    /// Whether the provider has what it needs (e.g. an API key). Unconfigured providers are
    /// treated as turned off.
    fn is_configured(&self) -> bool {
        true
    }

    /// Shows matching `query`, best first.
    async fn search(&self, _query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Err(ProviderError::Unsupported(format!(
            "{} cannot search by name",
            self.name()
        )))
    }

    /// Curated lists for Discover ([`discover`](Self::discover)); empty = none.
    fn discover_lists(&self) -> Vec<DiscoverList> {
        Vec::new()
    }

    /// The shows of the list `list` (an id from [`discover_lists`](Self::discover_lists)),
    /// best first, at most [`DISCOVER_LIMIT`]. Hits may carry ids the provider resolves only
    /// when the show is opened or added (e.g. an Apple id, not yet its feed). Cached for
    /// [`DISCOVER_TTL`].
    async fn discover(&self, list: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Err(ProviderError::NotFound(format!(
            "{} has no Discover list {list}",
            self.name()
        )))
    }

    /// The show behind `link` when it is one of this provider's links (`Ok(None)` otherwise,
    /// so the next provider is asked). Must save nothing. The returned data is also used to
    /// add the show without a second read.
    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError>;

    /// The full show: series, episodes, station and artwork.
    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError>;
}

/// The providers available to the service, in link-recognition order.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn RadioProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn RadioProvider>>) -> Self {
        Self { providers }
    }

    /// Adds a provider (asked about links after the ones already registered).
    pub fn with(mut self, p: Arc<dyn RadioProvider>) -> Self {
        self.providers.push(p);
        self
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn RadioProvider>> {
        self.providers.iter().find(|p| p.id() == id).cloned()
    }

    pub fn all(&self) -> &[Arc<dyn RadioProvider>] {
        &self.providers
    }

    /// Providers allowed by the settings (`enabled` empty = all) that are configured.
    pub fn enabled(&self, enabled: &[String]) -> Vec<Arc<dyn RadioProvider>> {
        self.providers
            .iter()
            .filter(|p| p.is_configured() && (enabled.is_empty() || enabled.iter().any(|e| e == p.id())))
            .cloned()
            .collect()
    }

    /// Enabled providers that can search, for the settings' country and language: providers
    /// for that country first, then the ones for everywhere; providers for another country or
    /// language are left out (no preference set = all, in registry order).
    pub fn for_search(&self, enabled: &[String], prefs: &SearchPrefs) -> Vec<Arc<dyn RadioProvider>> {
        let mut out: Vec<_> = self
            .for_prefs(enabled, prefs)
            .into_iter()
            .filter(|p| p.supports_search())
            .collect();
        // Stable: country-specific providers that fit go first.
        out.sort_by_key(|p| p.countries().is_empty());
        out
    }

    /// Enabled providers with Discover lists, for the settings' country and language (as
    /// [`for_search`](Self::for_search)), in registry order.
    pub fn for_discover(&self, enabled: &[String], prefs: &SearchPrefs) -> Vec<Arc<dyn RadioProvider>> {
        self.for_prefs(enabled, prefs)
            .into_iter()
            .filter(|p| !p.discover_lists().is_empty())
            .collect()
    }

    /// Enabled providers not limited to another country or language than `prefs`.
    fn for_prefs(&self, enabled: &[String], prefs: &SearchPrefs) -> Vec<Arc<dyn RadioProvider>> {
        let fits = |list: &[&str], want: Option<&String>| {
            list.is_empty() || want.is_none_or(|w| list.iter().any(|x| x.eq_ignore_ascii_case(w)))
        };
        self.enabled(enabled)
            .into_iter()
            .filter(|p| {
                fits(p.countries(), prefs.country.as_ref()) && fits(p.languages(), prefs.language.as_ref())
            })
            .collect()
    }

    /// Hands the current settings to every provider ([`RadioProvider::configure`]).
    pub fn configure(&self, settings: &RadioSettingsDto) {
        for p in &self.providers {
            p.configure(settings);
        }
    }

    pub fn dtos(&self, enabled: &[String]) -> Vec<RadioProviderDto> {
        self.providers
            .iter()
            .map(|p| RadioProviderDto {
                id: p.id().to_owned(),
                name: p.name().to_owned(),
                supports_search: p.supports_search(),
                enabled: p.is_configured() && (enabled.is_empty() || enabled.iter().any(|e| e == p.id())),
            })
            .collect()
    }
}
