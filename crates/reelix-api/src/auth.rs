//! Authentication: passwords, session tokens, API keys, the `CurrentUser` extractor and
//! the login rate limiter.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{ConnectInfo, FromRequestParts, Query};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum_extra::extract::CookieJar;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rand::rngs::OsRng;
use reelix_api_types::{API_KEY_HEADER, CSRF_HEADER, SESSION_COOKIE, UserDto};
use reelix_core::{Permissions, User};
use sha2::{Digest, Sha256};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Server-side lifetime of a "remember me" session (and its cookie Max-Age).
pub const REMEMBER_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Server-side lifetime of a browser-session cookie.
pub const SESSION_TTL: Duration = Duration::from_secs(24 * 3600);
/// Prefix of every API key.
pub const API_KEY_PREFIX: &str = "rlx_";

// ---------- Passwords ----------

/// Hashes a password with argon2id (crate defaults). CPU-heavy: call via `spawn_blocking`.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(ApiError::internal)
}

/// Verifies `password` against a PHC hash; `None` verifies against a dummy hash so that
/// unknown users cost the same time as known ones. Always returns false for `None`.
pub fn verify_password(password: &str, hash: Option<&str>) -> bool {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let (hash, real) = match hash {
        Some(h) => (h, true),
        None => (
            DUMMY
                .get_or_init(|| {
                    hash_password("reelix-dummy-password-for-timing").expect("hashing a constant cannot fail")
                })
                .as_str(),
            false,
        ),
    };
    let Ok(parsed) = PasswordHash::new(hash) else {
        // A corrupt stored hash: still do comparable work.
        let _ = verify_password(password, None);
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
        && real
}

pub(crate) async fn hash_password_blocking(password: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(ApiError::internal)?
}

pub(crate) async fn verify_password_blocking(password: String, hash: Option<String>) -> ApiResult<bool> {
    tokio::task::spawn_blocking(move || verify_password(&password, hash.as_deref()))
        .await
        .map_err(ApiError::internal)
}

// ---------- Tokens ----------

/// 32 random bytes, base64url without padding.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Hex SHA-256, used to store session tokens and API keys.
pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Generates a new API key, returning `(key, prefix)`.
pub fn generate_api_key() -> (String, String) {
    let body = random_token();
    let prefix = body.chars().take(8).collect();
    (format!("{API_KEY_PREFIX}{body}"), prefix)
}

// ---------- Cookies ----------

fn cookie_path(state: &AppState) -> &str {
    let base = state.config().url_base.as_str();
    if base.is_empty() { "/" } else { base }
}

fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .next()
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
        })
}

/// `Set-Cookie` value for a fresh session. `max_age = None` makes it a browser-session cookie.
pub(crate) fn session_cookie(
    state: &AppState,
    headers: &HeaderMap,
    token: &str,
    max_age: Option<Duration>,
) -> HeaderValue {
    let mut c = format!(
        "{SESSION_COOKIE}={token}; Path={}; HttpOnly; SameSite=Lax",
        cookie_path(state)
    );
    if is_https(headers) {
        c.push_str("; Secure");
    }
    if let Some(age) = max_age {
        c.push_str(&format!("; Max-Age={}", age.as_secs()));
    }
    HeaderValue::from_str(&c).expect("cookie is ASCII")
}

/// `Set-Cookie` value that removes the session cookie.
pub(crate) fn clear_cookie(state: &AppState, headers: &HeaderMap) -> HeaderValue {
    let mut c = format!(
        "{SESSION_COOKIE}=; Path={}; HttpOnly; SameSite=Lax; Max-Age=0",
        cookie_path(state)
    );
    if is_https(headers) {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).expect("cookie is ASCII")
}

/// Creates a session row and returns the `Set-Cookie` header value.
pub(crate) async fn start_session(
    state: &AppState,
    parts: &RequestMeta,
    user_id: i64,
    remember_me: bool,
) -> ApiResult<HeaderValue> {
    let token = random_token();
    let ttl = if remember_me { REMEMBER_TTL } else { SESSION_TTL };
    let expires = reelix_db::now_ms() + ttl.as_millis() as i64;
    state
        .db()
        .sessions()
        .create(
            &sha256_hex(&token),
            user_id,
            expires,
            parts.user_agent.as_deref(),
            parts.ip.map(|i| i.to_string()).as_deref(),
        )
        .await?;
    Ok(session_cookie(
        state,
        &parts.headers,
        &token,
        remember_me.then_some(REMEMBER_TTL),
    ))
}

