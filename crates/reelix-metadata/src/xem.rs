//! TheXEM client: scene ↔ TVDB numbering maps and scene names/aliases.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::MetaError;
use crate::http::{RetryPolicy, TtlCache, send};
use crate::model::TvdbId;

pub const DEFAULT_BASE_URL: &str = "https://thexem.info";

/// A season/episode/absolute triple in one numbering scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SceneNumber {
    pub season: u32,
    pub episode: u32,
    pub absolute: Option<u32>,
}

/// One episode as numbered by TVDB and by the scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct XemMapping {
    pub tvdb: SceneNumber,
    pub scene: SceneNumber,
}

/// All mappings of one show.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct XemMappings {
    pub tvdb_id: TvdbId,
    pub entries: Vec<XemMapping>,
}

impl XemMappings {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Scene numbering of a TVDB episode.
    pub fn scene_for(&self, season: u32, episode: u32) -> Option<SceneNumber> {
        self.entries
            .iter()
            .find(|m| m.tvdb.season == season && m.tvdb.episode == episode)
            .map(|m| m.scene)
    }

    /// TVDB numbering of a scene season/episode (as found in release names).
    pub fn tvdb_for_scene(&self, season: u32, episode: u32) -> Option<SceneNumber> {
        self.entries
            .iter()
            .find(|m| m.scene.season == season && m.scene.episode == episode)
            .map(|m| m.tvdb)
    }

    /// TVDB numbering of a scene absolute episode number (anime).
    pub fn tvdb_for_scene_absolute(&self, absolute: u32) -> Option<SceneNumber> {
        self.entries
            .iter()
            .find(|m| m.scene.absolute == Some(absolute))
            .map(|m| m.tvdb)
    }

    /// Scene seasons that exist for this show, sorted.
    pub fn scene_seasons(&self) -> Vec<u32> {
        let mut v: Vec<_> = self.entries.iter().map(|m| m.scene.season).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// A scene name (alias) of a show, optionally limited to one season.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneName {
    pub name: String,
    /// `None` applies to every season.
    pub season: Option<u32>,
}

/// Tunables for [`Xem`].
#[derive(Debug, Clone)]
pub struct XemOptions {
    pub base_url: String,
    pub retry: RetryPolicy,
    pub cache_ttl: Duration,
}

impl Default for XemOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            retry: RetryPolicy::default(),
            cache_ttl: Duration::from_secs(3 * 3600),
        }
    }
}

/// TheXEM client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Xem {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    opts: XemOptions,
    cache: TtlCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Envelope {
    result: String,
    data: Value,
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WNum {
    season: u32,
    episode: u32,
    absolute: Option<u32>,
}

impl From<WNum> for SceneNumber {
    fn from(n: WNum) -> Self {
        Self {
            season: n.season,
            episode: n.episode,
            absolute: n.absolute.filter(|a| *a > 0),
        }
    }
}

#[derive(Deserialize)]
struct WEntry {
    scene: Option<WNum>,
    tvdb: Option<WNum>,
}

/// Ids come back as strings or numbers depending on the endpoint.
fn as_id(v: &Value) -> Option<TvdbId> {
    v.as_u64()
        .map(|n| n as TvdbId)
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

impl Xem {
    pub fn new(http: reqwest::Client, opts: XemOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                opts,
                cache: TtlCache::new(64),
            }),
        }
    }

    /// GET returning `data`; XEM "failure" results map to `None`.
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Option<Arc<Value>>, MetaError> {
        let key = format!("{path}?{query:?}");
        if let Some(v) = self.inner.cache.get(&key) {
            return Ok(Some(v));
        }
        let url = format!("{}{}", self.inner.opts.base_url.trim_end_matches('/'), path);
        let resp = send(
            || self.inner.http.get(&url).query(query),
            None,
            &self.inner.opts.retry,
        )
        .await?;
        let st = resp.status();
        if !st.is_success() {
            return Err(MetaError::Status {
                status: st.as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        let env: Envelope = resp.json().await.map_err(|e| MetaError::Decode(e.to_string()))?;
        if env.result != "success" {
            tracing::debug!(path, message = %env.message, "xem returned no data");
            return Ok(None);
        }
        let v = Arc::new(env.data);
        self.inner.cache.insert(key, v.clone(), self.inner.opts.cache_ttl);
        Ok(Some(v))
    }

    /// Full TVDB ↔ scene mapping of a show; empty when XEM has none.
    pub async fn mappings(&self, tvdb_id: TvdbId) -> Result<XemMappings, MetaError> {
        let data = self
            .get(
                "/map/all",
                &[("id", tvdb_id.to_string()), ("origin", "tvdb".into())],
            )
            .await?;
        let entries = data
            .as_deref()
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let e = WEntry::deserialize(e).ok()?;
                        Some(XemMapping {
                            tvdb: e.tvdb?.into(),
                            scene: e.scene?.into(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(XemMappings { tvdb_id, entries })
    }

    /// TVDB ids that have any XEM mapping.
    pub async fn shows_with_mappings(&self) -> Result<HashSet<TvdbId>, MetaError> {
        let data = self.get("/map/havemap", &[("origin", "tvdb".into())]).await?;
        Ok(data
            .as_deref()
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(as_id).collect())
            .unwrap_or_default())
    }

    /// Scene names of every mapped show (`/map/allNames`).
    pub async fn all_names(&self) -> Result<HashMap<TvdbId, Vec<SceneName>>, MetaError> {
        let data = self
            .get(
                "/map/allNames",
                &[("origin", "tvdb".into()), ("seasonNumbers", "1".into())],
            )
            .await?;
        let mut out = HashMap::new();
        let Some(obj) = data.as_deref().and_then(Value::as_object) else {
            return Ok(out);
        };
        for (id, names) in obj {
            let Ok(id) = id.parse::<TvdbId>() else { continue };
            let mut list = Vec::new();
            for n in names.as_array().into_iter().flatten() {
                match n {
                    Value::String(s) => list.push(SceneName {
                        name: s.clone(),
                        season: None,
                    }),
                    // `{"Name": 2}` limits a name to a season; -1 means all seasons.
                    Value::Object(m) => {
                        for (name, season) in m {
                            let season = season
                                .as_i64()
                                .or_else(|| season.as_str().and_then(|s| s.parse().ok()))
                                .filter(|s| *s >= 0)
                                .map(|s| s as u32);
                            list.push(SceneName {
                                name: name.clone(),
                                season,
                            });
                        }
                    }
                    _ => {}
                }
            }
            out.insert(id, list);
        }
        Ok(out)
    }

    /// Scene names of one show (served from the cached `allNames` list).
    pub async fn names(&self, tvdb_id: TvdbId) -> Result<Vec<SceneName>, MetaError> {
        Ok(self.all_names().await?.remove(&tvdb_id).unwrap_or_default())
    }

    /// Names grouped by season (`None` = all seasons), handy for alias tables.
    pub async fn names_by_season(
        &self,
        tvdb_id: TvdbId,
    ) -> Result<BTreeMap<Option<u32>, Vec<String>>, MetaError> {
        let mut out: BTreeMap<Option<u32>, Vec<String>> = BTreeMap::new();
        for n in self.names(tvdb_id).await? {
            out.entry(n.season).or_default().push(n.name);
        }
        Ok(out)
    }
}
