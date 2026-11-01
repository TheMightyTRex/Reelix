//! OpenID Connect login: authorization code flow with PKCE and nonce (`openidconnect` crate).
//!
//! `GET /auth/oidc/login` → [`RequestService::oidc_begin`] returns the provider URL and keeps
//! the PKCE verifier + nonce in memory keyed by the CSRF `state` (10 minute TTL).
//! `GET /auth/oidc/callback` → [`RequestService::oidc_complete`] exchanges the code, verifies
//! the ID token and finds/creates the user.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreJwsSigningAlgorithm, CoreProviderMetadata,
};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet, EndpointSet,
    IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
};
use reelix_api_types::requests::OidcSettingsDto;
use reelix_core::{AuthSource, User};
use reelix_mediaserver::ExternalUser;

use crate::{Error, RequestService, Result};

const PENDING_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 1000;

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

struct Pending {
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    redirect_uri: String,
    return_to: String,
    created: Instant,
}

/// In-memory OIDC state.
pub struct OidcState {
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
}

impl OidcState {
    pub(crate) fn new(http: Option<reqwest::Client>) -> Self {
        // Following redirects from the identity provider opens up SSRF; disable them.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("Reelix/", env!("CARGO_PKG_VERSION")))
            .build()
            .or_else(|_| http.ok_or(()))
            .unwrap_or_default();
        Self {
            http,
            pending: Mutex::new(HashMap::new()),
        }
    }
}

/// Only allows relative in-app paths as post-login targets (no open redirects).
pub fn safe_return_to(r: Option<&str>) -> String {
    match r {
        // Printable ASCII only: browsers drop tabs/newlines, so `/\t/evil.com` would
        // otherwise become `//evil.com`.
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && !p.contains("://")
                && p.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            p.to_owned()
        }
        _ => "/".to_owned(),
    }
}

impl RequestService {
    async fn oidc_config(&self) -> Result<OidcSettingsDto> {
        let s = self.security_settings().await?.oidc;
        if !s.enabled || s.issuer_url.is_empty() || s.client_id.is_empty() {
            return Err(Error::Forbidden("single sign-on is not enabled".into()));
        }
        Ok(s)
    }

    async fn oidc_client(&self, cfg: &OidcSettingsDto, redirect_uri: &str) -> Result<Client> {
        let issuer = IssuerUrl::new(cfg.issuer_url.clone())
            .map_err(|e| Error::Validation(format!("issuer URL: {e}")))?;
        let meta = CoreProviderMetadata::discover_async(issuer, &self.oidc.http)
            .await
            .map_err(|e| Error::Upstream(format!("OIDC discovery failed: {e}")))?;
        let secret = (!cfg.client_secret.is_empty()).then(|| ClientSecret::new(cfg.client_secret.clone()));
        let redirect = RedirectUrl::new(redirect_uri.to_owned())
            .map_err(|e| Error::Validation(format!("redirect URL: {e}")))?;
        Ok(
            CoreClient::from_provider_metadata(meta, ClientId::new(cfg.client_id.clone()), secret)
                .set_redirect_uri(redirect),
        )
    }

    /// The externally configured callback URL, if any (else the API derives one).
    pub async fn oidc_redirect_override(&self) -> Result<Option<String>> {
        Ok(self
            .security_settings()
            .await?
            .oidc
            .redirect_url
            .filter(|u| !u.trim().is_empty()))
    }

