//! ListenBrainz client for music discovery: sitewide popular albums and new releases. No API
//! key needed.
//!
//! Endpoints: `/1/stats/sitewide/release-groups`, `/1/stats/sitewide/artists` and
//! `/1/explore/fresh-releases/`. Covers point at the Cover Art Archive.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::MetaError;
use crate::http::{RateLimiter, RetryPolicy, TtlCache, send};

pub const DEFAULT_BASE_URL: &str = "https://api.listenbrainz.org";
pub const DEFAULT_CAA_URL: &str = "https://coverartarchive.org";

/// Artists considered "popular" when picking new releases.
const POPULAR_ARTISTS: usize = 1000;
/// Album types shown as new releases (singles are left out).
const RELEASE_TYPES: [&str; 2] = ["Album", "EP"];

/// Time range of the sitewide statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsRange {
    Week,
    Month,
    Year,
}

impl StatsRange {
    fn as_str(self) -> &'static str {
        match self {
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

/// Tunables for [`ListenBrainz`].
#[derive(Debug, Clone)]
pub struct ListenBrainzOptions {
    pub base_url: String,
    pub caa_url: String,
    pub retry: RetryPolicy,
    /// Statistics are recomputed daily at most, so responses are cached for a while.
    pub cache_ttl: Duration,
    pub max_requests_per_minute: usize,
}

impl Default for ListenBrainzOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            caa_url: DEFAULT_CAA_URL.into(),
            retry: RetryPolicy::default(),
            cache_ttl: Duration::from_secs(6 * 3600),
            max_requests_per_minute: 30,
        }
    }
}

/// An album (MusicBrainz release group) from a ListenBrainz chart or release list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartAlbum {
    pub release_group_mbid: String,
    pub title: String,
    pub artist_name: String,
    /// First credited artist.
    pub artist_mbid: Option<String>,
    pub year: Option<i32>,
    /// Primary type (`Album`, `EP`, …) when known.
    pub album_type: Option<String>,
    pub cover_url: Option<String>,
}

/// ListenBrainz client. Cheap to clone; clones share the rate limiter and cache.
#[derive(Debug, Clone)]
pub struct ListenBrainz {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    opts: ListenBrainzOptions,
    user_agent: String,
    cache: TtlCache,
    limiter: RateLimiter,
}

impl ListenBrainz {
    pub fn new(http: reqwest::Client, opts: ListenBrainzOptions) -> Self {
        let limiter = RateLimiter::new(opts.max_requests_per_minute, Duration::from_secs(60));
        Self {
            inner: Arc::new(Inner {
                http,
                user_agent: format!(
                    "Reelix/{} ( https://github.com/demonzoned/Reelix )",
                    reelix_core::VERSION
                ),
                opts,
                cache: TtlCache::new(64),
                limiter,
            }),
        }
    }

    /// The most listened-to albums across ListenBrainz in `range`, up to `count` (each album
    /// once).
    pub async fn top_release_groups(
        &self,
        range: StatsRange,
        count: usize,
    ) -> Result<Vec<ChartAlbum>, MetaError> {
        // ListenBrainz lists some albums more than once (and some without an id), so ask for
        // extra entries to still fill `count` after de-duplicating.
        let res: Payload<wire::TopReleaseGroups> = self
            .get(
                "/1/stats/sitewide/release-groups",
                &[
                    ("range", range.as_str().into()),
                    ("count", (count * 2).min(1000).to_string()),
                ],
            )
            .await?;
        let mut seen = std::collections::HashSet::new();
        Ok(res
            .payload
            .release_groups
            .into_iter()
            .filter_map(|r| {
                let mbid = nonempty(r.release_group_mbid)?;
                let name = (r.release_group_name.to_lowercase(), r.artist_name.to_lowercase());
                if !seen.insert(mbid.clone()) || !seen.insert(format!("{}\u{0}{}", name.0, name.1)) {
                    return None;
                }
                Some(ChartAlbum {
                    cover_url: self.cover(r.caa_release_mbid.as_deref(), r.caa_id),
                    release_group_mbid: mbid,
                    title: r.release_group_name,
                    artist_name: r.artist_name,
                    artist_mbid: r.artist_mbids.into_iter().next(),
                    year: None,
                    album_type: None,
                })
            })
            .take(count)
            .collect())
    }

