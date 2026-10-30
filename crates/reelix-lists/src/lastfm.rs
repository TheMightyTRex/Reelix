//! Last.fm user charts (`user.gettopartists`, `user.gettopalbums`, `user.getlovedtracks`) with an
//! API key. Yields artists or albums; Last.fm's artist `mbid` is a MusicBrainz artist id, but its
//! album `mbid` is a *release* id, so albums are always matched by title + artist.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::Value;

use crate::common::{self, Settings};
use crate::{BuildContext, ImportList, ListContent, ListError, ListItem, MusicListItem, dedupe_music};

pub const IMPLEMENTATION: &str = "lastfm";
const PAGE_SIZE: u64 = 200;
const MAX_PAGES: u64 = 10;
const DEFAULT_LIMIT: u64 = 50;

const LIST_TYPES: &[(&str, &str)] = &[
    ("top_artists", "Top artists"),
    ("top_albums", "Top albums"),
    ("loved_tracks", "Artists of loved tracks"),
];

const PERIODS: &[(&str, &str)] = &[
    ("overall", "All time"),
    ("12month", "Last 12 months"),
    ("6month", "Last 6 months"),
    ("3month", "Last 3 months"),
    ("1month", "Last month"),
    ("7day", "Last 7 days"),
];

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Last.fm".into(),
        description: Some("A Last.fm user's top artists, top albums or loved tracks' artists.".into()),
        info_url: Some("https://www.last.fm/api/account/create".into()),
        fields: vec![
            FieldSchema::new("api_key", "API key", FieldKind::Secret).required(),
            FieldSchema::new("username", "Username", FieldKind::Text).required(),
            FieldSchema::new("list_type", "List", common::select(LIST_TYPES))
                .required()
                .default_value("top_artists"),
            FieldSchema::new("period", "Period", common::select(PERIODS))
                .default_value("overall")
                .help("Chart period (top artists / top albums)."),
            FieldSchema::new("limit", "Maximum items", FieldKind::Number)
                .default_value(DEFAULT_LIMIT)
                .help("How many entries of the chart to import."),
            common::base_url_field("https://ws.audioscrobbler.com"),
            common::sync_interval_field(),
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    TopArtists,
    TopAlbums,
    LovedTracks,
}

impl Kind {
    /// (method, root object, entry array).
    fn api(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::TopArtists => ("user.gettopartists", "topartists", "artist"),
            Self::TopAlbums => ("user.gettopalbums", "topalbums", "album"),
            Self::LovedTracks => ("user.getlovedtracks", "lovedtracks", "track"),
        }
    }
}

/// A configured Last.fm chart.
#[derive(Debug, Clone)]
pub struct LastFmList {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    username: String,
    kind: Kind,
    period: String,
    limit: u64,
}

/// Maps Last.fm's in-body errors (`{"error": 10, "message": "..."}`).
fn body_error(v: &Value) -> Option<ListError> {
    let code = v.get("error")?.as_u64()?;
    let msg = common::value_str(v.get("message")).unwrap_or_else(|| format!("Last.fm error {code}"));
    Some(match code {
        // Invalid API key, suspended key, authentication failed, invalid session.
        4 | 9 | 10 | 26 => ListError::Unauthorized,
        6 => ListError::NotFound(msg),
        29 => ListError::RateLimited { retry_after: None },
        _ => ListError::Status {
            status: 200,
            body: msg,
        },
    })
}

fn artist_of(v: &Value) -> (String, Option<String>) {
    match v {
        Value::Object(_) => (
            common::value_str(v.get("name").or(v.get("#text"))).unwrap_or_default(),
            common::mbid(common::value_str(v.get("mbid"))),
        ),
        Value::String(s) => (s.trim().to_string(), None),
        _ => (String::new(), None),
    }
}

