//! Kavita (REST API; the API key is exchanged for a JWT via `/api/Plugin/authenticate`).
//!
//! Refresh scans the libraries whose folders contain the imported paths
//! (`POST /api/Library/scan?libraryId=…`); an empty path list scans everything
//! (`POST /api/Library/scan-all`). The JWT is cached and renewed once on a 401.

use std::path::PathBuf;

use async_trait::async_trait;
use reelix_core::ContentKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{check, json as read_json};
use crate::plex::is_within;
use crate::settings::{PathMap, Settings};
use crate::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: "kavita".into(),
        name: "Kavita".into(),
        description: Some("Scans the matching Kavita library after ebook imports".into()),
        info_url: Some("https://wiki.kavitareader.com".into()),
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder("http://kavita:5000"),
            FieldSchema::new("api_key", "API key", FieldKind::Secret)
                .required()
                .help("User settings → 3rd Party Clients → API Key (an admin account, to start scans)"),
            FieldSchema::new("path_mappings", "Path mappings", FieldKind::List)
                .help("`reelix_path=server_path` when Kavita sees files under another path")
                .advanced(),
        ],
    }
}

/// A Kavita library.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaLibrary {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub folders: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthDto {
    token: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    kavita_version: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ServerInfoDto {
    #[serde(default)]
    kavita_version: Option<String>,
    #[serde(default)]
    install_id: Option<String>,
}

/// Kavita adapter.
pub struct Kavita {
    http: reqwest::Client,
    url: String,
    api_key: String,
    product: String,
    paths: PathMap,
    jwt: Mutex<Option<String>>,
}

impl Kavita {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, MsError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            url: s.url("url")?,
            api_key: s.str("api_key")?,
            product: ctx.product.clone(),
            paths: PathMap::parse(&s.list("path_mappings"))?,
            jwt: Mutex::new(None),
        })
    }

    async fn login(&self) -> Result<AuthDto, MsError> {
        let resp = self
            .http
            .post(format!("{}/api/Plugin/authenticate", self.url))
            .query(&[
                ("apiKey", self.api_key.as_str()),
                ("pluginName", self.product.as_str()),
            ])
            .send()
            .await?;
        // Kavita answers a bad key with 401 (newer) or 400 (older).
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            return Err(MsError::Unauthorized);
        }
        read_json(resp).await
    }

    async fn token(&self, renew: bool) -> Result<String, MsError> {
        let mut jwt = self.jwt.lock().await;
        if renew || jwt.is_none() {
            *jwt = Some(self.login().await?.token);
        }
        Ok(jwt.clone().unwrap_or_default())
    }

    /// Sends an authenticated request, renewing the JWT once when it is rejected.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<reqwest::Response, MsError> {
        let mut renew = false;
        loop {
            let token = self.token(renew).await?;
            let resp = self
                .http
                .request(method.clone(), format!("{}{path}", self.url))
                .bearer_auth(token)
                .query(query)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && !renew {
                renew = true;
                continue;
            }
            return check(resp).await;
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, MsError> {
        read_json(self.send(reqwest::Method::GET, path, &[]).await?).await
    }

    /// All libraries with their folders.
    pub async fn libraries(&self) -> Result<Vec<KavitaLibrary>, MsError> {
        match self.get("/api/Library/libraries").await {
            // Kavita before 0.8 lists libraries at `/api/Library`.
            Err(MsError::NotFound(_)) => self.get("/api/Library").await,
            r => r,
        }
    }
}

#[async_trait]
impl MediaServer for Kavita {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Kavita
    }

    async fn test(&self) -> Result<ServerInfo, MsError> {
        let auth = self.login().await?;
        *self.jwt.lock().await = Some(auth.token.clone());
        let info: ServerInfoDto = self.get("/api/Server/server-info-slim").await.unwrap_or_default();
        Ok(ServerInfo {
            kind: MediaServerKind::Kavita,
            id: info.install_id.unwrap_or_default(),
            name: if auth.username.is_empty() {
                "Kavita".into()
            } else {
                format!("Kavita ({})", auth.username)
            },
            version: info.kavita_version.or(auth.kavita_version).unwrap_or_default(),
        })
    }

    async fn authenticate(&self, _username: &str, _password: &str) -> Result<ExternalUser, MsError> {
        Err(MsError::Unsupported("Kavita logins are not supported"))
    }

    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        Err(MsError::Unsupported("Kavita user import is not supported"))
    }

    async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError> {
        if paths.is_empty() {
            self.send(reqwest::Method::POST, "/api/Library/scan-all", &[])
                .await?;
            return Ok(());
        }
        let libs = self.libraries().await?;
        let mut ids: Vec<i64> = Vec::new();
        for p in paths {
            let server_path = self.paths.to_server(&p.to_string_lossy());
            match libs
                .iter()
                .find(|l| l.folders.iter().any(|f| is_within(&server_path, f)))
            {
                Some(l) if !ids.contains(&l.id) => ids.push(l.id),
                Some(_) => {}
                None => tracing::debug!(path = %server_path, "no Kavita library contains path; skipping"),
            }
        }
        for id in ids {
            let id = id.to_string();
            self.send(
                reqwest::Method::POST,
                "/api/Library/scan",
                &[("libraryId", id.as_str()), ("force", "false")],
            )
            .await?;
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
        vec![ContentKind::Ebook]
    }
}
