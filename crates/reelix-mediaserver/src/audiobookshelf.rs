//! Audiobookshelf (REST API with `Authorization: Bearer <API token>`).
//!
//! Refresh scans the book libraries whose folders contain the imported paths
//! (`POST /api/libraries/{id}/scan`); an empty path list scans every book library.
//! Podcast libraries are left out of that refresh: radio reads them with
//! [`Audiobookshelf::podcast_libraries`] and [`Audiobookshelf::podcast_episodes`], and asks
//! for a scan with [`Audiobookshelf::scan_library`] after filing.

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reelix_core::ContentKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{check, json as read_json, json_limited};
use crate::plex::is_within;
use crate::settings::{PathMap, Settings};
use crate::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: "audiobookshelf".into(),
        name: "Audiobookshelf".into(),
        description: Some("Scans the matching Audiobookshelf library after audiobook imports".into()),
        info_url: Some("https://api.audiobookshelf.org".into()),
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder("http://audiobookshelf:13378"),
            FieldSchema::new("api_key", "API token", FieldKind::Secret)
                .required()
                .help("Settings → Users → (admin user) → API token, or Settings → API Keys"),
            FieldSchema::new("ebooks", "Ebooks", FieldKind::Bool)
                .default_value(false)
                .help("Also scan after ebook imports (when Audiobookshelf serves your ebook folder)"),
            FieldSchema::new("path_mappings", "Path mappings", FieldKind::List)
                .help("`reelix_path=server_path` when Audiobookshelf sees files under another path")
                .advanced(),
        ],
    }
}

/// An Audiobookshelf library.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsLibrary {
    pub id: String,
    pub name: String,
    /// `book` or `podcast`.
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub folders: Vec<AbsFolder>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsFolder {
    #[serde(default)]
    pub id: String,
    pub full_path: String,
}

#[derive(Deserialize)]
struct LibrariesDto {
    #[serde(default)]
    libraries: Vec<AbsLibrary>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    #[serde(default)]
    server_version: Option<String>,
}

/// Largest podcast listing or batch response read.
const MAX_PODCAST_BODY: usize = 32 * 1024 * 1024;
/// Items per summary page (`GET /api/libraries/{id}/items`).
const ITEMS_PAGE: usize = 100;
/// Items per detail batch (`POST /api/items/batch/get`).
const DETAILS_BATCH: usize = 25;
/// Stop paging after this many pages even if the server keeps answering.
const MAX_PAGES: usize = 2_000;

/// One podcast episode with an audio file, as read from a podcast library.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AbsPodcastEpisode {
    pub item_id: String,
    pub episode_id: String,
    /// The podcast's title.
    pub item_title: Option<String>,
    pub title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub guid: Option<String>,
    pub enclosure_url: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<u32>,
    /// The audio file's path as Audiobookshelf sees it.
    pub path: Option<String>,
    pub size: Option<u64>,
}

#[derive(Deserialize)]
struct ItemsPageDto {
    #[serde(default)]
    results: Vec<ItemSummaryDto>,
    #[serde(default)]
    total: Option<u64>,
}

