//! MusicBrainz client (music) with Cover Art Archive images. No API key needed, but
//! MusicBrainz requires a descriptive User-Agent and allows about one request per second.
//!
//! Every client talking to the same MusicBrainz base URL shares one process-wide rate limiter, so
//! several instances (e.g. one per service) still stay within the limit together.

mod wire;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::MetaError;
use crate::http::{RateLimiter, RetryPolicy, TtlCache, send};
use crate::music::{
    ArtistCredit, ArtistMetadata, ArtistSearchHit, CoverArt, MediumMetadata, MetadataStore,
    MusicMetadataProvider, ReleaseGroupMetadata, ReleaseGroupSearchHit, ReleaseMetadata, TrackMetadata,
};

pub const DEFAULT_BASE_URL: &str = "https://musicbrainz.org/ws/2";
pub const DEFAULT_CAA_URL: &str = "https://coverartarchive.org";

/// Provider name used as the [`MetadataStore`] namespace.
const PROVIDER: &str = "musicbrainz";
/// Browse page size (the MusicBrainz maximum).
const PAGE: usize = 100;
/// Safety stop for paging (5 000 entries).
const MAX_PAGES: usize = 50;
const SEARCH_LIMIT: usize = 25;
const RELEASE_INC: &str = "recordings+media+labels+artist-credits";

/// Tunables for [`MusicBrainz`].
#[derive(Debug, Clone)]
pub struct MusicBrainzOptions {
    pub base_url: String,
    pub caa_url: String,
    /// Sent on every request (MusicBrainz rejects anonymous clients).
    pub user_agent: String,
    pub retry: RetryPolicy,
    pub cache_ttl: Duration,
    /// Minimum spacing between requests.
    pub min_interval: Duration,
}

impl Default for MusicBrainzOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            caa_url: DEFAULT_CAA_URL.into(),
            user_agent: format!(
                "Reelix/{} ( https://github.com/demonzoned/Reelix )",
                reelix_core::VERSION
            ),
            retry: RetryPolicy::default(),
            cache_ttl: Duration::from_secs(12 * 3600),
            min_interval: Duration::from_secs(1),
        }
    }
}

/// MusicBrainz client. Cheap to clone; clones share the rate limiter and cache.
#[derive(Debug, Clone)]
pub struct MusicBrainz {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    opts: MusicBrainzOptions,
    cache: TtlCache,
    limiter: Arc<RateLimiter>,
    store: OnceLock<Arc<dyn MetadataStore>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("opts", &self.opts)
            .field("cache_entries", &self.cache.len())
            .field("store", &self.store.get().is_some())
            .finish_non_exhaustive()
    }
}

/// One limiter per (base URL, interval) for the whole process.
fn shared_limiter(base_url: &str, interval: Duration) -> Arc<RateLimiter> {
    type Limiters = Mutex<HashMap<(String, Duration), Arc<RateLimiter>>>;
    static LIMITERS: OnceLock<Limiters> = OnceLock::new();
    let mut map = LIMITERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.entry((base_url.trim_end_matches('/').to_string(), interval))
        .or_insert_with(|| Arc::new(RateLimiter::new(1, interval)))
        .clone()
}

#[derive(Clone, Copy)]
enum Host {
    MusicBrainz,
    CoverArt,
}

impl MusicBrainz {
    pub fn new(http: reqwest::Client, opts: MusicBrainzOptions) -> Self {
        let limiter = shared_limiter(&opts.base_url, opts.min_interval);
        Self {
            inner: Arc::new(Inner {
                http,
                opts,
                cache: TtlCache::new(500),
                limiter,
                store: OnceLock::new(),
            }),
        }
    }

    pub fn options(&self) -> &MusicBrainzOptions {
        &self.inner.opts
    }

    /// Installs a persistent response cache; only the first call has an effect.
    pub fn set_store(&self, store: Arc<dyn MetadataStore>) -> bool {
        self.inner.store.set(store).is_ok()
    }

    /// Drops every in-memory cached response.
    pub fn clear_cache(&self) {
        self.inner.cache.clear();
    }

