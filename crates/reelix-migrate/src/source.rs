//! Reading an existing *arr / Jellyseerr install over its HTTP API.
//!
//! Only the fields the migration uses are modelled; everything is `#[serde(default)]` so newer
//! or older versions of each app still deserialize.

use std::time::Duration;

use reelix_api_types::migrate::SourceKind;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::MigrateError;

const TIMEOUT: Duration = Duration::from_secs(60);
/// Largest API reply read (after decompression). Unpaged endpoints such as a big Radarr
/// `/movie` list run to tens of MB, so this is generous; it only stops runaway bodies.
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Reads a body, failing once it exceeds `limit` bytes.
async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let too_big = || format!("response larger than {} MB", limit / (1024 * 1024));
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_big());
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if out.len() + chunk.len() > limit {
            return Err(too_big());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// A connection to the app being migrated from.
#[derive(Debug, Clone)]
pub struct Source {
    pub kind: SourceKind,
    base: String,
    api_key: String,
    http: reqwest::Client,
}

impl Source {
    pub fn new(
        kind: SourceKind,
        url: &str,
        api_key: &str,
        http: reqwest::Client,
    ) -> Result<Self, MigrateError> {
        let base = url.trim().trim_end_matches('/').to_owned();
        let parsed =
            url::Url::parse(&base).map_err(|_| MigrateError::Input(format!("{url:?} is not a URL")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(MigrateError::Input(
                "the URL must start with http:// or https://".into(),
            ));
        }
        if api_key.trim().is_empty() {
            return Err(MigrateError::Input("the API key is required".into()));
        }
        Ok(Self {
            kind,
            base,
            api_key: api_key.trim().to_owned(),
            http,
        })
    }

    fn api_prefix(&self) -> &'static str {
        match self.kind {
            SourceKind::Radarr | SourceKind::Sonarr => "/api/v3",
            SourceKind::Prowlarr | SourceKind::Readarr | SourceKind::Lidarr | SourceKind::Jellyseerr => {
                "/api/v1"
            }
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, MigrateError> {
        let url = format!("{}{}{path}", self.base, self.api_prefix());
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("accept", "application/json")
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| MigrateError::Source(format!("could not reach {}: {e}", self.base)))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(MigrateError::Source(format!(
                "{} rejected the API key",
                self.kind.label()
            )));
        }
        if !status.is_success() {
            return Err(MigrateError::Source(format!(
                "{} returned HTTP {status} for {path}",
                self.kind.label()
            )));
        }
        let bytes = read_limited(resp, MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| MigrateError::Source(format!("reading {path}: {e}")))?;
        serde_json::from_slice(&bytes).map_err(|e| {
            MigrateError::Source(format!(
                "unexpected response from {} for {path} (is this really {}?): {e}",
                self.base,
                self.kind.label()
            ))
        })
    }

    /// `GET /system/status` (Jellyseerr: `/status`), which also checks the key.
    pub async fn status(&self) -> Result<SystemStatus, MigrateError> {
        let path = if self.kind == SourceKind::Jellyseerr {
            "/status"
        } else {
            "/system/status"
        };
        let s: SystemStatus = self.get(path).await?;
        if self.kind != SourceKind::Jellyseerr
            && !s.app_name.is_empty()
            && !s.app_name.eq_ignore_ascii_case(self.kind.label())
        {
            return Err(MigrateError::Input(format!(
                "this URL is a {} server, not {}",
                s.app_name,
                self.kind.label()
            )));
        }
        Ok(s)
    }

    /// Reads everything the migration can use from this source.
    pub async fn snapshot(&self) -> Result<Snapshot, MigrateError> {
        let status = self.status().await?;
        let mut s = Snapshot {
            kind: self.kind,
            version: status.version,
            ..Snapshot::default()
        };
        match self.kind {
            SourceKind::Jellyseerr => {
                s.users = self.paged::<SeerrUser>("/user").await?;
                s.requests = self.paged::<SeerrRequest>("/request").await?;
                return Ok(s);
            }
            SourceKind::Prowlarr => {
                s.tags = self.get("/tag").await?;
                s.indexers = self.get("/indexer").await?;
                return Ok(s);
            }
            _ => {}
        }
        s.tags = self.get("/tag").await?;
        s.root_folders = self.get("/rootfolder").await?;
        s.quality_profiles = self.get("/qualityprofile").await?;
        s.indexers = self.get("/indexer").await?;
        s.download_clients = self.get("/downloadclient").await?;
        s.remote_path_mappings = self.get("/remotepathmapping").await.unwrap_or_default();
        match self.kind {
            SourceKind::Radarr => s.movies = self.get("/movie").await?,
            SourceKind::Sonarr => s.series = self.get("/series").await?,
            SourceKind::Readarr => {
                s.metadata_profiles = self.get("/metadataprofile").await.unwrap_or_default();
                s.authors = self.get("/author").await?;
                s.books = self.get("/book").await?;
            }
            SourceKind::Lidarr => {
                s.metadata_profiles = self.get("/metadataprofile").await.unwrap_or_default();
                s.artists = self.get("/artist").await?;
                s.albums = self.get("/album").await?;
            }
            SourceKind::Prowlarr | SourceKind::Jellyseerr => {}
        }
        Ok(s)
    }

    /// Jellyseerr's `{pageInfo, results}` paging.
    async fn paged<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, MigrateError> {
        #[derive(Deserialize)]
        struct Page<T> {
            #[serde(rename = "pageInfo")]
            page_info: PageInfo,
            results: Vec<T>,
        }
        #[derive(Deserialize)]
        struct PageInfo {
            #[serde(default)]
            pages: u32,
        }
        const TAKE: u32 = 100;
        let mut out = Vec::new();
        let mut page = 0;
        loop {
            let sep = if path.contains('?') { '&' } else { '?' };
            let p: Page<T> = self
                .get(&format!("{path}{sep}take={TAKE}&skip={}", page * TAKE))
                .await?;
            let n = p.results.len();
            out.extend(p.results);
            page += 1;
            if n < TAKE as usize || page >= p.page_info.pages || page > 1000 {
                break;
            }
        }
        Ok(out)
    }
}

/// Everything read from one source.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub kind: SourceKind,
    pub version: String,
    pub tags: Vec<Tag>,
    pub root_folders: Vec<RootFolder>,
    pub quality_profiles: Vec<QualityProfile>,
    pub metadata_profiles: Vec<MetadataProfile>,
    pub indexers: Vec<Provider>,
    pub download_clients: Vec<Provider>,
    pub remote_path_mappings: Vec<RemotePathMapping>,
    pub movies: Vec<Movie>,
    pub series: Vec<Series>,
    pub authors: Vec<Author>,
    pub books: Vec<Book>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub users: Vec<SeerrUser>,
    pub requests: Vec<SeerrRequest>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SystemStatus {
    pub app_name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
    /// Readarr/Lidarr root folders carry their own defaults.
    pub name: String,
    pub default_quality_profile_id: Option<i64>,
    pub default_metadata_profile_id: Option<i64>,
    /// Readarr: Calibre-managed root folders.
    pub is_calibre_library: bool,
}

/// A quality profile; `items` is the *arr nested list (qualities or groups of qualities).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QualityProfile {
    pub id: i64,
    pub name: String,
    pub upgrade_allowed: bool,
    pub cutoff: i64,
    pub items: Vec<ProfileItem>,
    pub min_format_score: i64,
    pub cutoff_format_score: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProfileItem {
    /// Set on groups (items with `items`).
    pub id: Option<i64>,
    pub name: Option<String>,
    pub quality: Option<QualityRef>,
    pub items: Vec<ProfileItem>,
    pub allowed: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QualityRef {
    pub id: i64,
    pub name: String,
}

impl QualityProfile {
    /// Allowed quality names, lowest first (the *arr API lists items lowest first).
    pub fn allowed_qualities(&self) -> Vec<String> {
        fn walk(items: &[ProfileItem], parent_allowed: bool, out: &mut Vec<String>) {
            for i in items {
                let allowed = parent_allowed && i.allowed;
                if let Some(q) = &i.quality {
                    if allowed {
                        out.push(q.name.clone());
                    }
                } else {
                    walk(&i.items, allowed, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, true, &mut out);
        out
    }

    /// Name of the cutoff quality (or of the first quality in the cutoff group).
    pub fn cutoff_name(&self) -> Option<String> {
        fn find(items: &[ProfileItem], id: i64) -> Option<String> {
            for i in items {
                if let Some(q) = &i.quality
                    && q.id == id
                    && i.items.is_empty()
                {
                    return Some(q.name.clone());
                }
                if i.id == Some(id) {
                    return i
                        .items
                        .iter()
                        .rev()
                        .find_map(|c| c.quality.as_ref().map(|q| q.name.clone()));
                }
                if let Some(n) = find(&i.items, id) {
                    return Some(n);
                }
            }
            None
        }
        find(&self.items, self.cutoff)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MetadataProfile {
    pub id: i64,
    pub name: String,
}

/// An indexer or download client (`implementation` plus a `fields` list).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Provider {
    pub id: i64,
    pub name: String,
    pub implementation: String,
    /// Prowlarr: the Cardigann definition id.
    pub definition_name: Option<String>,
    pub protocol: String,
    pub enable: bool,
    /// Radarr/Sonarr indexers.
    pub enable_rss: bool,
    pub enable_automatic_search: bool,
    pub enable_interactive_search: bool,
    pub priority: i32,
    pub tags: Vec<i64>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Field {
    pub name: String,
    pub value: Value,
}

impl Provider {
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .map(|f| &f.value)
            .filter(|v| !v.is_null())
    }

    pub fn str_field(&self, name: &str) -> Option<String> {
        match self.field(name)? {
            Value::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn int_field(&self, name: &str) -> Option<i64> {
        match self.field(name)? {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn bool_field(&self, name: &str) -> Option<bool> {
        self.field(name)?.as_bool()
    }

    pub fn int_list_field(&self, name: &str) -> Vec<u32> {
        match self.field(name) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether the *arr marks secrets it no longer returns (`********`).
    pub fn is_masked(value: &str) -> bool {
        !value.is_empty() && value.chars().all(|c| c == '*')
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemotePathMapping {
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Movie {
    pub id: i64,
    pub title: String,
    pub year: i32,
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub path: String,
    pub root_folder_path: String,
    pub monitored: bool,
    pub minimum_availability: String,
    pub quality_profile_id: i64,
    pub tags: Vec<i64>,
    pub has_file: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Series {
    pub id: i64,
    pub title: String,
    pub year: i32,
    pub tvdb_id: u32,
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub path: String,
    pub root_folder_path: String,
    pub monitored: bool,
    /// Sonarr v4: `all`, `none`, … for new seasons.
    pub monitor_new_items: Option<String>,
    pub season_folder: bool,
    pub series_type: String,
    pub quality_profile_id: i64,
    pub tags: Vec<i64>,
    pub seasons: Vec<Season>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Season {
    pub season_number: u32,
    pub monitored: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Author {
    pub id: i64,
    pub author_name: String,
    pub foreign_author_id: String,
    pub path: String,
    pub root_folder_path: String,
    pub monitored: bool,
    pub monitor_new_items: Option<String>,
    pub quality_profile_id: i64,
    pub metadata_profile_id: i64,
    pub tags: Vec<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Book {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
    pub foreign_book_id: String,
    pub monitored: bool,
    pub release_date: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Artist {
    pub id: i64,
    pub artist_name: String,
    /// The MusicBrainz artist id.
    pub foreign_artist_id: String,
    pub path: String,
    pub root_folder_path: String,
    pub monitored: bool,
    pub monitor_new_items: Option<String>,
    pub quality_profile_id: i64,
    pub metadata_profile_id: i64,
    pub tags: Vec<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Album {
    pub id: i64,
    pub title: String,
    pub artist_id: i64,
    /// The MusicBrainz release-group id.
    pub foreign_album_id: String,
    pub monitored: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeerrUser {
    pub id: i64,
    pub email: Option<String>,
    pub username: Option<String>,
    pub plex_username: Option<String>,
    pub jellyfin_username: Option<String>,
    pub display_name: Option<String>,
    /// 1 = Plex, 2 = local, 3 = Jellyfin, 4 = Emby.
    pub user_type: i64,
    /// Overseerr/Jellyseerr permission bits.
    pub permissions: u64,
    pub plex_id: Option<i64>,
    pub jellyfin_user_id: Option<String>,
    pub movie_quota_limit: Option<i64>,
    pub movie_quota_days: Option<i64>,
    pub tv_quota_limit: Option<i64>,
    pub tv_quota_days: Option<i64>,
}

impl SeerrUser {
    /// The best name to log in with.
    pub fn login_name(&self) -> Option<String> {
        [
            &self.jellyfin_username,
            &self.plex_username,
            &self.username,
            &self.display_name,
            &self.email,
        ]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_owned())
        .find(|s| !s.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeerrRequest {
    pub id: i64,
    /// 1 = pending, 2 = approved, 3 = declined, 4 = failed, 5 = completed.
    pub status: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub is4k: bool,
    pub created_at: Option<String>,
    pub media: SeerrMedia,
    pub requested_by: Option<SeerrUser>,
    pub seasons: Vec<SeerrSeason>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeerrMedia {
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub media_type: String,
    /// 1 unknown, 2 pending, 3 processing, 4 partially available, 5 available.
    pub status: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeerrSeason {
    pub season_number: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_qualities_and_cutoff() {
        let p: QualityProfile = serde_json::from_value(serde_json::json!({
            "id": 1, "name": "HD", "upgradeAllowed": true, "cutoff": 1001,
            "items": [
                {"quality": {"id": 1, "name": "SDTV"}, "items": [], "allowed": false},
                {"id": 1001, "name": "WEB 1080p", "allowed": true, "items": [
                    {"quality": {"id": 3, "name": "WEBDL-1080p"}, "items": [], "allowed": true},
                    {"quality": {"id": 15, "name": "WEBRip-1080p"}, "items": [], "allowed": true}
                ]},
                {"quality": {"id": 7, "name": "Bluray-1080p"}, "items": [], "allowed": true}
            ]
        }))
        .unwrap();
        assert_eq!(
            p.allowed_qualities(),
            vec!["WEBDL-1080p", "WEBRip-1080p", "Bluray-1080p"]
        );
        assert_eq!(p.cutoff_name().as_deref(), Some("WEBRip-1080p"));
    }
}
