//! Jellyfin and Emby (shared API surface; they differ in auth header names).
//!
//! Jellyfin gets `Authorization: MediaBrowser …`; Emby gets `X-Emby-Authorization:
//! MediaBrowser …` plus `X-Emby-Token`. Both carry Client/Device/DeviceId/Version/Token.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{check, json as read_json};
use crate::settings::{PathMap, Settings};
use crate::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
    provider_ids,
};

const PAGE: usize = 500;

/// Schema for `jellyfin` or `emby`.
pub fn schema(kind: MediaServerKind) -> ProviderSchema {
    let (id, name, port) = match kind {
        MediaServerKind::Emby => ("emby", "Emby", 8096),
        _ => ("jellyfin", "Jellyfin", 8096),
    };
    ProviderSchema {
        implementation: id.into(),
        name: name.into(),
        description: Some(format!(
            "Library refresh, availability sync, watch history and {name} logins"
        )),
        info_url: None,
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder(&format!("http://{id}:{port}")),
            FieldSchema::new("api_key", "API key", FieldKind::Secret)
                .required()
                .help("Dashboard → API Keys"),
            FieldSchema::new("path_mappings", "Path mappings", FieldKind::List)
                .help("`reelix_path=server_path` when the server sees files under another path")
                .advanced(),
        ],
    }
}

