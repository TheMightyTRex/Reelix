//! Spotify playlists (client credentials; public playlists) and a user's followed artists / saved
//! albums (OAuth refresh token of the same app, scopes `user-follow-read user-library-read`).
//!
//! Spotify knows nothing about MusicBrainz: items carry names only (plus the album UPC when
//! Spotify returns it, and the release year). The import-list sync matches them against
//! MusicBrainz by artist name, and albums by title among that artist's release groups.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::Value;

use crate::common::{self, Settings};
use crate::{BuildContext, ImportList, ListContent, ListError, ListItem, MusicListItem, dedupe_music};

pub const IMPLEMENTATION: &str = "spotify";
const PLAYLIST_PAGE: u64 = 100;
const LIBRARY_PAGE: u64 = 50;
const DEFAULT_LIMIT: u64 = 200;
const MAX_LIMIT: u64 = 2000;

const LIST_TYPES: &[(&str, &str)] = &[
    ("playlist_artists", "Playlist: artists"),
    ("playlist_albums", "Playlist: albums"),
    ("followed_artists", "Followed artists (needs refresh token)"),
    ("saved_albums", "Saved albums (needs refresh token)"),
];

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Spotify".into(),
        description: Some(
            "Artists or albums of a Spotify playlist, or a user's followed artists / saved albums. \
             Items are matched to MusicBrainz by name."
                .into(),
        ),
        info_url: Some("https://developer.spotify.com/dashboard".into()),
        fields: vec![
            FieldSchema::new("client_id", "Client id", FieldKind::Text).required(),
            FieldSchema::new("client_secret", "Client secret", FieldKind::Secret).required(),
            FieldSchema::new("list_type", "List", common::select(LIST_TYPES))
                .required()
                .default_value("playlist_artists"),
            FieldSchema::new("playlist_id", "Playlist", FieldKind::Text)
                .placeholder("Playlist id, spotify:playlist:… or https://open.spotify.com/playlist/…")
                .help("Required for playlist lists."),
            FieldSchema::new("refresh_token", "Refresh token", FieldKind::Secret).help(
                "OAuth refresh token of this app with user-follow-read and user-library-read; \
                 required for followed artists and saved albums.",
            ),
            FieldSchema::new("limit", "Maximum items", FieldKind::Number).default_value(DEFAULT_LIMIT),
            common::base_url_field("https://api.spotify.com"),
            common::sync_interval_field(),
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Playlist { id: String, albums: bool },
    FollowedArtists,
    SavedAlbums,
}

/// A configured Spotify list.
#[derive(Debug, Clone)]
pub struct SpotifyList {
    http: reqwest::Client,
    api_url: String,
    accounts_url: String,
    client_id: String,
    client_secret: String,
    refresh_token: Option<String>,
    source: Source,
    limit: u64,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
}

/// Accepts a bare id, a `spotify:playlist:` URI or an open.spotify.com URL.
fn playlist_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    let id = if let Some(rest) = s.strip_prefix("spotify:playlist:") {
        rest
    } else if let Some((_, rest)) = s.split_once("/playlist/") {
        rest.split(['/', '?', '#']).next().unwrap_or_default()
    } else {
        s
    };
    (!id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| id.to_string())
}

fn first_artist(v: Option<&Value>) -> Option<String> {
    v?.as_array()?
        .iter()
        .find_map(|a| common::value_str(a.get("name")))
}

/// A Spotify album object as a list album.
fn album_item(album: &Value) -> Option<MusicListItem> {
    let title = common::value_str(album.get("name"))?;
    let artist = first_artist(album.get("artists"))?;
    Some(MusicListItem {
        year: common::year_from_date(album.get("release_date").and_then(Value::as_str)),
        upc: common::value_str(album.get("external_ids").and_then(|e| e.get("upc"))),
        ..MusicListItem::album(title, artist, None)
    })
}

/// One playlist entry (`{"track": {...}}`, or `{"item": {...}}` in the newer API shape).
fn playlist_entry(v: &Value, albums: bool) -> Option<MusicListItem> {
    let track = v.get("track").or(v.get("item")).filter(|t| t.is_object())?;
    if track.get("is_local").and_then(Value::as_bool) == Some(true)
        || track
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "track")
    {
        return None;
    }
    if albums {
        album_item(track.get("album")?)
    } else {
        first_artist(track.get("artists")).map(|n| MusicListItem::artist(n, None))
    }
}

/// Maps the accounts service's OAuth errors (400 `invalid_client` / `invalid_grant`).
async fn token_error(resp: reqwest::Response) -> ListError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    match v.get("error").and_then(Value::as_str) {
        Some("invalid_client" | "invalid_grant" | "unauthorized_client") => ListError::Unauthorized,
        _ if status == 401 || status == 403 => ListError::Unauthorized,
        _ => ListError::Status {
            status,
            body: body.chars().take(200).collect(),
        },
    }
}

