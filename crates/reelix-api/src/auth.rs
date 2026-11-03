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

/// At most this many argon2 hashes run at once (each uses ~19 MiB); a flood of logins queues
/// instead of exhausting memory.
const MAX_CONCURRENT_HASHES: usize = 4;

fn hash_permits() -> &'static tokio::sync::Semaphore {
    static PERMITS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    PERMITS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_HASHES))
}

pub(crate) async fn hash_password_blocking(password: String) -> ApiResult<String> {
    let _permit = hash_permits().acquire().await.map_err(ApiError::internal)?;
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(ApiError::internal)?
}

/// Logins allowed to wait for a hash permit; beyond that they're turned away with 429 instead
/// of piling up behind a flood.
const MAX_QUEUED_VERIFIES: usize = 32;

fn verify_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_HASHES + MAX_QUEUED_VERIFIES))
}

pub(crate) async fn verify_password_blocking(password: String, hash: Option<String>) -> ApiResult<bool> {
    let _slot = verify_slots()
        .try_acquire()
        .map_err(|_| ApiError::RateLimited { retry_after_secs: 1 })?;
    let _permit = hash_permits().acquire().await.map_err(ApiError::internal)?;
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

/// When session and login-flow cookies get the `Secure` attribute (`security.secure_cookies`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecureCookies {
    /// Only when a proxy reports HTTPS via `X-Forwarded-Proto`.
    #[default]
    Auto,
    Always,
    Never,
}

/// Security options from the server config.
#[derive(Debug, Clone, Default)]
pub struct SecurityConfig {
    /// Peers whose `X-Forwarded-For` is believed (`security.trusted_proxies`).
    pub trusted_proxies: Vec<IpNet>,
    pub secure_cookies: SecureCookies,
}

fn is_https(state: &AppState, headers: &HeaderMap) -> bool {
    match state.security().secure_cookies {
        SecureCookies::Always => true,
        SecureCookies::Never => false,
        SecureCookies::Auto => forwarded_https(headers),
    }
}

fn forwarded_https(headers: &HeaderMap) -> bool {
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
    if is_https(state, headers) {
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
    if is_https(state, headers) {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).expect("cookie is ASCII")
}

/// How long a pending external login (Plex PIN, OIDC redirect) stays bound to the browser.
const FLOW_TTL: Duration = Duration::from_secs(15 * 60);

/// `Set-Cookie` binding a pending external login to this browser: `value` is sealed with the
/// server secret, so the cookie can't be forged, and the flow can only be finished by the
/// browser that started it (a PIN id or OIDC state seen elsewhere is useless).
pub(crate) fn flow_cookie(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    value: &str,
) -> ApiResult<HeaderValue> {
    let sealed = state
        .secrets()
        .encrypt(&format!("login-flow\n{name}\n{value}"))
        .map_err(ApiError::internal)?;
    let mut c = format!(
        "{name}={sealed}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}",
        cookie_path(state),
        FLOW_TTL.as_secs()
    );
    if is_https(state, headers) {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).map_err(ApiError::internal)
}

/// Whether the request carries the [`flow_cookie`] `name` for exactly `value`.
pub(crate) fn flow_matches(state: &AppState, headers: &HeaderMap, name: &str, value: &str) -> bool {
    CookieJar::from_headers(headers)
        .get(name)
        .and_then(|c| state.secrets().decrypt(c.value()).ok())
        .is_some_and(|plain| plain == format!("login-flow\n{name}\n{value}"))
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

// ---------- Client address ----------

/// An IP network in CIDR notation (`10.0.0.0/8`, `::1/128`); a bare address is a single host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    addr: IpAddr,
    prefix: u8,
}

impl std::str::FromStr for IpNet {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid IP network {s:?}");
        let (addr, prefix) = match s.trim().split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s.trim(), None),
        };
        let addr: IpAddr = addr.parse().map_err(|_| invalid())?;
        let addr = addr.to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(invalid)?,
            None => max,
        };
        Ok(Self { addr, prefix })
    }
}

impl IpNet {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(self.prefix)).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

fn is_trusted(trusted: &[IpNet], ip: IpAddr) -> bool {
    trusted.iter().any(|n| n.contains(ip))
}