#[derive(Deserialize)]
struct ItemSummaryDto {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchDto {
    #[serde(default)]
    library_items: Vec<ItemDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemDto {
    id: String,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    media: Option<PodcastDto>,
}

#[derive(Deserialize, Default)]
struct PodcastDto {
    #[serde(default)]
    metadata: Option<PodcastMetaDto>,
    #[serde(default)]
    episodes: Vec<EpisodeDto>,
}

#[derive(Deserialize, Default)]
struct PodcastMetaDto {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeDto {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    season: Option<Value>,
    #[serde(default)]
    episode: Option<Value>,
    #[serde(default)]
    guid: Option<String>,
    #[serde(default)]
    enclosure: Option<EnclosureDto>,
    #[serde(default)]
    published_at: Option<Value>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    audio_file: Option<AudioFileDto>,
}

#[derive(Deserialize)]
struct EnclosureDto {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct AudioFileDto {
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    metadata: Option<FileMetaDto>,
}

#[derive(Deserialize)]
struct FileMetaDto {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

/// A season or episode number sent as a number or a string (`"3"`, `""`).
fn number(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `publishedAt`: unix ms (number or numeric string).
fn published(v: Option<&Value>) -> Option<DateTime<Utc>> {
    let ms = match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }?;
    (ms > 0).then(|| DateTime::from_timestamp_millis(ms)).flatten()
}

fn seconds(v: Option<f64>) -> Option<u32> {
    v.filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| d.round().min(u32::MAX as f64) as u32)
}

fn text(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

impl ItemDto {
    /// Episodes with an audio file (the rest are not in the library).
    fn into_episodes(self) -> Vec<AbsPodcastEpisode> {
        if self.media_type.as_deref().is_some_and(|t| t != "podcast") {
            return Vec::new();
        }
        let media = self.media.unwrap_or_default();
        let item_title = text(media.metadata.and_then(|m| m.title));
        media
            .episodes
            .into_iter()
            .filter_map(|e| {
                let file = e.audio_file?;
                let meta = file.metadata;
                Some(AbsPodcastEpisode {
                    item_id: self.id.clone(),
                    episode_id: e.id,
                    item_title: item_title.clone(),
                    title: text(e.title),
                    season: number(e.season.as_ref()),
                    episode: number(e.episode.as_ref()),
                    guid: text(e.guid),
                    enclosure_url: text(e.enclosure.and_then(|x| x.url)),
                    published_at: published(e.published_at.as_ref()),
                    duration_secs: seconds(e.duration).or_else(|| seconds(file.duration)),
                    size: meta.as_ref().and_then(|m| m.size).or(e.size),
                    path: text(meta.and_then(|m| m.path)),
                })
            })
            .collect()
    }
}

/// Audiobookshelf adapter.
pub struct Audiobookshelf {
    http: reqwest::Client,
    url: String,
    token: String,
    ebooks: bool,
    paths: PathMap,
}

impl Audiobookshelf {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, MsError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            url: s.url("url")?,
            token: s.str("api_key")?,
            ebooks: s.bool("ebooks", false),
            paths: PathMap::parse(&s.list("path_mappings"))?,
        })
    }

    /// The server address from the settings (also where its web app is).
    pub fn url(&self) -> &str {
        &self.url
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token)
    }

    async fn all_libraries(&self) -> Result<Vec<AbsLibrary>, MsError> {
        let dto: LibrariesDto =
            read_json(self.req(reqwest::Method::GET, "/api/libraries").send().await?).await?;
        Ok(dto.libraries)
    }

    /// Book libraries (podcast libraries are left out).
    pub async fn libraries(&self) -> Result<Vec<AbsLibrary>, MsError> {
        Ok(self
            .all_libraries()
            .await?
            .into_iter()
            .filter(|l| l.media_type.is_empty() || l.media_type == "book")
            .collect())
    }

    /// Podcast libraries (`mediaType` `podcast`).
    pub async fn podcast_libraries(&self) -> Result<Vec<AbsLibrary>, MsError> {
        Ok(self
            .all_libraries()
            .await?
            .into_iter()
            .filter(|l| l.media_type == "podcast")
            .collect())
    }

    /// Ids of every item of a library, from summary pages
    /// (`GET /api/libraries/{id}/items?limit=…&page=…&minified=1`).
    async fn item_ids(&self, library_id: &str) -> Result<Vec<String>, MsError> {
        let mut ids: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for page in 0..MAX_PAGES {
            let resp = self
                .req(
                    reqwest::Method::GET,
                    &format!("/api/libraries/{library_id}/items"),
                )
                .query(&[
                    ("limit", ITEMS_PAGE.to_string()),
                    ("page", page.to_string()),
                    ("minified", "1".into()),
                ])
                .send()
                .await?;
            let dto: ItemsPageDto = json_limited(resp, MAX_PODCAST_BODY).await?;
            let n = dto.results.len();
            let before = ids.len();
            for r in dto.results {
                if seen.insert(r.id.clone()) {
                    ids.push(r.id);
                }
            }
            // A short page, the announced total, or a page with nothing new (a server that
            // ignores `page`) ends the listing.
            if n < ITEMS_PAGE || ids.len() == before || dto.total.is_some_and(|t| ids.len() as u64 >= t) {
                break;
            }
        }
        Ok(ids)
    }

