//! ListenBrainz user statistics (`/1/stats/user/{user}/artists|release-groups`) and playlists
//! (`/1/playlist/{mbid}`, JSPF). ListenBrainz speaks MusicBrainz natively, so items usually carry
//! artist / release-group MBIDs. A user token is optional (private playlists).

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::Value;

use crate::common::{self, Settings};
use crate::{BuildContext, ImportList, ListContent, ListError, ListItem, MusicListItem, dedupe_music};

pub const IMPLEMENTATION: &str = "listenbrainz";
const PAGE_SIZE: u64 = 100;
const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 1000;

const LIST_TYPES: &[(&str, &str)] = &[
    ("top_artists", "Top artists"),
    ("top_release_groups", "Top albums (release groups)"),
    ("playlist_artists", "Playlist: artists"),
    ("playlist_albums", "Playlist: albums"),
];

const RANGES: &[(&str, &str)] = &[
    ("all_time", "All time"),
    ("year", "Last year"),
    ("half_yearly", "Last 6 months"),
    ("quarter", "Last quarter"),
    ("month", "Last month"),
    ("week", "Last week"),
    ("this_year", "This year"),
    ("this_month", "This month"),
    ("this_week", "This week"),
];

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "ListenBrainz".into(),
        description: Some(
            "A ListenBrainz user's top artists / albums, or the artists or albums of a playlist.".into(),
        ),
        info_url: Some("https://listenbrainz.org/".into()),
        fields: vec![
            FieldSchema::new("list_type", "List", common::select(LIST_TYPES))
                .required()
                .default_value("top_artists"),
            FieldSchema::new("username", "Username", FieldKind::Text)
                .help("Required for top artists / albums."),
            FieldSchema::new("playlist_id", "Playlist", FieldKind::Text)
                .placeholder("Playlist MBID or https://listenbrainz.org/playlist/…")
                .help("Required for playlist lists."),
            FieldSchema::new("range", "Period", common::select(RANGES))
                .default_value("all_time")
                .help("Statistics range (top artists / albums)."),
            FieldSchema::new("limit", "Maximum items", FieldKind::Number).default_value(DEFAULT_LIMIT),
            FieldSchema::new("token", "User token", FieldKind::Secret)
                .help("Optional; needed for private playlists (listenbrainz.org/settings)."),
            common::base_url_field("https://api.listenbrainz.org"),
            common::sync_interval_field(),
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    TopArtists { user: String },
    TopReleaseGroups { user: String },
    Playlist { id: String, albums: bool },
}

/// A configured ListenBrainz list.
#[derive(Debug, Clone)]
pub struct ListenBrainzList {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
    source: Source,
    range: String,
    limit: u64,
}

fn parse_artist_stat(v: &Value) -> Option<MusicListItem> {
    let name = common::value_str(v.get("artist_name")).unwrap_or_default();
    let mbid =
        common::mbid(common::value_str(v.get("artist_mbid"))).or_else(|| first_mbid(v.get("artist_mbids")));
    (!name.is_empty() || mbid.is_some()).then(|| MusicListItem::artist(name, mbid))
}

fn first_mbid(v: Option<&Value>) -> Option<String> {
    v?.as_array()?
        .iter()
        .find_map(|m| common::mbid(m.as_str().map(String::from)))
}

fn parse_release_group_stat(v: &Value) -> Option<MusicListItem> {
    let title = common::value_str(v.get("release_group_name"))?;
    let artist = common::value_str(v.get("artist_name")).unwrap_or_default();
    Some(MusicListItem {
        artist_mbid: first_mbid(v.get("artist_mbids")),
        ..MusicListItem::album(
            title,
            artist,
            common::mbid(common::value_str(v.get("release_group_mbid"))),
        )
    })
}

const JSPF_TRACK_EXT: &str = "https://musicbrainz.org/doc/jspf#track";

/// One JSPF track as an artist (first artist identifier) or an album (track `album` + creator).
fn parse_playlist_track(v: &Value, albums: bool) -> Option<MusicListItem> {
    let artist = common::value_str(v.get("creator")).unwrap_or_default();
    let ext = v.get("extension").and_then(|e| e.get(JSPF_TRACK_EXT));
    let artist_mbid = ext
        .and_then(|e| e.get("artist_identifiers"))
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find_map(|u| u.as_str().and_then(|u| common::mbid_from_url(u, "artist")))
        });
    let item = if albums {
        let title = common::value_str(v.get("album"))?;
        MusicListItem {
            artist_mbid,
            ..MusicListItem::album(title, artist, None)
        }
    } else {
        MusicListItem::artist(artist, artist_mbid)
    };
    (!item.artist_name.is_empty() || item.artist_mbid.is_some()).then_some(item)
}