impl SpotifyList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let refresh_token = s.str("refresh_token");
        let playlist = |albums| -> Result<Source, ListError> {
            let raw = s.req("playlist_id")?;
            let id = playlist_id(&raw).ok_or_else(|| {
                ListError::InvalidSettings(format!("cannot read a playlist id from {raw:?}"))
            })?;
            Ok(Source::Playlist { id, albums })
        };
        let source = match s.req("list_type")?.as_str() {
            "playlist_artists" => playlist(false)?,
            "playlist_albums" => playlist(true)?,
            "followed_artists" => Source::FollowedArtists,
            "saved_albums" => Source::SavedAlbums,
            o => return Err(ListError::InvalidSettings(format!("unknown Spotify list {o:?}"))),
        };
        if !matches!(source, Source::Playlist { .. }) && refresh_token.is_none() {
            return Err(ListError::InvalidSettings(
                "refresh_token is required for followed artists and saved albums".into(),
            ));
        }
        Ok(Self {
            http: ctx.http.clone(),
            api_url: s.base_url(&ctx.base_urls.spotify_api),
            accounts_url: ctx.base_urls.spotify_accounts.trim_end_matches('/').to_string(),
            client_id: s.req("client_id")?,
            client_secret: s.req("client_secret")?,
            refresh_token,
            source,
            limit: s.u64("limit").unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        })
    }

    /// Client-credentials token for playlists, refresh-token grant for the user library.
    async fn token(&self) -> Result<String, ListError> {
        let form: Vec<(&str, &str)> = match (&self.source, &self.refresh_token) {
            (Source::Playlist { .. }, None) => vec![("grant_type", "client_credentials")],
            (_, Some(rt)) => vec![("grant_type", "refresh_token"), ("refresh_token", rt.as_str())],
            (_, None) => return Err(ListError::InvalidSettings("refresh_token is required".into())),
        };
        let resp = self
            .http
            .post(format!("{}/api/token", self.accounts_url))
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&form)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(token_error(resp).await);
        }
        Ok(common::json::<Token>(resp).await?.access_token)
    }

    async fn get(&self, token: &str, path: &str, query: &[(&str, String)]) -> Result<Value, ListError> {
        let req = self
            .http
            .get(format!("{}{path}", self.api_url))
            .bearer_auth(token)
            .query(query);
        common::get_json(req).await
    }

    /// Offset-paged endpoints (`items` + `total`); `next` URLs are not followed so the token is
    /// only ever sent to the configured API host.
    async fn offset_pages(
        &self,
        token: &str,
        path: &str,
        page: u64,
        limit: u64,
        mut parse: impl FnMut(&Value) -> Option<MusicListItem>,
    ) -> Result<Vec<MusicListItem>, ListError> {
        let mut out = Vec::new();
        let mut offset = 0;
        // Bounded by entries read, not items produced (playlists repeat artists).
        while offset < limit.max(page) * 5 {
            let v = self
                .get(
                    token,
                    path,
                    &[("limit", page.to_string()), ("offset", offset.to_string())],
                )
                .await?;
            let items = v
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = items.len() as u64;
            out.extend(items.iter().filter_map(&mut parse));
            offset += n;
            let total = common::value_u64(v.get("total")).unwrap_or(0);
            out = dedupe_music(out);
            if n < page || offset >= total || out.len() as u64 >= limit {
                break;
            }
        }
        Ok(out)
    }

    async fn followed_artists(&self, token: &str, limit: u64) -> Result<Vec<MusicListItem>, ListError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        while (out.len() as u64) < limit {
            let mut q = vec![
                ("type", "artist".to_string()),
                ("limit", LIBRARY_PAGE.to_string()),
            ];
            if let Some(a) = &after {
                q.push(("after", a.clone()));
            }
            let v = self.get(token, "/v1/me/following", &q).await?;
            let artists = v.get("artists").cloned().unwrap_or(Value::Null);
            let items = artists
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            out.extend(
                items
                    .iter()
                    .filter_map(|a| common::value_str(a.get("name")).map(|n| MusicListItem::artist(n, None))),
            );
            after = common::value_str(artists.get("cursors").and_then(|c| c.get("after")));
            if items.len() < LIBRARY_PAGE as usize || after.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn collect(&self, limit: u64) -> Result<Vec<MusicListItem>, ListError> {
        let token = self.token().await?;
        let items = match &self.source {
            Source::Playlist { id, albums } => {
                let albums = *albums;
                self.offset_pages(
                    &token,
                    &format!("/v1/playlists/{id}/tracks"),
                    PLAYLIST_PAGE,
                    limit,
                    |v| playlist_entry(v, albums),
                )
                .await?
            }
            Source::SavedAlbums => {
                self.offset_pages(&token, "/v1/me/albums", LIBRARY_PAGE, limit, |v| {
                    album_item(v.get("album")?)
                })
                .await?
            }
            Source::FollowedArtists => self.followed_artists(&token, limit).await?,
        };
        let mut items = dedupe_music(items);
        items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(items)
    }
}

#[async_trait]
impl ImportList for SpotifyList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    fn content(&self) -> ListContent {
        ListContent::Music
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        Ok(Vec::new())
    }

    async fn fetch_music(&self) -> Result<Vec<MusicListItem>, ListError> {
        self.collect(self.limit).await
    }

    async fn test(&self) -> Result<(), ListError> {
        self.collect(1).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn playlist_id_forms() {
        let id = "37i9dQZF1DXcBWIGoYBM5M";
        assert_eq!(playlist_id(id).as_deref(), Some(id));
        assert_eq!(
            playlist_id(&format!("spotify:playlist:{id}")).as_deref(),
            Some(id)
        );
        assert_eq!(
            playlist_id(&format!("https://open.spotify.com/playlist/{id}?si=abc")).as_deref(),
            Some(id)
        );
        assert_eq!(playlist_id("not/an id"), None);
    }

    #[test]
    fn skips_local_tracks_and_episodes() {
        assert!(
            playlist_entry(
                &json!({"track": {"is_local": true, "artists": [{"name": "A"}]}}),
                false
            )
            .is_none()
        );
        assert!(
            playlist_entry(
                &json!({"track": {"type": "episode", "artists": [{"name": "A"}]}}),
                false
            )
            .is_none()
        );
        let a = playlist_entry(
            &json!({"track": {"type": "track", "artists": [{"name": "A"}]}}),
            false,
        )
        .unwrap();
        assert_eq!(a.artist_name, "A");
    }
}
