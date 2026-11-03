//! Trakt: user lists, watchlists and popular/trending/anticipated feeds, plus OAuth device-code
//! helpers ([`TraktDeviceAuth`]) for obtaining and refreshing access tokens.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::common::{self, KindFilter, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "trakt";
const API_VERSION: &str = "2";
const PAGE_SIZE: u64 = 100;
const MAX_PAGES: u64 = 50;

/// What a Trakt list instance pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraktListType {
    Watchlist,
    UserList,
    Popular,
    Trending,
    Anticipated,
}

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Trakt".into(),
        description: Some("Trakt watchlists, user lists and popular/trending/anticipated feeds.".into()),
        info_url: Some("https://trakt.docs.apiary.io/".into()),
        fields: vec![
            FieldSchema::new("client_id", "Client id", FieldKind::Secret).required(),
            FieldSchema::new("client_secret", "Client secret", FieldKind::Secret)
                .help("Needed for device authorization and token refresh."),
            FieldSchema::new("access_token", "Access token", FieldKind::Secret)
                .help("Obtained via device authorization; required for private lists and 'me'."),
            FieldSchema::new("refresh_token", "Refresh token", FieldKind::Secret).advanced(),
            FieldSchema::new("token_expires_at", "Token expiry (RFC 3339)", FieldKind::Text).advanced(),
            FieldSchema::new(
                "list_type",
                "List type",
                common::select(&[
                    ("watchlist", "Watchlist"),
                    ("user_list", "User list"),
                    ("popular", "Popular"),
                    ("trending", "Trending"),
                    ("anticipated", "Anticipated"),
                ]),
            )
            .required()
            .default_value("watchlist"),
            FieldSchema::new("username", "Username", FieldKind::Text)
                .default_value("me")
                .help("'me' uses the authorized account."),
            FieldSchema::new("list_slug", "List slug", FieldKind::Text).help("Required for user lists."),
            FieldSchema::new(
                "media",
                "Media",
                common::select(&[("movies", "Movies"), ("shows", "Shows")]),
            )
            .default_value("movies")
            .help("Used by watchlist and popular/trending/anticipated feeds."),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            FieldSchema::new("limit", "Maximum items", FieldKind::Number)
                .default_value(100)
                .help("0 = no limit (capped at 5000)."),
            common::base_url_field("https://api.trakt.tv"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured Trakt list.
#[derive(Debug, Clone)]
pub struct TraktList {
    http: reqwest::Client,
    base_url: String,
    client_id: String,
    access_token: Option<String>,
    list_type: TraktListType,
    username: String,
    list_slug: Option<String>,
    shows: bool,
    filter: KindFilter,
    limit: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Ids {
    #[serde(default)]
    imdb: Option<String>,
    #[serde(default)]
    tmdb: Option<u64>,
    #[serde(default)]
    tvdb: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Media {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    year: Option<i32>,
    #[serde(default)]
    ids: Ids,
}

/// Parses one Trakt row: either `{ "movie": {...} }` / `{ "show": {...} }` or a bare media object.
fn parse_entry(v: Value, default_kind: MediaKind) -> Option<ListItem> {
    let (kind, media) = if let Some(m) = v.get("movie") {
        (MediaKind::Movie, m.clone())
    } else if let Some(s) = v.get("show") {
        (MediaKind::Series, s.clone())
    } else if v.get("ids").is_some() {
        (default_kind, v)
    } else {
        return None; // seasons, episodes, people
    };
    let m: Media = serde_json::from_value(media).ok()?;
    Some(ListItem {
        year: m.year,
        tmdb_id: m.ids.tmdb,
        imdb_id: common::imdb_id(m.ids.imdb),
        tvdb_id: m.ids.tvdb,
        ..ListItem::new(kind, m.title.unwrap_or_default())
    })
}

impl TraktList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let list_type = match s.req("list_type")?.as_str() {
            "watchlist" => TraktListType::Watchlist,
            "user_list" => TraktListType::UserList,
            "popular" => TraktListType::Popular,
            "trending" => TraktListType::Trending,
            "anticipated" => TraktListType::Anticipated,
            o => {
                return Err(ListError::InvalidSettings(format!(
                    "unknown Trakt list type {o:?}"
                )));
            }
        };
        let list_slug = s.str("list_slug");
        if list_type == TraktListType::UserList && list_slug.is_none() {
            return Err(ListError::InvalidSettings(
                "list_slug is required for user lists".into(),
            ));
        }
        let username = s.str("username").unwrap_or_else(|| "me".into());
        let access_token = s.str("access_token");
        if username == "me" && access_token.is_none() && list_type == TraktListType::Watchlist {
            return Err(ListError::InvalidSettings(
                "an access token is required for 'me'".into(),
            ));
        }
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.trakt),
            client_id: s.req("client_id")?,
            access_token,
            list_type,
            username,
            list_slug,
            shows: s.str("media").as_deref() == Some("shows"),
            filter: KindFilter::parse(s.str("filter"))?,
            limit: match s.u64("limit").unwrap_or(100) {
                0 => PAGE_SIZE * MAX_PAGES,
                n => n.min(PAGE_SIZE * MAX_PAGES),
            },
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self
            .http
            .get(format!("{}{path}", self.base_url))
            .header("trakt-api-version", API_VERSION)
            .header("trakt-api-key", &self.client_id);
        match &self.access_token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn path(&self) -> String {
        let media = if self.shows { "shows" } else { "movies" };
        let user = &self.username;
        match self.list_type {
            TraktListType::Watchlist => format!("/users/{user}/watchlist/{media}"),
            TraktListType::UserList => {
                format!(
                    "/users/{user}/lists/{}/items",
                    self.list_slug.as_deref().unwrap_or_default()
                )
            }
            TraktListType::Popular => format!("/{media}/popular"),
            TraktListType::Trending => format!("/{media}/trending"),
            TraktListType::Anticipated => format!("/{media}/anticipated"),
        }
    }

    async fn collect(&self, limit: u64) -> Result<Vec<ListItem>, ListError> {
        let path = self.path();
        let default_kind = if self.shows {
            MediaKind::Series
        } else {
            MediaKind::Movie
        };
        let per_page = limit.min(PAGE_SIZE);
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let resp = common::send(self.get(&path).query(&[("page", page), ("limit", per_page)])).await?;
            let page_count = resp
                .headers()
                .get("x-pagination-page-count")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1);
            let rows: Vec<Value> = common::json(resp).await?;
            let n = rows.len();
            out.extend(rows.into_iter().filter_map(|r| parse_entry(r, default_kind)));
            if n == 0 || page >= page_count || out.len() as u64 >= limit {
                break;
            }
        }
        out.truncate(limit as usize);
        Ok(dedupe(
            out.into_iter().filter(|i| self.filter.allows(i.kind)).collect(),
        ))
    }
}