/// Jellyfin or Emby server.
pub struct EmbyLike {
    kind: MediaServerKind,
    http: reqwest::Client,
    url: String,
    api_key: String,
    paths: PathMap,
    client_id: String,
    product: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SystemInfo {
    #[serde(default)]
    server_name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct Policy {
    #[serde(default)]
    is_administrator: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserDto {
    id: String,
    name: String,
    #[serde(default)]
    policy: Option<Policy>,
    #[serde(default)]
    primary_image_tag: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthResult {
    user: UserDto,
    access_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsPage {
    #[serde(default)]
    items: Vec<ItemDto>,
    #[serde(default)]
    total_record_count: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct UserData {
    #[serde(default)]
    play_count: u32,
    #[serde(default)]
    last_played_date: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemDto {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "Type", default)]
    item_type: String,
    #[serde(default)]
    production_year: Option<i32>,
    #[serde(default)]
    provider_ids: HashMap<String, Value>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    date_created: Option<String>,
    #[serde(default)]
    series_name: Option<String>,
    #[serde(default)]
    series_id: Option<String>,
    #[serde(default)]
    parent_index_number: Option<u32>,
    #[serde(default)]
    index_number: Option<u32>,
    #[serde(default)]
    user_data: Option<UserData>,
}

impl ItemDto {
    fn ids(&self) -> (Option<i64>, Option<i64>, Option<String>) {
        let pairs: Vec<(String, String)> = self
            .provider_ids
            .iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) => Some((k.clone(), s.clone())),
                Value::Number(n) => Some((k.clone(), n.to_string())),
                _ => None,
            })
            .collect();
        provider_ids(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    }
}

/// Lenient RFC 3339 parse (servers emit 7 fractional digits; bad values become `None`).
pub(crate) fn parse_date(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

impl EmbyLike {
    pub fn from_settings(kind: MediaServerKind, raw: &Value, ctx: &BuildContext) -> Result<Self, MsError> {
        let s = Settings::new(&schema(kind), raw)?;
        Ok(Self {
            kind,
            http: ctx.http.clone(),
            url: s.url("url")?,
            api_key: s.str("api_key")?,
            paths: PathMap::parse(&s.list("path_mappings"))?,
            client_id: ctx.client_id.clone(),
            product: ctx.product.clone(),
            version: ctx.version.clone(),
        })
    }

    /// `MediaBrowser Client=…, Device=…, DeviceId=…, Version=…[, Token=…]`.
    pub fn auth_header(&self, token: Option<&str>) -> String {
        let q = |s: &str| s.replace('"', "");
        let mut h = format!(
            "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
            q(&self.product),
            q(&self.product),
            q(&self.client_id),
            q(&self.version)
        );
        if let Some(t) = token {
            h.push_str(&format!(", Token=\"{}\"", q(t)));
        }
        h
    }

    fn req(&self, method: reqwest::Method, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
        let auth = self.auth_header(token);
        let b = self
            .http
            .request(method, format!("{}{path}", self.url))
            .header(reqwest::header::ACCEPT, "application/json");
        match self.kind {
            MediaServerKind::Emby => {
                let b = b.header("X-Emby-Authorization", auth);
                match token {
                    Some(t) => b.header("X-Emby-Token", t),
                    None => b,
                }
            }
            _ => b.header(reqwest::header::AUTHORIZATION, auth),
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.req(reqwest::Method::GET, path, Some(&self.api_key))
    }

    fn user(&self, u: UserDto, token: Option<String>) -> ExternalUser {
        ExternalUser {
            avatar_url: u
                .primary_image_tag
                .as_ref()
                .map(|t| format!("{}/Users/{}/Images/Primary?tag={t}", self.url, u.id)),
            id: u.id,
            username: u.name,
            email: None,
            is_admin: u.policy.unwrap_or_default().is_administrator,
            access_token: token,
        }
    }

    /// Pages through an items endpoint.
    async fn items(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<ItemDto>, MsError> {
        let mut out = Vec::new();
        loop {
            let start = out.len().to_string();
            let limit = PAGE.to_string();
            let page: ItemsPage = read_json(
                self.get(path)
                    .query(query)
                    .query(&[("StartIndex", start.as_str()), ("Limit", limit.as_str())])
                    .send()
                    .await?,
            )
            .await?;
            let n = page.items.len();
            out.extend(page.items);
            let total = page.total_record_count.unwrap_or(out.len());
            if n < PAGE || out.len() >= total {
                return Ok(out);
            }
        }
    }
}

#[async_trait]
impl MediaServer for EmbyLike {
    fn kind(&self) -> MediaServerKind {
        self.kind
    }

    async fn test(&self) -> Result<ServerInfo, MsError> {
        let info: SystemInfo = read_json(self.get("/System/Info").send().await?).await?;
        Ok(ServerInfo {
            kind: self.kind,
            id: info.id,
            name: info.server_name,
            version: info.version,
        })
    }

    async fn authenticate(&self, username: &str, password: &str) -> Result<ExternalUser, MsError> {
        let body = json!({ "Username": username, "Pw": password });
        let resp = self
            .req(reqwest::Method::POST, "/Users/AuthenticateByName", None)
            .json(&body)
            .send()
            .await?;
        if matches!(resp.status().as_u16(), 400 | 401) {
            return Err(MsError::InvalidCredentials);
        }
        let r: AuthResult = read_json(resp).await?;
        Ok(self.user(r.user, Some(r.access_token)))
    }

    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        let users: Vec<UserDto> = read_json(self.get("/Users").send().await?).await?;
        Ok(users.into_iter().map(|u| self.user(u, None)).collect())
    }

    async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError> {
        let resp = if paths.is_empty() {
            self.req(reqwest::Method::POST, "/Library/Refresh", Some(&self.api_key))
                .send()
                .await?
        } else {
            let updates: Vec<Value> = paths
                .iter()
                .map(|p| {
                    json!({
                        "Path": self.paths.to_server(&p.to_string_lossy()),
                        "UpdateType": "Modified"
                    })
                })
                .collect();
            self.req(
                reqwest::Method::POST,
                "/Library/Media/Updated",
                Some(&self.api_key),
            )
            .json(&json!({ "Updates": updates }))
            .send()
            .await?
        };
        check(resp).await?;
        Ok(())
    }

    async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
        let items = self
            .items(
                "/Items",
                &[
                    ("Recursive", "true"),
                    ("IncludeItemTypes", "Movie,Series"),
                    ("Fields", "ProviderIds,Path,DateCreated,ProductionYear"),
                ],
            )
            .await?;
        Ok(items
            .into_iter()
            .filter_map(|i| {
                let kind = match i.item_type.as_str() {
                    "Movie" => MediaKind::Movie,
                    "Series" => MediaKind::Series,
                    _ => return None,
                };
                let (tmdb_id, tvdb_id, imdb_id) = i.ids();
                Some(MsItem {
                    kind,
                    added_at: parse_date(i.date_created.as_deref()),
                    id: i.id,
                    title: i.name,
                    year: i.production_year,
                    tmdb_id,
                    tvdb_id,
                    imdb_id,
                    path: i.path,
                })
            })
            .collect())
    }

    async fn watch_history(&self, user_id: &str) -> Result<Vec<WatchedItem>, MsError> {
        let mut query = vec![
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie,Episode"),
            ("Filters", "IsPlayed"),
            ("Fields", "ProviderIds,Path,ProductionYear"),
            ("SortBy", "DatePlayed"),
            ("SortOrder", "Descending"),
        ];
        // Jellyfin 10.9+ moved user-scoped items to /Items?userId=; Emby keeps /Users/{id}/Items.
        let path = match self.kind {
            MediaServerKind::Emby => format!("/Users/{user_id}/Items"),
            _ => {
                query.push(("UserId", user_id));
                "/Items".to_string()
            }
        };
        let items = self.items(&path, &query).await?;
        Ok(items
            .into_iter()
            .filter_map(|i| {
                let is_episode = match i.item_type.as_str() {
                    "Movie" => false,
                    "Episode" => true,
                    _ => return None,
                };
                let (tmdb_id, tvdb_id, imdb_id) = i.ids();
                let ud = i.user_data.unwrap_or_default();
                Some(WatchedItem {
                    kind: if is_episode {
                        MediaKind::Series
                    } else {
                        MediaKind::Movie
                    },
                    id: i.id,
                    title: i.name,
                    series_title: i.series_name,
                    series_id: i.series_id,
                    season: i.parent_index_number.filter(|_| is_episode),
                    episode: i.index_number.filter(|_| is_episode),
                    year: i.production_year,
                    tmdb_id,
                    tvdb_id,
                    imdb_id,
                    watched_at: parse_date(ud.last_played_date.as_deref()),
                    play_count: ud.play_count.max(1),
                })
            })
            .collect())
    }
}
