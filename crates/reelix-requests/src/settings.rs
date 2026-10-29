//! Request and security settings (stored in the `settings` table).

use reelix_api_types::requests::{RequestSettingsDto, SecuritySettingsDto};
use reelix_core::provider::SECRET_MASK;

use crate::{Error, RequestService, Result};

pub const REQUEST_SETTINGS_KEY: &str = "requests.settings";
pub const SECURITY_SETTINGS_KEY: &str = "auth.security";

impl RequestService {
    pub async fn request_settings(&self) -> Result<RequestSettingsDto> {
        Ok(self
            .db
            .settings()
            .get::<RequestSettingsDto>(REQUEST_SETTINGS_KEY)
            .await?
            .unwrap_or_default())
    }

    pub async fn set_request_settings(&self, s: RequestSettingsDto) -> Result<RequestSettingsDto> {
        if !["announced", "in_cinemas", "released"].contains(&s.minimum_availability.as_str()) {
            return Err(Error::Validation(
                "minimum_availability must be announced, in_cinemas or released".into(),
            ));
        }
        if !["standard", "daily", "anime"].contains(&s.series_type.as_str()) {
            return Err(Error::Validation(
                "series_type must be standard, daily or anime".into(),
            ));
        }
        if s.movie_quota_days == 0 || s.tv_quota_days == 0 {
            return Err(Error::Validation("quota days must be at least 1".into()));
        }
        if s.default_permissions.is_admin() {
            return Err(Error::Validation(
                "default permissions cannot include ADMIN".into(),
            ));
        }
        self.db.settings().set(REQUEST_SETTINGS_KEY, &s).await?;
        self.changed("settings", "updated", 0, None);
        Ok(s)
    }

    /// Security settings with the OIDC client secret decrypted (internal use).
    pub async fn security_settings(&self) -> Result<SecuritySettingsDto> {
        let mut s = self
            .db
            .settings()
            .get::<SecuritySettingsDto>(SECURITY_SETTINGS_KEY)
            .await?
            .unwrap_or_default();
        if !s.oidc.client_secret.is_empty() {
            s.oidc.client_secret = self.secrets.decrypt(&s.oidc.client_secret)?;
        }
        Ok(s)
    }

    /// Security settings for display: the client secret is masked.
    pub async fn security_settings_masked(&self) -> Result<SecuritySettingsDto> {
        let mut s = self.security_settings().await?;
        if !s.oidc.client_secret.is_empty() {
            s.oidc.client_secret = SECRET_MASK.into();
        }
        Ok(s)
    }

    pub async fn set_security_settings(&self, mut s: SecuritySettingsDto) -> Result<SecuritySettingsDto> {
        let current = self.security_settings().await?;
        if s.oidc.client_secret == SECRET_MASK {
            s.oidc.client_secret = current.oidc.client_secret.clone();
        }
        s.oidc.issuer_url = s.oidc.issuer_url.trim().trim_end_matches('/').to_owned();
        s.oidc.scopes.retain(|x| !x.trim().is_empty() && x != "openid");
        if s.oidc.enabled {
            if url::Url::parse(&s.oidc.issuer_url).is_err() {
                return Err(Error::Validation("OIDC issuer URL is not a valid URL".into()));
            }
            if s.oidc.client_id.trim().is_empty() {
                return Err(Error::Validation("OIDC client id is required".into()));
            }
            if !["preferred_username", "email", "sub", "name", "nickname"]
                .contains(&s.oidc.username_claim.as_str())
            {
                return Err(Error::Validation(
                    "username claim must be preferred_username, email, sub, name or nickname".into(),
                ));
            }
        }
        if let Some(u) = &s.oidc.redirect_url
            && !u.trim().is_empty()
            && url::Url::parse(u).is_err()
        {
            return Err(Error::Validation("OIDC redirect URL is not a valid URL".into()));
        }
        let mut stored = s.clone();
        if !stored.oidc.client_secret.is_empty() {
            stored.oidc.client_secret = self.secrets.encrypt(&stored.oidc.client_secret)?;
        }
        self.db.settings().set(SECURITY_SETTINGS_KEY, &stored).await?;
        self.changed("settings", "updated", 0, None);
        if !s.oidc.client_secret.is_empty() {
            s.oidc.client_secret = SECRET_MASK.into();
        }
        Ok(s)
    }
}
