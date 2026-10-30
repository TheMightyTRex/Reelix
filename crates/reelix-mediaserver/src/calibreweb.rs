//! Calibre-Web (no real REST API: OPDS with HTTP Basic auth plus the web login form).
//!
//! Limits: Calibre-Web reads Calibre's `metadata.db` directly, so books added to that
//! library (Reelix's `calibredb add` step) normally appear on their own and there is no
//! per-path scan. Refresh is best effort: it signs in through the login form and calls
//! `GET /reconnect`, which re-opens the Calibre database. That route only exists when
//! Calibre-Web was started with `-r`; a 404 is logged and treated as success. Calibre-Web
//! cannot import loose files from a folder, so paths are ignored.

use std::path::PathBuf;

use async_trait::async_trait;
use reelix_core::ContentKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reqwest::StatusCode;
use serde_json::Value;

use crate::error::check;
use crate::settings::Settings;
use crate::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: "calibreweb".into(),
        name: "Calibre-Web".into(),
        description: Some("Connection test and a best-effort database reconnect after ebook imports".into()),
        info_url: Some("https://github.com/janeczku/calibre-web".into()),
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder("http://calibre-web:8083"),
            FieldSchema::new("username", "Username", FieldKind::Text).required(),
            FieldSchema::new("password", "Password", FieldKind::Secret).required(),
        ],
    }
}

/// Calibre-Web adapter.
pub struct CalibreWeb {
    http: reqwest::Client,
    url: String,
    username: String,
    password: String,
}

/// The `csrf_token` hidden input's value in the login page (Flask-WTF).
pub(crate) fn csrf_token(html: &str) -> Option<String> {
    let at = html.find("name=\"csrf_token\"")?;
    let tag_start = html[..at].rfind('<')?;
    let tag_end = at + html[at..].find('>')?;
    let tag = &html[tag_start..tag_end];
    let v = tag.find("value=\"")? + "value=\"".len();
    let len = tag[v..].find('"')?;
    Some(tag[v..v + len].to_string())
}

impl CalibreWeb {
    pub fn from_settings(raw: &Value, _ctx: &BuildContext) -> Result<Self, MsError> {
        let s = Settings::new(&schema(), raw)?;
        // A private client: the login session lives in its cookie store, and redirects must
        // stay visible to tell a successful login (302) from a re-rendered form (200).
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            url: s.url("url")?,
            username: s.str("username")?,
            password: s.str("password")?,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.url))
            .basic_auth(&self.username, Some(&self.password))
    }

    /// Signs in through the web form; the session cookie stays in the client.
    async fn login(&self) -> Result<(), MsError> {
        let page = check(self.http.get(format!("{}/login", self.url)).send().await?)
            .await?
            .text()
            .await?;
        let mut form = vec![
            ("username", self.username.as_str()),
            ("password", self.password.as_str()),
            ("remember_me", "on"),
            ("next", "/"),
        ];
        let csrf = csrf_token(&page);
        if let Some(t) = &csrf {
            form.push(("csrf_token", t.as_str()));
        }
        let resp = self
            .http
            .post(format!("{}/login", self.url))
            .form(&form)
            .send()
            .await?;
        let status = resp.status();
        if status.is_redirection() {
            let to_login = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .is_some_and(|l| l.contains("/login"));
            return if to_login {
                Err(MsError::InvalidCredentials)
            } else {
                Ok(())
            };
        }
        if status == StatusCode::OK {
            // The form was rendered again with an error flash.
            return Err(MsError::InvalidCredentials);
        }
        check(resp).await.map(drop)
    }
}

#[async_trait]
impl MediaServer for CalibreWeb {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::CalibreWeb
    }

    /// The OPDS catalog root accepts HTTP Basic auth, which checks URL and credentials.
    async fn test(&self) -> Result<ServerInfo, MsError> {
        let body = check(self.get("/opds").send().await?).await?.text().await?;
        if !body.contains("<feed") {
            return Err(MsError::Parse(
                "the OPDS catalog did not return an Atom feed".into(),
            ));
        }
        Ok(ServerInfo {
            kind: MediaServerKind::CalibreWeb,
            id: String::new(),
            name: "Calibre-Web".into(),
            version: String::new(),
        })
    }

    async fn authenticate(&self, _username: &str, _password: &str) -> Result<ExternalUser, MsError> {
        Err(MsError::Unsupported("Calibre-Web logins are not supported"))
    }

    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        Err(MsError::Unsupported("Calibre-Web user import is not supported"))
    }

    /// Best effort (see the module docs); `paths` are ignored.
    async fn refresh_library(&self, _paths: &[PathBuf]) -> Result<(), MsError> {
        self.login().await?;
        let resp = self.get("/reconnect").send().await?;
        match resp.status() {
            StatusCode::NOT_FOUND => {
                tracing::debug!(
                    "Calibre-Web /reconnect is disabled (start Calibre-Web with -r); new books still \
                     appear once they are in the Calibre database"
                );
                Ok(())
            }
            s if s.is_redirection() => Err(MsError::Unauthorized),
            _ => check(resp).await.map(drop),
        }
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

#[cfg(test)]
mod tests {
    use super::csrf_token;

    #[test]
    fn finds_csrf_token_in_login_form() {
        let html = r#"<form><input id="csrf_token" name="csrf_token" type="hidden" value="IjAb.c-d_e">
            <input name="username"></form>"#;
        assert_eq!(csrf_token(html).as_deref(), Some("IjAb.c-d_e"));
        assert_eq!(csrf_token("<form></form>"), None);
    }
}