/// The client's address: the TCP peer, or, when the peer is a trusted proxy, the right-most
/// `X-Forwarded-For` hop that isn't one (entries further left are client-controlled).
pub fn client_ip(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    let mut ip = peer?.to_canonical();
    if !is_trusted(trusted, ip) {
        return Some(ip);
    }
    let hops: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect();
    for hop in hops.into_iter().rev() {
        let Ok(hop) = hop.trim().parse::<IpAddr>() else {
            break;
        };
        ip = hop.to_canonical();
        if !is_trusted(trusted, ip) {
            break;
        }
    }
    Some(ip)
}

/// Request metadata needed by login/setup handlers.
pub struct RequestMeta {
    pub headers: HeaderMap,
    /// Client address (see [`client_ip`]).
    pub ip: Option<IpAddr>,
    /// TCP peer address.
    pub peer: Option<IpAddr>,
    peer_trusted: bool,
    pub user_agent: Option<String>,
}

impl RequestMeta {
    /// Whether the request comes from the server machine itself. A reverse proxy on the same
    /// host makes every request arrive from loopback, so an untrusted peer that forwards
    /// anything doesn't count, and a trusted one must forward a loopback client.
    pub fn is_local(&self) -> bool {
        let forwarded = ["x-forwarded-for", "forwarded", "x-real-ip"]
            .iter()
            .any(|h| self.headers.contains_key(*h));
        if self.peer_trusted {
            self.headers.contains_key("x-forwarded-for") && self.ip.is_some_and(|ip| ip.is_loopback())
        } else {
            !forwarded && self.peer.is_some_and(|ip| ip.is_loopback())
        }
    }
}

impl FromRequestParts<AppState> for RequestMeta {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_canonical());
        let trusted = &state.security().trusted_proxies;
        Ok(Self {
            ip: client_ip(peer, &parts.headers, trusted),
            peer,
            peer_trusted: peer.is_some_and(|p| is_trusted(trusted, p)),
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

/// The credential a request carries, as stored (SHA-256 of the API key or session token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Credential {
    ApiKey(String),
    Session(String),
}

impl Credential {
    /// An API key (header, then `?apikey=`) wins over the session cookie.
    pub(crate) fn from_parts(parts: &Parts) -> Option<Self> {
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
        match api_key {
            Some(key) => Some(Self::ApiKey(sha256_hex(key.trim()))),
            None => session_token(&parts.headers).map(|t| Self::Session(sha256_hex(&t))),
        }
    }

    /// The user the credential still belongs to (`None` once revoked or expired).
    pub(crate) async fn user(&self, state: &AppState) -> ApiResult<Option<User>> {
        Ok(match self {
            Self::ApiKey(hash) => state.db().api_keys().find_user(hash).await?,
            Self::Session(hash) => state.db().sessions().find_user(hash, reelix_db::now_ms()).await?,
        })
    }
}

/// Resolves credentials. `Ok(None)` = no or invalid credentials.
async fn authenticate(parts: &Parts, state: &AppState) -> ApiResult<Option<CurrentUser>> {
    let Some(cred) = Credential::from_parts(parts) else {
        return Ok(None);
    };
    let Some(user) = cred.user(state).await? else {
        return Ok(None);
    };
    let method = match cred {
        Credential::ApiKey(_) => AuthMethod::ApiKey,
        Credential::Session(_) => AuthMethod::Session,
    };
    if method == AuthMethod::Session && !is_safe_method(&parts.method) && !has_csrf_header(&parts.headers) {
        return Err(ApiError::Csrf);
    }
    Ok(Some(CurrentUser { user, method }))
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
/// Failures one address may make across all usernames before it is locked (password spraying).
const IP_FREE_ATTEMPTS: u32 = 20;
const BASE_LOCK: Duration = Duration::from_secs(30);
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug)]
struct Attempts {
    failures: u32,
    /// Attempts reserved but not decided yet. They count against the free attempts, so a burst
    /// of parallel guesses can't all pass the check before the first one fails.
    in_flight: u32,
    locked_until: Option<Instant>,
    last_failure: Instant,
}