    /// Starts a login: returns the identity provider's authorization URL.
    pub async fn oidc_begin(&self, redirect_uri: &str, return_to: Option<&str>) -> Result<String> {
        let cfg = self.oidc_config().await?;
        let client = self.oidc_client(&cfg, redirect_uri).await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut req = client.authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        );
        for s in &cfg.scopes {
            req = req.add_scope(Scope::new(s.clone()));
        }
        let (url, state, nonce) = req.set_pkce_challenge(challenge).url();
        let mut pending = self.oidc.pending.lock().expect("oidc lock");
        pending.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        if pending.len() >= MAX_PENDING {
            return Err(Error::Conflict(
                "too many pending sign-ins; try again shortly".into(),
            ));
        }
        pending.insert(
            state.secret().clone(),
            Pending {
                nonce,
                verifier,
                redirect_uri: redirect_uri.to_owned(),
                return_to: safe_return_to(return_to),
                created: Instant::now(),
            },
        );
        Ok(url.to_string())
    }

    /// Completes a login. Returns the user and the in-app path to go to.
    pub async fn oidc_complete(&self, code: &str, state: &str) -> Result<(User, String)> {
        let p = self
            .oidc
            .pending
            .lock()
            .expect("oidc lock")
            .remove(state)
            .filter(|p| p.created.elapsed() < PENDING_TTL)
            .ok_or_else(|| {
                Error::Unauthorized("sign-in expired or invalid state; please try again".into())
            })?;
        let cfg = self.oidc_config().await?;
        let client = self.oidc_client(&cfg, &p.redirect_uri).await?;
        let token = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .map_err(|e| Error::Upstream(format!("OIDC provider has no token endpoint: {e}")))?
            .set_pkce_verifier(p.verifier)
            .request_async(&self.oidc.http)
            .await
            .map_err(|e| Error::Unauthorized(format!("OIDC code exchange failed: {e}")))?;
        let id_token = token
            .extra_fields()
            .id_token()
            .ok_or_else(|| Error::Unauthorized("identity provider returned no ID token".into()))?;
        use CoreJwsSigningAlgorithm as A;
        let verifier = client.id_token_verifier().set_allowed_algs([
            A::RsaSsaPkcs1V15Sha256,
            A::RsaSsaPkcs1V15Sha384,
            A::RsaSsaPkcs1V15Sha512,
            A::RsaSsaPssSha256,
            A::RsaSsaPssSha384,
            A::RsaSsaPssSha512,
            A::EcdsaP256Sha256,
            A::EcdsaP384Sha384,
            A::EdDsa,
            A::HmacSha256,
        ]);
        let claims = id_token
            .claims(&verifier, &p.nonce)
            .map_err(|e| Error::Unauthorized(format!("invalid ID token: {e}")))?;
        let sub = claims.subject().as_str().to_owned();
        let email = claims.email().map(|e| e.as_str().to_owned());
        let name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.as_str().to_owned());
        let preferred = claims.preferred_username().map(|u| u.as_str().to_owned());
        let nickname = claims
            .nickname()
            .and_then(|n| n.get(None))
            .map(|n| n.as_str().to_owned());
        let username = match cfg.username_claim.as_str() {
            "email" => email.clone(),
            "sub" => Some(sub.clone()),
            "name" => name.clone(),
            "nickname" => nickname.clone(),
            _ => preferred.clone(),
        }
        .or(preferred)
        .or(email.clone())
        .unwrap_or_else(|| sub.clone());
        let ext = ExternalUser {
            id: sub,
            username,
            email,
            is_admin: false,
            avatar_url: claims
                .picture()
                .and_then(|p| p.get(None))
                .map(|u| u.as_str().to_owned()),
            access_token: None,
        };
        let user = self
            .find_or_create(AuthSource::Oidc, &ext, cfg.auto_create)
            .await?;
        Ok((user, p.return_to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_is_relative_only() {
        assert_eq!(safe_return_to(Some("/discover")), "/discover");
        assert_eq!(safe_return_to(Some("//evil.com")), "/");
        assert_eq!(safe_return_to(Some("https://evil.com")), "/");
        assert_eq!(safe_return_to(Some("/\\evil")), "/");
        assert_eq!(safe_return_to(Some("/\t/evil.com")), "/");
        assert_eq!(safe_return_to(Some("/caf\u{e9}")), "/");
        assert_eq!(safe_return_to(None), "/");
    }
}
