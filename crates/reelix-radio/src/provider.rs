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
use reelix_api_types::radio::{RadioProviderDto, RadioSeriesKind};

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
}

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

    /// Shows matching `query`, best first.
    async fn search(&self, _query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Err(ProviderError::Unsupported(format!(
            "{} cannot search by name",
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

    /// Providers allowed by the settings (`enabled` empty = all).
    pub fn enabled(&self, enabled: &[String]) -> Vec<Arc<dyn RadioProvider>> {
        self.providers
            .iter()
            .filter(|p| enabled.is_empty() || enabled.iter().any(|e| e == p.id()))
            .cloned()
            .collect()
    }

    pub fn dtos(&self, enabled: &[String]) -> Vec<RadioProviderDto> {
        self.providers
            .iter()
            .map(|p| RadioProviderDto {
                id: p.id().to_owned(),
                name: p.name().to_owned(),
                supports_search: p.supports_search(),
                enabled: enabled.is_empty() || enabled.iter().any(|e| e == p.id()),
            })
            .collect()
    }
}