impl Attempts {
    fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            in_flight: 0,
            locked_until: None,
            last_failure: now,
        }
    }

    /// `Some(retry_after_secs)` while no further attempt may start.
    fn blocked(&self, free: u32, now: Instant) -> Option<u64> {
        if let Some(until) = self.locked_until
            && until > now
        {
            return Some(((until - now).as_secs_f64().ceil() as u64).max(1));
        }
        // Once a lock has expired, one attempt at a time may try again.
        let allowed = free.saturating_sub(self.failures).max(1);
        (self.in_flight >= allowed).then_some(1)
    }

    fn fail(&mut self, free: u32, now: Instant) {
        // Forget old failures after a quiet period.
        if self.locked_until.is_none_or(|u| u <= now) && now.duration_since(self.last_failure) > MAX_LOCK * 2
        {
            self.failures = 0;
        }
        self.failures += 1;
        self.last_failure = now;
        if self.failures >= free {
            let exp = (self.failures - free).min(10);
            self.locked_until = Some(now + (BASE_LOCK * 2u32.pow(exp)).min(MAX_LOCK));
        }
    }
}

/// In-memory failed-login tracker keyed by `ip|username` and by IP alone: after 5 failures for
/// one username (or 20 from one address, against password spraying) the key is locked for 30s,
/// doubling with each further failure, capped at 15 minutes.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    entries: Mutex<HashMap<String, Attempts>>,
}

/// A reserved login attempt from [`LoginLimiter::begin`]. Report the outcome with
/// [`failed`](Self::failed) or [`succeeded`](Self::succeeded); dropping it undecided (an
/// error before the verdict) only releases the reservation.
#[must_use]
pub struct LoginAttempt<'a> {
    limiter: &'a LoginLimiter,
    keys: [String; 2],
    done: bool,
}

impl LoginAttempt<'_> {
    pub fn failed(mut self) {
        self.limiter.finish(&self.keys, Some(false), Instant::now());
        self.done = true;
    }

    pub fn succeeded(mut self) {
        self.limiter.finish(&self.keys, Some(true), Instant::now());
        self.done = true;
    }
}

impl Drop for LoginAttempt<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.limiter.finish(&self.keys, None, Instant::now());
        }
    }
}

fn free_attempts(key: &str) -> u32 {
    if key.contains('|') {
        FREE_ATTEMPTS
    } else {
        IP_FREE_ATTEMPTS
    }
}

impl LoginLimiter {
    pub fn key(ip: Option<IpAddr>, username: &str) -> String {
        format!("{}|{}", Self::ip_key(ip), username.trim().to_lowercase())
    }

    fn ip_key(ip: Option<IpAddr>) -> String {
        ip.map(|i| i.to_string()).unwrap_or_else(|| "unknown".into())
    }