    /// Albums and EPs released in the last `days` days by artists among the most listened-to
    /// this year, the most popular artists first; up to `count`.
    ///
    /// ListenBrainz has no listen counts for brand-new releases, so artist popularity is what
    /// separates notable releases from the thousands published every week.
    pub async fn new_releases(&self, days: u32, count: usize) -> Result<Vec<ChartAlbum>, MetaError> {
        let artists: Payload<wire::TopArtists> = self
            .get(
                "/1/stats/sitewide/artists",
                &[
                    ("range", StatsRange::Year.as_str().into()),
                    ("count", POPULAR_ARTISTS.to_string()),
                ],
            )
            .await?;
        let mut rank: HashMap<String, usize> = HashMap::new();
        for (i, a) in artists.payload.artists.into_iter().enumerate() {
            if let Some(m) = a.artist_mbid {
                rank.entry(m).or_insert(i);
            }
        }
        let fresh: Payload<wire::FreshReleases> = self
            .get(
                "/1/explore/fresh-releases/",
                &[
                    ("days", days.clamp(1, 90).to_string()),
                    ("past", "true".into()),
                    ("future", "false".into()),
                ],
            )
            .await?;
        let mut picked: Vec<(usize, &str, ChartAlbum)> = Vec::new();
        let mut releases = fresh.payload.releases;
        // Newest first, so the stable sort below keeps the latest release of an artist first.
        releases.sort_by(|a, b| b.release_date.cmp(&a.release_date));
        for r in &releases {
            let kind = r.release_group_primary_type.as_deref().unwrap_or_default();
            if !RELEASE_TYPES.contains(&kind) {
                continue;
            }
            let Some(best) = r.artist_mbids.iter().filter_map(|m| rank.get(m)).min() else {
                continue;
            };
            let Some(mbid) = nonempty(r.release_group_mbid.clone()) else {
                continue;
            };
            if picked.iter().any(|(_, _, a)| a.release_group_mbid == mbid) {
                continue;
            }
            picked.push((
                *best,
                r.release_date.as_str(),
                ChartAlbum {
                    cover_url: self.cover(r.caa_release_mbid.as_deref(), r.caa_id),
                    release_group_mbid: mbid,
                    title: r.release_name.clone(),
                    artist_name: r.artist_credit_name.clone(),
                    artist_mbid: r.artist_mbids.first().cloned(),
                    year: r.release_date.get(..4).and_then(|y| y.parse().ok()),
                    album_type: Some(kind.to_string()),
                },
            ));
        }
        picked.sort_by_key(|(rank, _, _)| *rank);
        Ok(picked.into_iter().take(count).map(|(_, _, a)| a).collect())
    }

    /// 250px front cover of the release ListenBrainz picked for the album.
    fn cover(&self, release_mbid: Option<&str>, caa_id: Option<i64>) -> Option<String> {
        let release = release_mbid.filter(|m| !m.is_empty())?;
        let id = caa_id.filter(|id| *id > 0)?;
        Some(format!(
            "{}/release/{release}/{id}-250.jpg",
            self.inner.opts.caa_url.trim_end_matches('/')
        ))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, MetaError> {
        let v = self.get_value(path, query).await?;
        Ok(T::deserialize(&*v)?)
    }

    async fn get_value(&self, path: &str, query: &[(&str, String)]) -> Result<Arc<Value>, MetaError> {
        let inner = &self.inner;
        let key = format!("{path}?{query:?}");
        if let Some(v) = inner.cache.get(&key) {
            return Ok(v);
        }
        let url = format!("{}{}", inner.opts.base_url.trim_end_matches('/'), path);
        let resp = send(
            || {
                inner
                    .http
                    .get(&url)
                    .query(query)
                    .header(reqwest::header::USER_AGENT, &inner.user_agent)
                    .header(reqwest::header::ACCEPT, "application/json")
            },
            Some(&inner.limiter),
            &inner.opts.retry,
        )
        .await?;
        let status = resp.status();
        // 204: statistics not calculated yet.
        if status == StatusCode::NO_CONTENT {
            return Ok(Arc::new(Value::Object(Default::default())));
        }
        let body = resp.bytes().await?;
        if !status.is_success() {
            return Err(match status {
                StatusCode::NOT_FOUND => MetaError::NotFound,
                StatusCode::TOO_MANY_REQUESTS => MetaError::RateLimited {
                    retry_after_secs: None,
                },
                _ => MetaError::Status {
                    status: status.as_u16(),
                    message: String::from_utf8_lossy(&body).chars().take(200).collect(),
                },
            });
        }
        let value = Arc::new(serde_json::from_slice::<Value>(&body)?);
        inner.cache.insert(key, value.clone(), inner.opts.cache_ttl);
        Ok(value)
    }
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Payload<T: Default> {
    payload: T,
}

mod wire {
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub(super) struct TopReleaseGroups {
        pub release_groups: Vec<ReleaseGroupStat>,
    }

    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub(super) struct ReleaseGroupStat {
        pub release_group_mbid: Option<String>,
        pub release_group_name: String,
        pub artist_name: String,
        pub artist_mbids: Vec<String>,
        pub caa_release_mbid: Option<String>,
        pub caa_id: Option<i64>,
    }

    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub(super) struct TopArtists {
        pub artists: Vec<ArtistStat>,
    }

    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub(super) struct ArtistStat {
        pub artist_mbid: Option<String>,
    }

    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub(super) struct FreshReleases {
        pub releases: Vec<FreshRelease>,
    }

    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub(super) struct FreshRelease {
        pub release_group_mbid: Option<String>,
        pub release_name: String,
        pub artist_credit_name: String,
        pub artist_mbids: Vec<String>,
        pub release_date: String,
        pub release_group_primary_type: Option<String>,
        pub caa_release_mbid: Option<String>,
        pub caa_id: Option<i64>,
    }
}