pub(crate) fn session_token(headers: &HeaderMap) -> Option<String> {
    CookieJar::from_headers(headers)
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_owned())
        .filter(|v| !v.is_empty())
}

pub(crate) fn has_csrf_header(headers: &HeaderMap) -> bool {
    headers.contains_key(CSRF_HEADER)
}

/// Request metadata needed by login/setup handlers.
pub struct RequestMeta {
    pub headers: HeaderMap,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for RequestMeta {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            ip: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip()),
            user_agent: parts
                .headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(512).collect()),
            headers: parts.headers.clone(),
        })
    }
}

// ---------- CurrentUser ----------

/// How the current request was authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Session,
    ApiKey,
}

/// The authenticated user. Rejects with 401 when no valid credentials are present, and
/// with 403 `csrf` for cookie-authenticated state-changing requests lacking the CSRF header.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub user: User,
    pub method: AuthMethod,
}

impl CurrentUser {
    /// Fails with 403 unless the user has `required` (admins pass every check).
    pub fn require(&self, required: Permissions) -> ApiResult<()> {
        if self.user.permissions.has(required) {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }

    pub fn is_admin(&self) -> bool {
        self.user.permissions.is_admin()
    }
}

#[derive(serde::Deserialize)]
struct ApiKeyQuery {
    apikey: Option<String>,
}

fn is_safe_method(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Resolves credentials. `Ok(None)` = no or invalid credentials.
async fn authenticate(parts: &Parts, state: &AppState) -> ApiResult<Option<CurrentUser>> {
    let header_key = parts
        .headers
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let api_key = match header_key {
        Some(k) => Some(k),
        None => Query::<ApiKeyQuery>::try_from_uri(&parts.uri)
            .ok()
            .and_then(|q| q.0.apikey),
    };
    if let Some(key) = api_key {
        let user = state.db().api_keys().find_user(&sha256_hex(key.trim())).await?;
        return Ok(user.map(|user| CurrentUser {
            user,
            method: AuthMethod::ApiKey,
        }));
    }

    let Some(token) = session_token(&parts.headers) else {
        return Ok(None);
    };
    let Some(user) = state
        .db()
        .sessions()
        .find_user(&sha256_hex(&token), reelix_db::now_ms())
        .await?
    else {
        return Ok(None);
    };
    if !is_safe_method(&parts.method) && !has_csrf_header(&parts.headers) {
        return Err(ApiError::Csrf);
    }
    Ok(Some(CurrentUser {
        user,
        method: AuthMethod::Session,
    }))
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(u) = parts.extensions.get::<CurrentUser>() {
            return Ok(u.clone());
        }
        let user = authenticate(parts, state).await?.ok_or(ApiError::Unauthorized)?;
        parts.extensions.insert(user.clone());
        Ok(user)
    }
}

/// Like [`CurrentUser`] but never rejects for missing/invalid credentials.
#[derive(Debug, Clone)]
pub struct MaybeUser(pub Option<CurrentUser>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        match CurrentUser::from_request_parts(parts, state).await {
            Ok(u) => Ok(Self(Some(u))),
            Err(ApiError::Unauthorized) => Ok(Self(None)),
            Err(e) => Err(e),
        }
    }
}

pub(crate) fn user_dto(u: &User) -> UserDto {
    UserDto {
        id: u.id,
        username: u.username.clone(),
        display_name: u.display_name().to_owned(),
        email: u.email.clone(),
        auth_source: u.auth_source.as_str().to_owned(),
        permissions: u.permissions,
        created_at: u.created_at,
    }
}

// ---------- Login rate limiter ----------

const FREE_ATTEMPTS: u32 = 5;
const BASE_LOCK: Duration = Duration::from_secs(30);
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug)]
struct Attempts {
    failures: u32,
    locked_until: Option<Instant>,
    last_failure: Instant,
}