/// Accepts a playlist MBID or a listenbrainz.org playlist URL.
fn playlist_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    let id = match s.split_once("/playlist/") {
        Some((_, rest)) => rest.split(['/', '?', '#']).next().unwrap_or_default(),
        None => s,
    };
    common::mbid(Some(id.to_string()))
}

impl ListenBrainzList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let list_type = s.req("list_type")?;
        let playlist = |albums| -> Result<Source, ListError> {
            let raw = s.req("playlist_id")?;
            let id = playlist_id(&raw).ok_or_else(|| {
                ListError::InvalidSettings(format!("cannot read a playlist id from {raw:?}"))
            })?;
            Ok(Source::Playlist { id, albums })
        };
        let source = match list_type.as_str() {
            "top_artists" => Source::TopArtists {
                user: s.req("username")?,
            },
            "top_release_groups" => Source::TopReleaseGroups {
                user: s.req("username")?,
            },
            "playlist_artists" => playlist(false)?,
            "playlist_albums" => playlist(true)?,
            o => {
                return Err(ListError::InvalidSettings(format!(
                    "unknown ListenBrainz list {o:?}"
                )));
            }
        };
        let range = s.str("range").unwrap_or_else(|| "all_time".into());
        if !RANGES.iter().any(|(r, _)| *r == range) {
            return Err(ListError::InvalidSettings(format!(
                "unknown ListenBrainz range {range:?}"
            )));
        }
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.listenbrainz),
            token: s.str("token"),
            source,
            range,
            limit: s.u64("limit").unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let req = self.http.get(url);
        match &self.token {
            Some(t) => req.header(reqwest::header::AUTHORIZATION, format!("Token {t}")),
            None => req,
        }
    }

    /// Paged stats; 204 means the statistics are not computed yet (empty list).
    async fn stats(&self, user: &str, entity: &str, key: &str, limit: u64) -> Result<Vec<Value>, ListError> {
        let url = format!("{}/1/stats/user/{user}/{entity}", self.base_url);
        let mut out = Vec::new();
        let mut offset = 0;
        while (out.len() as u64) < limit {
            let count = (limit - out.len() as u64).min(PAGE_SIZE);
            let req = self.get(&url).query(&[
                ("count", count.to_string()),
                ("offset", offset.to_string()),
                ("range", self.range.clone()),
            ]);
            let resp = common::send(req).await?;
            if resp.status() == reqwest::StatusCode::NO_CONTENT {
                break;
            }
            let v: Value = common::json(resp).await?;
            let page: Vec<Value> = v
                .get("payload")
                .and_then(|p| p.get(key))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = page.len() as u64;
            out.extend(page);
            offset += n;
            if n < count {
                break;
            }
        }
        Ok(out)
    }

    async fn collect(&self, limit: u64) -> Result<Vec<MusicListItem>, ListError> {
        let items: Vec<MusicListItem> = match &self.source {
            Source::TopArtists { user } => self
                .stats(user, "artists", "artists", limit)
                .await?
                .iter()
                .filter_map(parse_artist_stat)
                .collect(),
            Source::TopReleaseGroups { user } => self
                .stats(user, "release-groups", "release_groups", limit)
                .await?
                .iter()
                .filter_map(parse_release_group_stat)
                .collect(),
            Source::Playlist { id, albums } => {
                let url = format!("{}/1/playlist/{id}", self.base_url);
                let v: Value = common::get_json(self.get(&url)).await?;
                v.get("playlist")
                    .and_then(|p| p.get("track"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| parse_playlist_track(t, *albums))
                    .collect()
            }
        };
        let mut items = dedupe_music(items);
        items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(items)
    }
}

#[async_trait]
impl ImportList for ListenBrainzList {
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

    #[test]
    fn playlist_ids() {
        let id = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
        assert_eq!(playlist_id(id).as_deref(), Some(id));
        assert_eq!(
            playlist_id(&format!("https://listenbrainz.org/playlist/{id}/")).as_deref(),
            Some(id)
        );
        assert_eq!(playlist_id("nope"), None);
    }
}