    async fn get_value(
        &self,
        host: Host,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Arc<Value>, MetaError> {
        let inner = &self.inner;
        let (base, limiter) = match host {
            Host::MusicBrainz => (&inner.opts.base_url, Some(&*inner.limiter)),
            Host::CoverArt => (&inner.opts.caa_url, None),
        };
        let mut query = query.to_vec();
        if matches!(host, Host::MusicBrainz) {
            query.push(("fmt", "json".into()));
        }
        let url = format!("{}{}", base.trim_end_matches('/'), path);
        let key = format!("{url}?{query:?}");
        if let Some(v) = inner.cache.get(&key) {
            return Ok(v);
        }
        let ttl = inner.opts.cache_ttl;
        if let Some(store) = inner.store.get()
            && let Some(raw) = store.load(PROVIDER, &key).await
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
        {
            let v = Arc::new(v);
            inner.cache.insert(key, v.clone(), ttl);
            return Ok(v);
        }
        let resp = send(
            || {
                inner
                    .http
                    .get(&url)
                    .query(&query)
                    .header(reqwest::header::USER_AGENT, &inner.opts.user_agent)
                    .header(reqwest::header::ACCEPT, "application/json")
            },
            limiter,
            &inner.opts.retry,
        )
        .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if !status.is_success() {
            let message = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(String::from))
                .unwrap_or_else(|| String::from_utf8_lossy(&body).chars().take(200).collect());
            return Err(match status {
                StatusCode::NOT_FOUND => MetaError::NotFound,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => MetaError::Unauthorized(message),
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => MetaError::RateLimited {
                    retry_after_secs: None,
                },
                _ => MetaError::Status {
                    status: status.as_u16(),
                    message,
                },
            });
        }
        let value = serde_json::from_slice::<Value>(&body)?;
        if let Some(store) = inner.store.get() {
            store.save(PROVIDER, &key, &value.to_string(), ttl).await;
        }
        let value = Arc::new(value);
        inner.cache.insert(key, value.clone(), ttl);
        Ok(value)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, MetaError> {
        let v = self.get_value(Host::MusicBrainz, path, query).await?;
        Ok(T::deserialize(&*v)?)
    }

    /// Every release group credited to the artist (browse, paged).
    async fn artist_release_groups(&self, mbid: &str) -> Result<Vec<ReleaseGroupMetadata>, MetaError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page: wire::ReleaseGroupList = self
                .get(
                    "/release-group",
                    &[
                        ("artist", mbid.to_string()),
                        ("inc", "artist-credits+genres+ratings".into()),
                        ("limit", PAGE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let n = page.release_groups.len();
            out.extend(page.release_groups.into_iter().map(release_group_from));
            offset += n;
            let total = page.release_group_count.map_or(0, |c| c as usize);
            if n == 0 || offset >= total {
                break;
            }
        }
        Ok(out)
    }

    /// Cover Art Archive images of one release (`Ok(None)` when it has none).
    pub async fn release_cover_art(&self, release_mbid: &str) -> Result<Option<CoverArt>, MetaError> {
        let id = mbid(release_mbid).ok_or(MetaError::NotFound)?;
        self.caa(&format!("/release/{id}")).await
    }

    async fn caa(&self, path: &str) -> Result<Option<CoverArt>, MetaError> {
        let v = match self.get_value(Host::CoverArt, path, &[]).await {
            Ok(v) => v,
            Err(MetaError::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        let index = wire::CoverIndex::deserialize(&*v)?;
        Ok(cover_from(index))
    }
}

#[async_trait]
impl MusicMetadataProvider for MusicBrainz {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    async fn search_artists(&self, q: &str) -> Result<Vec<ArtistSearchHit>, MetaError> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let res: wire::ArtistSearch = self
            .get(
                "/artist",
                &[("query", lucene_escape(q)), ("limit", SEARCH_LIMIT.to_string())],
            )
            .await?;
        Ok(res
            .artists
            .into_iter()
            .map(|a| ArtistSearchHit {
                id: a.id,
                name: a.name,
                sort_name: a.sort_name,
                disambiguation: nonempty(a.disambiguation),
                artist_type: nonempty(a.artist_type),
                country: nonempty(a.country),
                score: a.score,
            })
            .collect())
    }

    async fn search_release_groups(
        &self,
        q: &str,
        artist: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = format!("releasegroup:({})", lucene_escape(q));
        if let Some(a) = artist.map(str::trim).filter(|a| !a.is_empty()) {
            query.push_str(&match mbid(a) {
                Some(id) => format!(" AND arid:{id}"),
                None => format!(" AND artist:({})", lucene_escape(a)),
            });
        }
        let res: wire::ReleaseGroupList = self
            .get(
                "/release-group",
                &[("query", query), ("limit", SEARCH_LIMIT.to_string())],
            )
            .await?;
        Ok(res
            .release_groups
            .into_iter()
            .map(|g| ReleaseGroupSearchHit {
                artist_id: g
                    .artist_credit
                    .first()
                    .map(|c| c.artist.id.clone())
                    .unwrap_or_default(),
                artist_name: credit_string(&g.artist_credit),
                id: g.id,
                title: g.title,
                primary_type: nonempty(g.primary_type),
                secondary_types: g.secondary_types,
                first_release_date: nonempty(g.first_release_date),
                score: g.score,
            })
            .collect())
    }

    async fn artist(&self, raw_id: &str) -> Result<ArtistMetadata, MetaError> {
        let id = mbid(raw_id).ok_or(MetaError::NotFound)?;
        let a: wire::Artist = self
            .get(&format!("/artist/{id}"), &[("inc", "genres+url-rels".into())])
            .await?;
        let mut release_groups = self.artist_release_groups(&id).await?;
        release_groups.sort_by(|a, b| {
            let key =
                |g: &ReleaseGroupMetadata| g.first_release_date.clone().unwrap_or_else(|| "9999".into());
            key(a).cmp(&key(b)).then_with(|| a.title.cmp(&b.title))
        });
        let mut links: Vec<String> = a
            .relations
            .into_iter()
            .filter_map(|r| r.url.map(|u| u.resource.trim().to_string()))
            .filter(|u| !u.is_empty())
            .collect();
        links.dedup();
        Ok(ArtistMetadata {
            id: if a.id.is_empty() { id } else { a.id },
            name: a.name,
            sort_name: a.sort_name,
            disambiguation: nonempty(a.disambiguation),
            artist_type: nonempty(a.artist_type),
            country: nonempty(a.country),
            genres: genre_names(a.genres),
            links,
            release_groups,
        })
    }

    async fn release_group(&self, raw_id: &str) -> Result<ReleaseGroupMetadata, MetaError> {
        let id = mbid(raw_id).ok_or(MetaError::NotFound)?;
        let g: wire::ReleaseGroup = self
            .get(
                &format!("/release-group/{id}"),
                &[("inc", "artist-credits+genres+ratings".into())],
            )
            .await?;
        Ok(release_group_from(g))
    }

    async fn releases(&self, release_group_mbid: &str) -> Result<Vec<ReleaseMetadata>, MetaError> {
        let id = mbid(release_group_mbid).ok_or(MetaError::NotFound)?;
        let mut out = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page: wire::ReleaseList = self
                .get(
                    "/release",
                    &[
                        ("release-group", id.clone()),
                        ("inc", RELEASE_INC.into()),
                        ("limit", PAGE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let n = page.releases.len();
            out.extend(page.releases.into_iter().map(|r| release_from(r, &id)));
            offset += n;
            let total = page.release_count.map_or(0, |c| c as usize);
            if n == 0 || offset >= total {
                break;
            }
        }
        Ok(out)
    }

    async fn release(&self, raw_id: &str) -> Result<ReleaseMetadata, MetaError> {
        let id = mbid(raw_id).ok_or(MetaError::NotFound)?;
        let r: wire::Release = self
            .get(
                &format!("/release/{id}"),
                &[("inc", format!("{RELEASE_INC}+release-groups"))],
            )
            .await?;
        Ok(release_from(r, ""))
    }

    async fn cover_art(&self, release_group_mbid: &str) -> Result<Option<CoverArt>, MetaError> {
        let id = mbid(release_group_mbid).ok_or(MetaError::NotFound)?;
        self.caa(&format!("/release-group/{id}")).await
    }
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Lower-cased MBID when `raw` is a UUID.
pub(crate) fn mbid(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    let ok = s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    ok.then_some(s)
}

/// Escapes Lucene query syntax so user input is searched literally.
pub(crate) fn lucene_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '+' | '-'
                | '&'
                | '|'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn genre_names(mut genres: Vec<wire::Genre>) -> Vec<String> {
    genres.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    genres
        .into_iter()
        .map(|g| g.name)
        .filter(|n| !n.is_empty())
        .collect()
}

fn credits(c: &[wire::ArtistCredit]) -> Vec<ArtistCredit> {
    c.iter()
        .map(|c| ArtistCredit {
            artist_id: c.artist.id.clone(),
            name: if c.name.is_empty() {
                c.artist.name.clone()
            } else {
                c.name.clone()
            },
            join_phrase: c.joinphrase.clone(),
        })
        .collect()
}

/// `Simon & Garfunkel`-style display string of a credit.
fn credit_string(c: &[wire::ArtistCredit]) -> String {
    c.iter()
        .map(|c| {
            let name = if c.name.is_empty() {
                &c.artist.name
            } else {
                &c.name
            };
            format!("{name}{}", c.joinphrase)
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn release_group_from(g: wire::ReleaseGroup) -> ReleaseGroupMetadata {
    ReleaseGroupMetadata {
        artist_credit: credits(&g.artist_credit),
        id: g.id,
        title: g.title,
        disambiguation: nonempty(g.disambiguation),
        primary_type: nonempty(g.primary_type),
        secondary_types: g.secondary_types,
        first_release_date: nonempty(g.first_release_date),
        genres: genre_names(g.genres),
        rating: g.rating.and_then(|r| r.value),
    }
}

fn release_from(r: wire::Release, group_id: &str) -> ReleaseMetadata {
    let label = r.label_info.iter().find_map(|l| {
        l.label
            .as_ref()
            .map(|l| l.name.trim().to_string())
            .filter(|n| !n.is_empty())
    });
    let catalog_number = r
        .label_info
        .iter()
        .find_map(|l| nonempty(l.catalog_number.clone()));
    let release_credit = credits(&r.artist_credit);
    let media = r
        .media
        .into_iter()
        .enumerate()
        .map(|(i, m)| MediumMetadata {
            position: if m.position == 0 { i as u32 + 1 } else { m.position },
            format: nonempty(m.format),
            title: nonempty(m.title),
            tracks: m
                .tracks
                .into_iter()
                .enumerate()
                .map(|(j, t)| {
                    let artist_credit = if !t.artist_credit.is_empty() {
                        credits(&t.artist_credit)
                    } else if !t.recording.artist_credit.is_empty() {
                        credits(&t.recording.artist_credit)
                    } else {
                        release_credit.clone()
                    };
                    let position = if t.position == 0 { j as u32 + 1 } else { t.position };
                    TrackMetadata {
                        number: if t.number.is_empty() {
                            position.to_string()
                        } else {
                            t.number
                        },
                        title: if t.title.is_empty() {
                            t.recording.title
                        } else {
                            t.title
                        },
                        length_ms: t.length.or(t.recording.length).filter(|l| *l > 0),
                        id: t.id,
                        recording_id: t.recording.id,
                        position,
                        artist_credit,
                    }
                })
                .collect(),
        })
        .collect();
    ReleaseMetadata {
        release_group_id: r
            .release_group
            .map(|g| g.id)
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| group_id.to_string()),
        id: r.id,
        title: r.title,
        status: nonempty(r.status),
        country: nonempty(r.country),
        date: nonempty(r.date),
        label,
        catalog_number,
        barcode: nonempty(r.barcode),
        disambiguation: nonempty(r.disambiguation),
        media,
    }
}

fn https(url: Option<String>) -> Option<String> {
    let u = nonempty(url)?;
    Some(match u.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => u,
    })
}

fn cover_from(index: wire::CoverIndex) -> Option<CoverArt> {
    let img = index
        .images
        .iter()
        .find(|i| i.front)
        .or_else(|| index.images.iter().find(|i| i.types.iter().any(|t| t == "Front")))
        .or_else(|| index.images.first())?;
    let t = &img.thumbnails;
    Some(CoverArt {
        front: https(img.image.clone()),
        thumb_500: https(t.t500.clone().or_else(|| t.large.clone())),
        thumb_250: https(t.t250.clone().or_else(|| t.small.clone())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbids_and_escaping() {
        assert_eq!(
            mbid(" A74B1B7F-71A5-4011-9441-D0B5E4122711 ").as_deref(),
            Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
        );
        assert_eq!(mbid("radiohead"), None);
        assert_eq!(mbid("a74b1b7f/71a5-4011-9441-d0b5e4122711"), None);
        assert_eq!(lucene_escape("AC/DC: Live!"), r"AC\/DC\: Live\!");
        assert_eq!(lucene_escape("Sigur Rós"), "Sigur Rós");
    }

    #[test]
    fn default_user_agent_identifies_reelix() {
        let o = MusicBrainzOptions::default();
        assert!(
            o.user_agent
                .starts_with(&format!("Reelix/{} (", reelix_core::VERSION))
        );
        assert_eq!(o.min_interval, Duration::from_secs(1));
        let mb = MusicBrainz::new(reqwest::Client::new(), o);
        assert_eq!(mb.name(), "musicbrainz");
    }

    #[test]
    fn limiter_is_shared_per_base_url() {
        let a = MusicBrainz::new(reqwest::Client::new(), MusicBrainzOptions::default());
        let b = MusicBrainz::new(reqwest::Client::new(), MusicBrainzOptions::default());
        assert!(Arc::ptr_eq(&a.inner.limiter, &b.inner.limiter));
        let c = MusicBrainz::new(
            reqwest::Client::new(),
            MusicBrainzOptions {
                base_url: "http://localhost:1/ws/2".into(),
                ..Default::default()
            },
        );
        assert!(!Arc::ptr_eq(&a.inner.limiter, &c.inner.limiter));
    }
}