    /// Every episode with an audio file in a podcast library: item summaries page by page,
    /// then item details in batches (`POST /api/items/batch/get`). Each response is read up
    /// to 32 MB.
    pub async fn podcast_episodes(&self, library_id: &str) -> Result<Vec<AbsPodcastEpisode>, MsError> {
        let ids = self.item_ids(library_id).await?;
        let mut out = Vec::new();
        for chunk in ids.chunks(DETAILS_BATCH) {
            let resp = self
                .req(reqwest::Method::POST, "/api/items/batch/get")
                .json(&json!({ "libraryItemIds": chunk }))
                .send()
                .await?;
            let dto: BatchDto = json_limited(resp, MAX_PODCAST_BODY).await?;
            for item in dto.library_items {
                out.extend(item.into_episodes());
            }
        }
        Ok(out)
    }

    /// Asks Audiobookshelf to scan one library (`POST /api/libraries/{id}/scan`).
    pub async fn scan_library(&self, library_id: &str) -> Result<(), MsError> {
        self.scan(library_id).await
    }

    async fn scan(&self, library_id: &str) -> Result<(), MsError> {
        check(
            self.req(
                reqwest::Method::POST,
                &format!("/api/libraries/{library_id}/scan"),
            )
            .send()
            .await?,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl MediaServer for Audiobookshelf {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Audiobookshelf
    }

    /// Validates the token against `/api/libraries`; the version comes from `/status`.
    async fn test(&self) -> Result<ServerInfo, MsError> {
        let libs = self.libraries().await?;
        let status: StatusDto = match self.req(reqwest::Method::GET, "/status").send().await {
            Ok(r) => read_json(r).await.unwrap_or_default(),
            Err(_) => StatusDto::default(),
        };
        Ok(ServerInfo {
            kind: MediaServerKind::Audiobookshelf,
            id: libs.first().map(|l| l.id.clone()).unwrap_or_default(),
            name: "Audiobookshelf".into(),
            version: status.server_version.unwrap_or_default(),
        })
    }

    async fn authenticate(&self, _username: &str, _password: &str) -> Result<ExternalUser, MsError> {
        Err(MsError::Unsupported("Audiobookshelf logins are not supported"))
    }

    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        Err(MsError::Unsupported(
            "Audiobookshelf user import is not supported",
        ))
    }

    async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError> {
        let libs = self.libraries().await?;
        let mut targets: Vec<&AbsLibrary> = Vec::new();
        if paths.is_empty() {
            targets.extend(&libs);
        }
        for p in paths {
            let server_path = self.paths.to_server(&p.to_string_lossy());
            match libs
                .iter()
                .find(|l| l.folders.iter().any(|f| is_within(&server_path, &f.full_path)))
            {
                Some(l) if !targets.iter().any(|t| t.id == l.id) => targets.push(l),
                Some(_) => {}
                None => {
                    tracing::debug!(path = %server_path, "no Audiobookshelf library contains path; skipping")
                }
            }
        }
        for l in targets {
            self.scan(&l.id).await?;
        }
        Ok(())
    }

    /// Not a movie/TV server: nothing to match for availability.
    async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
        Ok(Vec::new())
    }

    async fn watch_history(&self, _user_id: &str) -> Result<Vec<WatchedItem>, MsError> {
        Ok(Vec::new())
    }

    fn content_kinds(&self) -> Vec<ContentKind> {
        let mut k = vec![ContentKind::Audiobook];
        if self.ebooks {
            k.push(ContentKind::Ebook);
        }
        k
    }
}
