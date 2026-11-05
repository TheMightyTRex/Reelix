//! plex.tv account APIs: PIN login, account, friends, server discovery and the Discover
//! watchlist (`discover.provider.plex.tv`).

use serde::Deserialize;

use crate::error::json as read_json;
use crate::{BuildContext, ExternalUser, MsError, MsItem};
use reelix_core::MediaKind;

pub const PLEX_TV_URL: &str = "https://plex.tv";
pub const DISCOVER_URL: &str = "https://discover.provider.plex.tv";
pub const APP_AUTH_URL: &str = "https://app.plex.tv/auth";

/// A login PIN; send the user to [`PlexPin::auth_url`], then poll [`PlexTv::check_pin`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlexPin {
    pub id: i64,
    pub code: String,
    pub auth_url: String,
}

/// A server resource from `/api/v2/resources`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlexServer {
    pub name: String,
    pub client_identifier: String,
    pub owned: bool,
    /// Token to use against this server (differs from the account token on shared servers).
    pub access_token: Option<String>,
    pub connections: Vec<PlexConnection>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlexConnection {
    pub uri: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub relay: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinDto {
    id: i64,
    code: String,
    #[serde(default)]
    auth_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserDto {
    id: i64,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    thumb: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceDto {
    name: String,
    client_identifier: String,
    #[serde(default)]
    provides: String,
    #[serde(default)]
    owned: bool,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    connections: Vec<PlexConnection>,
}

/// `{ "MediaContainer": { "Metadata": [...] } }` shape shared by server and Discover APIs.
#[derive(Deserialize)]
pub(crate) struct Container<T> {
    #[serde(rename = "MediaContainer")]
    pub media_container: T,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataList {
    #[serde(rename = "Metadata", default)]
    pub metadata: Vec<Metadata>,
    #[serde(default)]
    pub total_size: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct GuidDto {
    pub id: String,
}

#[derive(Deserialize)]
pub(crate) struct PartDto {
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct MediaDto {
    #[serde(rename = "Part", default)]
    pub part: Vec<PartDto>,
}

#[derive(Deserialize)]
pub(crate) struct LocationDto {
    pub path: String,
}

/// Item metadata (library, history and Discover entries share this shape).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metadata {
    #[serde(default)]
    pub rating_key: Option<String>,
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub added_at: Option<i64>,
    #[serde(default)]
    pub viewed_at: Option<i64>,
    #[serde(default)]
    pub grandparent_title: Option<String>,
    #[serde(default)]
    pub grandparent_rating_key: Option<String>,
    #[serde(default)]
    pub parent_index: Option<u32>,
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(rename = "Guid", default)]
    pub guids: Vec<GuidDto>,
    #[serde(rename = "Media", default)]
    pub media: Vec<MediaDto>,
    #[serde(rename = "Location", default)]
    pub location: Vec<LocationDto>,
}

impl Metadata {
    /// `(tmdb, tvdb, imdb)` from `Guid` entries like `tmdb://603`.
    pub fn ids(&self) -> (Option<i64>, Option<i64>, Option<String>) {
        crate::provider_ids(self.guids.iter().filter_map(|g| g.id.split_once("://")))
    }

    pub fn kind(&self) -> Option<MediaKind> {
        match self.item_type.as_str() {
            "movie" => Some(MediaKind::Movie),
            "show" => Some(MediaKind::Series),
            _ => None,
        }
    }

    pub fn into_item(self) -> Option<MsItem> {
        let kind = self.kind()?;
        let (tmdb_id, tvdb_id, imdb_id) = self.ids();
        let path = self
            .media
            .first()
            .and_then(|m| m.part.first())
            .and_then(|p| p.file.clone())
            .or_else(|| self.location.first().map(|l| l.path.clone()));
        Some(MsItem {
            kind,
            id: self.rating_key.unwrap_or_default(),
            title: self.title,
            year: self.year,
            tmdb_id,
            tvdb_id,
            imdb_id,
            path,
            added_at: self.added_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        })
    }
}

/// A Discover rating key is a plain identifier (hex or digits). Anything else is not put in a
/// request path, so a reply cannot steer the follow-up request to another path or query.
fn is_safe_rating_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// plex.tv client (no server needed).
#[derive(Debug, Clone)]
pub struct PlexTv {
    http: reqwest::Client,
    client_id: String,
    product: String,
    version: String,
    plex_tv_url: String,
    discover_url: String,
}

impl PlexTv {
    pub fn new(ctx: &BuildContext) -> Self {
        Self {
            http: ctx.http.clone(),
            client_id: ctx.client_id.clone(),
            product: ctx.product.clone(),
            version: ctx.version.clone(),
            plex_tv_url: PLEX_TV_URL.into(),
            discover_url: DISCOVER_URL.into(),
        }
    }

    /// Overrides the plex.tv and Discover base URLs (tests, proxies).
    pub fn with_base_urls(mut self, plex_tv: &str, discover: &str) -> Self {
        self.plex_tv_url = plex_tv.trim_end_matches('/').into();
        self.discover_url = discover.trim_end_matches('/').into();
        self
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    fn req(&self, method: reqwest::Method, url: String, token: Option<&str>) -> reqwest::RequestBuilder {
        let b = self
            .http
            .request(method, url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header("X-Plex-Client-Identifier", &self.client_id)
            .header("X-Plex-Product", &self.product)
            .header("X-Plex-Version", &self.version);
        match token {
            Some(t) => b.header("X-Plex-Token", t),
            None => b,
        }
    }

    /// Browser URL where the user approves the PIN.
    pub fn auth_url(&self, code: &str, forward_url: Option<&str>) -> String {
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        q.append_pair("clientID", &self.client_id);
        q.append_pair("code", code);
        q.append_pair("context[device][product]", &self.product);
        if let Some(f) = forward_url {
            q.append_pair("forwardUrl", f);
        }
        format!("{APP_AUTH_URL}#?{}", q.finish())
    }

    /// Starts the PIN login flow (`POST /api/v2/pins?strong=true`).
    pub async fn create_pin(&self, forward_url: Option<&str>) -> Result<PlexPin, MsError> {
        let url = format!("{}/api/v2/pins", self.plex_tv_url);
        let pin: PinDto = read_json(
            self.req(reqwest::Method::POST, url, None)
                .query(&[("strong", "true")])
                .send()
                .await?,
        )
        .await?;
        Ok(PlexPin {
            auth_url: self.auth_url(&pin.code, forward_url),
            id: pin.id,
            code: pin.code,
        })
    }

    /// Polls a PIN; returns the account token once the user approved it.
    pub async fn check_pin(&self, id: i64) -> Result<Option<String>, MsError> {
        let url = format!("{}/api/v2/pins/{id}", self.plex_tv_url);
        let pin: PinDto = read_json(self.req(reqwest::Method::GET, url, None).send().await?).await?;
        Ok(pin.auth_token.filter(|t| !t.is_empty()))
    }

    fn user(u: UserDto, token: Option<String>, is_admin: bool) -> ExternalUser {
        ExternalUser {
            id: u.id.to_string(),
            username: u.username.or(u.title).unwrap_or_default(),
            email: u.email,
            is_admin,
            avatar_url: u.thumb,
            access_token: token,
        }
    }

    /// The account owning `token` (`GET /api/v2/user`).
    pub async fn account(&self, token: &str) -> Result<ExternalUser, MsError> {
        let url = format!("{}/api/v2/user", self.plex_tv_url);
        let u: UserDto = read_json(self.req(reqwest::Method::GET, url, Some(token)).send().await?).await?;
        Ok(Self::user(u, Some(token.to_string()), false))
    }

    /// Friends / shared users of the account.
    pub async fn friends(&self, token: &str) -> Result<Vec<ExternalUser>, MsError> {
        let url = format!("{}/api/v2/friends", self.plex_tv_url);
        let users: Vec<UserDto> =
            read_json(self.req(reqwest::Method::GET, url, Some(token)).send().await?).await?;
        Ok(users.into_iter().map(|u| Self::user(u, None, false)).collect())
    }

    /// Media servers visible to the account.
    pub async fn servers(&self, token: &str) -> Result<Vec<PlexServer>, MsError> {
        let url = format!("{}/api/v2/resources", self.plex_tv_url);
        let res: Vec<ResourceDto> = read_json(
            self.req(reqwest::Method::GET, url, Some(token))
                .query(&[("includeHttps", "1"), ("includeRelay", "1")])
                .send()
                .await?,
        )
        .await?;
        Ok(res
            .into_iter()
            .filter(|r| r.provides.split(',').any(|p| p.trim() == "server"))
            .map(|r| PlexServer {
                name: r.name,
                client_identifier: r.client_identifier,
                owned: r.owned,
                access_token: r.access_token,
                connections: r.connections,
            })
            .collect())
    }

    /// The account's Discover watchlist. With `resolve_ids`, items lacking external ids are
    /// looked up individually via `/library/metadata/{ratingKey}`.
    pub async fn watchlist(&self, token: &str, resolve_ids: bool) -> Result<Vec<MsItem>, MsError> {
        const SIZE: usize = 100;
        let mut raw = Vec::new();
        loop {
            let url = format!("{}/library/sections/watchlist/all", self.discover_url);
            let start = raw.len().to_string();
            let page: Container<MetadataList> = read_json(
                self.req(reqwest::Method::GET, url, Some(token))
                    .query(&[
                        ("includeGuids", "1"),
                        ("X-Plex-Container-Start", start.as_str()),
                        ("X-Plex-Container-Size", &SIZE.to_string()),
                    ])
                    .send()
                    .await?,
            )
            .await?;
            let page = page.media_container;
            let n = page.metadata.len();
            raw.extend(page.metadata);
            if n < SIZE || page.total_size.is_some_and(|t| raw.len() >= t) {
                break;
            }
        }
        let mut out = Vec::with_capacity(raw.len());
        for mut m in raw {
            if resolve_ids
                && m.guids.is_empty()
                && let Some(key) = m.rating_key.clone()
                && is_safe_rating_key(&key)
            {
                let url = format!("{}/library/metadata/{key}", self.discover_url);
                let detail: Container<MetadataList> =
                    read_json(self.req(reqwest::Method::GET, url, Some(token)).send().await?).await?;
                if let Some(d) = detail.media_container.metadata.into_iter().next() {
                    m.guids = d.guids;
                }
            }
            if let Some(item) = m.into_item() {
                out.push(item);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::is_safe_rating_key;

    #[test]
    fn rating_keys_are_plain_identifiers() {
        assert!(is_safe_rating_key("5d776825880197001ec967c6"));
        assert!(is_safe_rating_key("12345"));
        assert!(!is_safe_rating_key(""));
        assert!(!is_safe_rating_key("../../api/v2/user"));
        assert!(!is_safe_rating_key("1?X-Plex-Token=x"));
        assert!(!is_safe_rating_key("a/b"));
    }
}
