//! Audiobookshelf (REST API with `Authorization: Bearer <API token>`).
//!
//! Refresh scans the book libraries whose folders contain the imported paths
//! (`POST /api/libraries/{id}/scan`); an empty path list scans every book library.
//! Podcast libraries are never touched.

use std::path::PathBuf;

use async_trait::async_trait;
use reelix_core::ContentKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{check, json as read_json};
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

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token)
    }

    /// Book libraries (podcast libraries are left out).
    pub async fn libraries(&self) -> Result<Vec<AbsLibrary>, MsError> {
        let dto: LibrariesDto =
            read_json(self.req(reqwest::Method::GET, "/api/libraries").send().await?).await?;
        Ok(dto
            .libraries
            .into_iter()
            .filter(|l| l.media_type.is_empty() || l.media_type == "book")
            .collect())
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
