//! Navidrome (Subsonic API with token auth: `t = md5(password + salt)`, `s = salt`).
//!
//! Subsonic has no per-folder scan, so refresh starts a library-wide scan (`startScan`).
//! When `music_folders` lists Reelix-side music roots, paths outside them are ignored, so a
//! movie import never triggers a music scan even if a caller does not filter by content kind.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use reelix_core::ContentKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::Value;

use crate::error::json as read_json;
use crate::plex::is_within;
use crate::settings::Settings;
use crate::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};

/// Subsonic API version Reelix speaks (token auth needs 1.13+).
pub const API_VERSION: &str = "1.16.1";

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: "navidrome".into(),
        name: "Navidrome".into(),
        description: Some("Starts a Navidrome library scan after album imports".into()),
        info_url: Some("https://www.navidrome.org/docs/developers/subsonic-api/".into()),
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder("http://navidrome:4533"),
            FieldSchema::new("username", "Username", FieldKind::Text)
                .required()
                .help("An admin account (only admins may start scans)"),
            FieldSchema::new("password", "Password", FieldKind::Secret).required(),
            FieldSchema::new("full_scan", "Full scan", FieldKind::Bool)
                .default_value(false)
                .help("Rescan every file instead of only changed folders (slow)")
                .advanced(),
            FieldSchema::new("music_folders", "Music folders", FieldKind::List)
                .help("Reelix paths Navidrome serves; imports elsewhere do not start a scan (empty: any)")
                .advanced(),
        ],
    }
}

/// Subsonic token for `password` and `salt`: lowercase hex `md5(password + salt)`.
pub fn token(password: &str, salt: &str) -> String {
    crate::md5::hex(format!("{password}{salt}").as_bytes())
}

/// A fresh random salt (12 hex chars) per request.
fn salt() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: SubsonicResponse,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicResponse {
    status: String,
    #[serde(default)]
    version: String,
    #[serde(rename = "type", default)]
    server_type: Option<String>,
    #[serde(default)]
    server_version: Option<String>,
    #[serde(default)]
    error: Option<SubsonicError>,
    #[serde(default)]
    scan_status: Option<ScanStatus>,
}

#[derive(Deserialize)]
struct SubsonicError {
    code: i64,
    #[serde(default)]
    message: String,
}

/// `getScanStatus` / `startScan` result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatus {
    pub scanning: bool,
    #[serde(default)]
    pub count: u64,
}

/// Navidrome adapter.
pub struct Navidrome {
    http: reqwest::Client,
    url: String,
    username: String,
    password: String,
    product: String,
    full_scan: bool,
    music_folders: Vec<String>,
}

impl Navidrome {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, MsError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            url: s.url("url")?,
            username: s.str("username")?,
            password: s.str("password")?,
            product: ctx.product.clone(),
            full_scan: s.bool("full_scan", false),
            music_folders: s.list("music_folders"),
        })
    }

    async fn call(&self, endpoint: &str, extra: &[(&str, &str)]) -> Result<SubsonicResponse, MsError> {
        let salt = salt();
        let token = token(&self.password, &salt);
        let resp = self
            .http
            .get(format!("{}/rest/{endpoint}", self.url))
            .query(&[
                ("u", self.username.as_str()),
                ("t", token.as_str()),
                ("s", salt.as_str()),
                ("v", API_VERSION),
                ("c", self.product.as_str()),
                ("f", "json"),
            ])
            .query(extra)
            .send()
            .await?;
        let env: Envelope = read_json(resp).await?;
        let r = env.response;
        if r.status == "ok" {
            return Ok(r);
        }
        let (code, message) = r.error.map_or((0, String::new()), |e| (e.code, e.message));
        Err(match code {
            // 40: wrong username or password; 41: token auth unsupported (LDAP users).
            40 | 41 => MsError::Unauthorized,
            50 => MsError::Status {
                status: 403,
                body: format!("Navidrome: {message} (an admin account is required)"),
            },
            _ => MsError::Status {
                status: 500,
                body: format!("Subsonic error {code}: {message}"),
            },
        })
    }

    /// Whether a scan is running and how many items the library has.
    pub async fn scan_status(&self) -> Result<ScanStatus, MsError> {
        self.call("getScanStatus", &[])
            .await?
            .scan_status
            .ok_or_else(|| MsError::Parse("getScanStatus: missing scanStatus".into()))
    }

    /// Starts a scan (Navidrome ignores the request while one is running).
    pub async fn start_scan(&self) -> Result<Option<ScanStatus>, MsError> {
        let extra: &[(&str, &str)] = if self.full_scan {
            &[("fullScan", "true")]
        } else {
            &[]
        };
        Ok(self.call("startScan", extra).await?.scan_status)
    }
}

#[async_trait]
impl MediaServer for Navidrome {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Navidrome
    }

    async fn test(&self) -> Result<ServerInfo, MsError> {
        let r = self.call("ping", &[]).await?;
        let name = match r.server_type.as_deref() {
            None | Some("navidrome") => "Navidrome".to_string(),
            Some(other) => other.to_string(),
        };
        Ok(ServerInfo {
            kind: MediaServerKind::Navidrome,
            id: String::new(),
            name,
            version: r.server_version.unwrap_or(r.version),
        })
    }

    async fn authenticate(&self, _username: &str, _password: &str) -> Result<ExternalUser, MsError> {
        Err(MsError::Unsupported("Navidrome logins are not supported"))
    }

    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        Err(MsError::Unsupported("Navidrome user import is not supported"))
    }

    async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError> {
        if !paths.is_empty() && !self.music_folders.is_empty() {
            let inside = paths.iter().any(|p| {
                let p = p.to_string_lossy();
                self.music_folders.iter().any(|f| is_within(&p, f))
            });
            if !inside {
                tracing::debug!("no imported path inside Navidrome's music folders; skipping scan");
                return Ok(());
            }
        }
        self.start_scan().await.map(drop)
    }

    /// Not a movie/TV server: nothing to match for availability.
    async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
        Ok(Vec::new())
    }

    async fn watch_history(&self, _user_id: &str) -> Result<Vec<WatchedItem>, MsError> {
        Ok(Vec::new())
    }

    fn content_kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Music]
    }
}

#[cfg(test)]
mod tests {
    use super::{salt, token};

    #[test]
    fn token_is_md5_of_password_and_salt() {
        assert_eq!(token("sesame", "c19b2d"), "26719a1196d2a940705a59634eb18eab");
    }

    #[test]
    fn salts_are_fresh() {
        let (a, b) = (salt(), salt());
        assert_eq!(a.len(), 12);
        assert_ne!(a, b);
    }
}