/// In-memory per-`ip|username` failed-login tracker: after 5 failures the key is locked
/// for 30s, doubling with each further failure, capped at 15 minutes.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    entries: Mutex<HashMap<String, Attempts>>,
}

impl LoginLimiter {
    pub fn key(ip: Option<IpAddr>, username: &str) -> String {
        let ip = ip.map(|i| i.to_string()).unwrap_or_else(|| "unknown".into());
        format!("{ip}|{}", username.trim().to_lowercase())
    }

    /// `Err(RateLimited)` while `key` is locked out.
    pub fn check(&self, key: &str) -> ApiResult<()> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: &str, now: Instant) -> ApiResult<()> {
        let entries = self.entries.lock().expect("limiter lock poisoned");
        if let Some(until) = entries.get(key).and_then(|a| a.locked_until)
            && until > now
        {
            let secs = (until - now).as_secs_f64().ceil() as u64;
            return Err(ApiError::RateLimited {
                retry_after_secs: secs.max(1),
            });
        }
        Ok(())
    }

    pub fn record_failure(&self, key: &str) {
        self.record_failure_at(key, Instant::now());
    }

    fn record_failure_at(&self, key: &str, now: Instant) {
        let mut entries = self.entries.lock().expect("limiter lock poisoned");
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, a| {
                a.locked_until.is_some_and(|u| u > now) || now.duration_since(a.last_failure) < MAX_LOCK
            });
        }
        let a = entries.entry(key.to_owned()).or_insert(Attempts {
            failures: 0,
            locked_until: None,
            last_failure: now,
        });
        // Forget old failures after a quiet period.
        if a.locked_until.is_none_or(|u| u <= now) && now.duration_since(a.last_failure) > MAX_LOCK * 2 {
            a.failures = 0;
        }
        a.failures += 1;
        a.last_failure = now;
        if a.failures >= FREE_ATTEMPTS {
            let exp = (a.failures - FREE_ATTEMPTS).min(10);
            a.locked_until = Some(now + (BASE_LOCK * 2u32.pow(exp)).min(MAX_LOCK));
        }
    }

    pub fn record_success(&self, key: &str) {
        self.entries.lock().expect("limiter lock poisoned").remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_locks_after_five_failures_and_doubles() {
        let l = LoginLimiter::default();
        let t0 = Instant::now();
        for _ in 0..4 {
            l.record_failure_at("k", t0);
            assert!(l.check_at("k", t0).is_ok());
        }
        l.record_failure_at("k", t0);
        match l.check_at("k", t0) {
            Err(ApiError::RateLimited { retry_after_secs }) => assert_eq!(retry_after_secs, 30),
            other => panic!("expected rate limit, got {other:?}"),
        }
        let t1 = t0 + Duration::from_secs(31);
        assert!(l.check_at("k", t1).is_ok());
        l.record_failure_at("k", t1);
        match l.check_at("k", t1) {
            Err(ApiError::RateLimited { retry_after_secs }) => assert_eq!(retry_after_secs, 60),
            other => panic!("expected rate limit, got {other:?}"),
        }
        // Cap at 15 minutes.
        let mut t = t1;
        for _ in 0..10 {
            t += MAX_LOCK + Duration::from_secs(1);
            l.record_failure_at("k", t);
        }
        match l.check_at("k", t) {
            Err(ApiError::RateLimited { retry_after_secs }) => assert_eq!(retry_after_secs, 900),
            other => panic!("expected rate limit, got {other:?}"),
        }
        l.record_success("k");
        assert!(l.check_at("k", t).is_ok());
    }

    #[test]
    fn password_roundtrip_and_dummy() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", Some(&h)));
        assert!(!verify_password("wrong", Some(&h)));
        assert!(!verify_password("correct horse", None));
        assert!(!verify_password("x", Some("not a hash")));
    }

    #[test]
    fn api_key_shape() {
        let (key, prefix) = generate_api_key();
        assert!(key.starts_with("rlx_"));
        assert_eq!(key.len(), 4 + 43);
        assert_eq!(&key[4..12], prefix);
    }
}