    /// Reserves an attempt for `username` from `ip` in one locked step, or `Err(RateLimited)`
    /// while the `ip|username` key or the address is locked out or has too many in flight.
    pub fn begin(&self, ip: Option<IpAddr>, username: &str) -> ApiResult<LoginAttempt<'_>> {
        self.begin_at(ip, username, Instant::now())
    }

    fn begin_at(&self, ip: Option<IpAddr>, username: &str, now: Instant) -> ApiResult<LoginAttempt<'_>> {
        let keys = [Self::key(ip, username), Self::ip_key(ip)];
        let mut entries = self.entries.lock().expect("limiter lock poisoned");
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, a| {
                a.in_flight > 0
                    || a.locked_until.is_some_and(|u| u > now)
                    || now.duration_since(a.last_failure) < MAX_LOCK
            });
        }
        let retry = keys
            .iter()
            .filter_map(|k| entries.get(k).and_then(|a| a.blocked(free_attempts(k), now)))
            .max();
        if let Some(retry_after_secs) = retry {
            return Err(ApiError::RateLimited { retry_after_secs });
        }
        for k in &keys {
            entries
                .entry(k.clone())
                .or_insert_with(|| Attempts::new(now))
                .in_flight += 1;
        }
        Ok(LoginAttempt {
            limiter: self,
            keys,
            done: false,
        })
    }

    /// Releases a reservation; `Some(ok)` also records the outcome.
    fn finish(&self, keys: &[String; 2], outcome: Option<bool>, now: Instant) {
        let mut entries = self.entries.lock().expect("limiter lock poisoned");
        for k in keys {
            let Some(a) = entries.get_mut(k) else {
                continue;
            };
            a.in_flight = a.in_flight.saturating_sub(1);
            match outcome {
                Some(false) => a.fail(free_attempts(k), now),
                // A success clears the username's failures, not the address's.
                Some(true) if k.contains('|') => {
                    entries.remove(k);
                }
                _ if a.in_flight == 0 && a.failures == 0 => {
                    entries.remove(k);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail_at(l: &LoginLimiter, ip: Option<IpAddr>, user: &str, t: Instant) {
        let mut a = l.begin_at(ip, user, t).expect("not limited");
        l.finish(&a.keys, Some(false), t);
        a.done = true;
    }

    fn check_at(l: &LoginLimiter, user: &str, t: Instant) -> ApiResult<()> {
        l.begin_at(None, user, t).map(drop)
    }

    fn retry_after(r: ApiResult<()>) -> u64 {
        match r {
            Err(ApiError::RateLimited { retry_after_secs }) => retry_after_secs,
            other => panic!("expected rate limit, got {other:?}"),
        }
    }

    #[test]
    fn limiter_locks_after_five_failures_and_doubles() {
        let l = LoginLimiter::default();
        let t0 = Instant::now();
        for _ in 0..4 {
            fail_at(&l, None, "k", t0);
            assert!(check_at(&l, "k", t0).is_ok());
        }
        fail_at(&l, None, "k", t0);
        assert_eq!(retry_after(check_at(&l, "k", t0)), 30);
        let t1 = t0 + Duration::from_secs(31);
        assert!(check_at(&l, "k", t1).is_ok());
        fail_at(&l, None, "k", t1);
        assert_eq!(retry_after(check_at(&l, "k", t1)), 60);
        // Cap at 15 minutes.
        let mut t = t1;
        for _ in 0..10 {
            t += MAX_LOCK + Duration::from_secs(1);
            fail_at(&l, None, "k", t);
        }
        assert_eq!(retry_after(check_at(&l, "k", t)), 900);
        l.begin_at(None, "other", t).unwrap().succeeded();
        assert_eq!(retry_after(check_at(&l, "k", t)), 900);
    }

    #[test]
    fn limiter_counts_in_flight_attempts() {
        let l = LoginLimiter::default();
        let t = Instant::now();
        // Five parallel guesses use up the free attempts before any of them is decided.
        let pending: Vec<_> = (0..5).map(|_| l.begin_at(None, "k", t).unwrap()).collect();
        assert_eq!(retry_after(check_at(&l, "k", t)), 1);
        drop(pending);
        assert!(check_at(&l, "k", t).is_ok());
        // A success clears the username's failures.
        fail_at(&l, None, "k", t);
        l.begin_at(None, "k", t).unwrap().succeeded();
        for _ in 0..4 {
            fail_at(&l, None, "k", t);
        }
        assert!(check_at(&l, "k", t).is_ok());
    }

    #[test]
    fn limiter_locks_an_address_spraying_usernames() {
        let l = LoginLimiter::default();
        let t = Instant::now();
        let ip: Option<IpAddr> = Some("203.0.113.9".parse().unwrap());
        for i in 0..IP_FREE_ATTEMPTS {
            fail_at(&l, ip, &format!("user{i}"), t);
        }
        assert!(l.begin_at(ip, "fresh", t).is_err());
        assert!(
            l.begin_at(Some("203.0.113.10".parse().unwrap()), "fresh", t)
                .is_ok()
        );
    }

    #[test]
    fn client_ip_only_believes_trusted_proxies() {
        let nets: Vec<IpNet> = ["10.0.0.0/8", "::1"].iter().map(|n| n.parse().unwrap()).collect();
        assert!("10.0.0.0/33".parse::<IpNet>().is_err());
        assert!(nets[0].contains("10.1.2.3".parse().unwrap()));
        assert!(nets[0].contains("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!nets[0].contains("11.0.0.1".parse().unwrap()));
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.1.1.1, 192.0.2.7, 10.0.0.2".parse().unwrap());
        let ip = |peer: &str| {
            client_ip(Some(peer.parse().unwrap()), &h, &nets)
                .unwrap()
                .to_string()
        };
        // Untrusted peer: the header is ignored.
        assert_eq!(ip("198.51.100.1"), "198.51.100.1");
        // Trusted peer: the right-most hop that isn't a trusted proxy; the spoofed 1.1.1.1 is not.
        assert_eq!(ip("10.0.0.1"), "192.0.2.7");
        assert_eq!(ip("::1"), "192.0.2.7");
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