fn parse_entry(kind: Kind, v: &Value) -> Option<MusicListItem> {
    let item = match kind {
        Kind::TopArtists => {
            let name = common::value_str(v.get("name"))?;
            MusicListItem::artist(name, common::mbid(common::value_str(v.get("mbid"))))
        }
        Kind::LovedTracks => {
            let (name, mbid) = artist_of(v.get("artist")?);
            MusicListItem::artist(name, mbid)
        }
        Kind::TopAlbums => {
            let title = common::value_str(v.get("name"))?;
            let (artist, artist_mbid) = artist_of(v.get("artist")?);
            MusicListItem {
                artist_mbid,
                ..MusicListItem::album(title, artist, None)
            }
        }
    };
    (!item.artist_name.is_empty() || item.artist_mbid.is_some()).then_some(item)
}

impl LastFmList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let kind = match s.req("list_type")?.as_str() {
            "top_artists" => Kind::TopArtists,
            "top_albums" => Kind::TopAlbums,
            "loved_tracks" => Kind::LovedTracks,
            o => return Err(ListError::InvalidSettings(format!("unknown Last.fm list {o:?}"))),
        };
        let period = s.str("period").unwrap_or_else(|| "overall".into());
        if !PERIODS.iter().any(|(p, _)| *p == period) {
            return Err(ListError::InvalidSettings(format!(
                "unknown Last.fm period {period:?}"
            )));
        }
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.lastfm),
            api_key: s.req("api_key")?,
            username: s.req("username")?,
            kind,
            period,
            limit: s
                .u64("limit")
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(1, PAGE_SIZE * MAX_PAGES),
        })
    }

    async fn collect(&self, limit: u64) -> Result<Vec<MusicListItem>, ListError> {
        let (method, root, entry) = self.kind.api();
        let url = format!("{}/2.0/", self.base_url);
        let per_page = limit.min(PAGE_SIZE);
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut q = vec![
                ("method", method.to_string()),
                ("user", self.username.clone()),
                ("api_key", self.api_key.clone()),
                ("format", "json".into()),
                ("limit", per_page.to_string()),
                ("page", page.to_string()),
            ];
            if self.kind != Kind::LovedTracks {
                q.push(("period", self.period.clone()));
            }
            let resp = self.http.get(&url).query(&q).send().await?;
            let status = resp.status();
            let bytes = resp.bytes().await?;
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if let Some(e) = body_error(&v) {
                return Err(e);
            }
            if !status.is_success() {
                return Err(match status.as_u16() {
                    401 | 403 => ListError::Unauthorized,
                    404 => ListError::NotFound("/2.0/".into()),
                    429 => ListError::RateLimited { retry_after: None },
                    s => ListError::Status {
                        status: s,
                        body: String::from_utf8_lossy(&bytes).chars().take(200).collect(),
                    },
                });
            }
            let obj = v
                .get(root)
                .ok_or_else(|| ListError::Parse(format!("Last.fm response lacks {root:?}")))?;
            // A single entry is returned as an object rather than an array.
            let entries: Vec<&Value> = match obj.get(entry) {
                Some(Value::Array(a)) => a.iter().collect(),
                Some(o @ Value::Object(_)) => vec![o],
                _ => Vec::new(),
            };
            let n = entries.len() as u64;
            out.extend(entries.into_iter().filter_map(|e| parse_entry(self.kind, e)));
            let total_pages =
                common::value_u64(obj.get("@attr").and_then(|a| a.get("totalPages"))).unwrap_or(1);
            if out.len() as u64 >= limit || n < per_page || page >= total_pages {
                break;
            }
        }
        let mut out = dedupe_music(out);
        out.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(out)
    }
}

#[async_trait]
impl ImportList for LastFmList {
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
    fn parses_entries() {
        let a = parse_entry(
            Kind::TopArtists,
            &json!({"name": "Radiohead", "mbid": "A74B1B7F-71A5-4011-9441-D0B5E4122711"}),
        )
        .unwrap();
        assert_eq!(
            a.artist_mbid.as_deref(),
            Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
        );
        let t = parse_entry(
            Kind::LovedTracks,
            &json!({"name": "x", "artist": {"name": "Björk", "mbid": ""}}),
        )
        .unwrap();
        assert_eq!((t.artist_name.as_str(), t.artist_mbid), ("Björk", None));
        assert!(body_error(&json!({"error": 10, "message": "Invalid API key"})).is_some());
    }
}