#[async_trait]
impl ImportList for TraktList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        self.collect(self.limit).await
    }

    async fn test(&self) -> Result<(), ListError> {
        self.collect(1).await.map(|_| ())
    }
}

/// Device code returned by `POST /oauth/device/code`; show `user_code` + `verification_url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// Minimum polling interval in seconds.
    pub interval: u64,
}

/// OAuth token pair; persist into the list's `access_token`/`refresh_token` settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraktToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    /// Unix seconds when the token was issued.
    pub created_at: i64,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TraktToken {
    /// Absolute expiry time.
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(self.created_at.saturating_add(self.expires_in), 0)
    }
}

/// Result of one device-token poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    Authorized(TraktToken),
    /// User hasn't approved yet; poll again after `interval`.
    Pending,
    /// Polling too fast; increase the interval.
    SlowDown,
    Denied,
    Expired,
    AlreadyUsed,
    InvalidCode,
}

/// OAuth device-flow helper.
#[derive(Debug, Clone)]
pub struct TraktDeviceAuth {
    http: reqwest::Client,
    base_url: String,
    client_id: String,
    client_secret: String,
}

impl TraktDeviceAuth {
    /// `base_url` is usually [`crate::BaseUrls::trakt`].
    pub fn new(http: reqwest::Client, base_url: &str, client_id: &str, client_secret: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        }
    }

    fn post(&self, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base_url))
            .header("trakt-api-version", API_VERSION)
            .json(&body)
    }

    /// Starts the flow (`POST /oauth/device/code`).
    pub async fn start(&self) -> Result<DeviceCode, ListError> {
        common::get_json(self.post("/oauth/device/code", json!({ "client_id": self.client_id }))).await
    }

    /// Polls once (`POST /oauth/device/token`), mapping Trakt's status codes to [`DevicePoll`].
    pub async fn poll(&self, device_code: &str) -> Result<DevicePoll, ListError> {
        let body = json!({
            "code": device_code,
            "client_id": self.client_id,
            "client_secret": self.client_secret,
        });
        let resp = self.post("/oauth/device/token", body).send().await?;
        Ok(match resp.status().as_u16() {
            200 => DevicePoll::Authorized(common::json(resp).await?),
            400 => DevicePoll::Pending,
            404 => DevicePoll::InvalidCode,
            409 => DevicePoll::AlreadyUsed,
            410 => DevicePoll::Expired,
            418 => DevicePoll::Denied,
            429 => DevicePoll::SlowDown,
            401 | 403 => return Err(ListError::Unauthorized),
            s => {
                let body = common::error_text(resp).await;
                return Err(ListError::Status {
                    status: s,
                    body: body.chars().take(200).collect(),
                });
            }
        })
    }

    /// Polls until authorized, denied or expired, honouring `interval` and slow-down responses.
    pub async fn wait_for_token(&self, code: &DeviceCode) -> Result<TraktToken, ListError> {
        let mut interval = Duration::from_secs(code.interval);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
        loop {
            match self.poll(&code.device_code).await? {
                DevicePoll::Authorized(t) => return Ok(t),
                DevicePoll::Pending => {}
                DevicePoll::SlowDown => interval += Duration::from_secs(1),
                other => return Err(ListError::AuthorizationFailed(format!("{other:?}"))),
            }
            if tokio::time::Instant::now() + interval > deadline {
                return Err(ListError::AuthorizationFailed("Expired".into()));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Exchanges a refresh token for a new pair (`POST /oauth/token`).
    pub async fn refresh(&self, refresh_token: &str) -> Result<TraktToken, ListError> {
        let body = json!({
            "refresh_token": refresh_token,
            "client_id": self.client_id,
            "client_secret": self.client_secret,
            "redirect_uri": "urn:ietf:wg:oauth:2.0:oob",
            "grant_type": "refresh_token",
        });
        common::get_json(self.post("/oauth/token", body)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wrapped_and_bare_rows() {
        let wrapped = json!({"type": "show", "show": {"title": "S", "year": 2008, "ids": {"tvdb": 81189, "imdb": "tt0903747"}}});
        let item = parse_entry(wrapped, MediaKind::Movie).unwrap();
        assert_eq!((item.kind, item.tvdb_id), (MediaKind::Series, Some(81189)));
        let bare = json!({"title": "M", "year": 1999, "ids": {"tmdb": 603}});
        assert_eq!(parse_entry(bare, MediaKind::Movie).unwrap().tmdb_id, Some(603));
        assert!(parse_entry(json!({"type": "person", "person": {}}), MediaKind::Movie).is_none());
    }

    #[test]
    fn token_expiry() {
        let t = TraktToken {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_in: 7_776_000,
            created_at: 1_700_000_000,
            token_type: None,
            scope: None,
        };
        assert_eq!(t.expires_at().unwrap().timestamp(), 1_707_776_000);
    }
}
