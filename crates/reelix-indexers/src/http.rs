//! HTTP plumbing shared by the adapters: per-indexer clients with proxies, the SSRF
//! guard, a tiny cookie jar and the FlareSolverr client.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::redirect;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::IndexerError;

/// Largest search/caps/page response read from an indexer.
pub const MAX_PAGE_BYTES: usize = 32 * 1024 * 1024;
/// Largest `.torrent` / `.nzb` download.
pub const MAX_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Reads a response body, failing once it exceeds `limit` bytes (a hostile or broken
/// indexer can't make Reelix buffer gigabytes).
pub async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, IndexerError> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(IndexerError::Parse(format!(
            "response larger than {} MB",
            limit / (1024 * 1024)
        )));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > limit {
            return Err(IndexerError::Parse(format!(
                "response larger than {} MB",
                limit / (1024 * 1024)
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Proxy kind supported per indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    Http,
    /// SOCKS5 with remote DNS resolution (`socks5h`).
    Socks5,
}

/// An HTTP or SOCKS5 proxy an indexer's traffic is routed through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Proxy URL, e.g. `socks5h://proxy:1080`.
    pub fn url(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5h",
        };
        format!("{scheme}://{}:{}", self.host, self.port)
    }

    fn to_reqwest(&self) -> Result<reqwest::Proxy, IndexerError> {
        let mut p =
            reqwest::Proxy::all(self.url()).map_err(|e| IndexerError::Config(format!("proxy: {e}")))?;
        if let Some(u) = self.username.as_deref().filter(|u| !u.is_empty()) {
            p = p.basic_auth(u, self.password.as_deref().unwrap_or(""));
        }
        Ok(p)
    }
}

/// Rejects loopback/private/link-local targets unless explicitly allowed.
#[derive(Debug, Clone, Copy)]
pub struct AddressGuard {
    pub allow_private: bool,
}

/// True for addresses that must not be reachable from user-supplied URLs by default.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private(IpAddr::V4(v4));
            }
            let s = v6.segments();
            v6.is_loopback() || v6.is_unspecified() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

impl AddressGuard {
    /// Scheme + literal-host check (no DNS); used at build time and on redirects.
    pub fn check_literal(&self, url: &Url) -> Result<(), IndexerError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(IndexerError::Blocked(format!(
                "unsupported scheme `{}`",
                url.scheme()
            )));
        }
        if self.allow_private {
            return Ok(());
        }
        match url.host() {
            Some(url::Host::Ipv4(ip)) if is_private(ip.into()) => Err(IndexerError::Blocked(ip.to_string())),
            Some(url::Host::Ipv6(ip)) if is_private(ip.into()) => Err(IndexerError::Blocked(ip.to_string())),
            Some(url::Host::Domain(d))
                if d.eq_ignore_ascii_case("localhost") || d.ends_with(".localhost") =>
            {
                Err(IndexerError::Blocked(d.to_string()))
            }
            None => Err(IndexerError::Blocked("missing host".into())),
            _ => Ok(()),
        }
    }

    /// Full check including DNS resolution of host names.
    pub async fn check(&self, url: &Url) -> Result<(), IndexerError> {
        self.check_literal(url)?;
        if self.allow_private {
            return Ok(());
        }
        if let Some(url::Host::Domain(d)) = url.host() {
            let port = url.port_or_known_default().unwrap_or(80);
            let addrs = tokio::net::lookup_host((d, port))
                .await
                .map_err(|e| IndexerError::Blocked(format!("cannot resolve {d}: {e}")))?;
            for a in addrs {
                if is_private(a.ip()) {
                    return Err(IndexerError::Blocked(format!("{d} resolves to {}", a.ip())));
                }
            }
        }
        Ok(())
    }
}

/// Network options for building an indexer's own HTTP client.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub proxy: Option<ProxyConfig>,
    pub timeout: Duration,
    pub user_agent: String,
    pub guard: AddressGuard,
}

/// Builds a per-indexer client. Redirects are followed (≤ 10 hops) except into non-HTTP
/// schemes (magnet links surface as the 3xx response) or blocked addresses.
pub(crate) fn build_client(
    opts: &HttpOptions,
    follow_redirects: bool,
) -> Result<reqwest::Client, IndexerError> {
    let guard = opts.guard;
    let policy = if follow_redirects {
        redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 || guard.check_literal(attempt.url()).is_err() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        })
    } else {
        redirect::Policy::none()
    };
    let mut b = reqwest::Client::builder()
        .timeout(opts.timeout)
        .user_agent(opts.user_agent.clone())
        .redirect(policy)
        .gzip(true);
    if let Some(p) = &opts.proxy {
        b = b.proxy(p.to_reqwest()?);
    }
    b.build()
        .map_err(|e| IndexerError::Config(format!("http client: {e}")))
}

/// Maps non-success statuses to typed errors (401/403 → auth, 429 → request limit).
pub(crate) fn status_error(resp: &reqwest::Response) -> Option<IndexerError> {
    let s = resp.status();
    if s.is_success() || s.is_redirection() {
        return None;
    }
    Some(match s.as_u16() {
        401 | 403 => IndexerError::Auth(format!("HTTP {s}")),
        429 => IndexerError::RequestLimit {
            retry_after: retry_after(resp),
        },
        code => IndexerError::Status { status: code },
    })
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let v = resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Minimal name → value cookie jar (enough for tracker sessions).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CookieJar(pub Vec<(String, String)>);

impl CookieJar {
    pub fn set(&mut self, name: &str, value: &str) {
        if let Some(c) = self.0.iter_mut().find(|c| c.0 == name) {
            c.1 = value.to_string();
        } else {
            self.0.push((name.to_string(), value.to_string()));
        }
    }

    /// Absorbs every `Set-Cookie` header of a response.
    pub fn absorb(&mut self, headers: &reqwest::header::HeaderMap) {
        for v in headers.get_all(reqwest::header::SET_COOKIE) {
            if let Some((n, val)) = v
                .to_str()
                .ok()
                .and_then(|s| s.split(';').next())
                .and_then(|kv| kv.split_once('='))
            {
                self.set(n.trim(), val.trim());
            }
        }
    }

    /// Parses a raw `a=1; b=2` cookie string (user-supplied cookie logins).
    pub fn parse_header(&mut self, raw: &str) {
        for kv in raw.split(';') {
            if let Some((n, v)) = kv.split_once('=') {
                self.set(n.trim(), v.trim());
            }
        }
    }

    pub fn header(&self) -> Option<String> {
        (!self.0.is_empty()).then(|| {
            self.0
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// True when a response looks like a Cloudflare browser challenge.
pub(crate) fn is_cloudflare_challenge(status: u16, headers: &reqwest::header::HeaderMap, body: &str) -> bool {
    let cf = headers
        .get(reqwest::header::SERVER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.to_ascii_lowercase().contains("cloudflare"));
    matches!(status, 403 | 503)
        && cf
        && (body.contains("Just a moment")
            || body.contains("cf-challenge")
            || body.contains("challenge-platform"))
}

/// Client for a FlareSolverr instance (`POST /v1`).
#[derive(Debug, Clone)]
pub struct FlareSolverr {
    http: reqwest::Client,
    endpoint: Url,
    max_timeout: Duration,
}

/// A solved page as returned by FlareSolverr.
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub url: String,
    pub status: u16,
    pub body: String,
    pub cookies: Vec<(String, String)>,
    pub user_agent: Option<String>,
}

#[derive(Deserialize)]
struct FsResponse {
    status: String,
    #[serde(default)]
    message: String,
    solution: Option<FsSolution>,
}

#[derive(Deserialize)]
struct FsSolution {
    url: String,
    #[serde(default)]
    status: u16,
    #[serde(default)]
    response: String,
    #[serde(default)]
    cookies: Vec<FsCookie>,
    #[serde(rename = "userAgent")]
    user_agent: Option<String>,
}

#[derive(Deserialize)]
struct FsCookie {
    name: String,
    value: String,
}

impl FlareSolverr {
    /// `base` is the FlareSolverr root, e.g. `http://flaresolverr:8191/`.
    pub fn new(http: reqwest::Client, base: &Url) -> Result<Self, IndexerError> {
        let endpoint = base
            .join("v1")
            .map_err(|e| IndexerError::Config(format!("flaresolverr url: {e}")))?;
        Ok(Self {
            http,
            endpoint,
            max_timeout: Duration::from_secs(60),
        })
    }

    pub fn with_max_timeout(mut self, d: Duration) -> Self {
        self.max_timeout = d;
        self
    }

    /// Solves a GET (or form POST when `post_data` is set) for `url`.
    pub async fn solve(
        &self,
        url: &str,
        post_data: Option<&str>,
        jar: &CookieJar,
    ) -> Result<Solution, IndexerError> {
        let cookies: Vec<_> = jar
            .0
            .iter()
            .map(|(n, v)| json!({"name": n, "value": v}))
            .collect();
        let mut body = json!({
            "cmd": if post_data.is_some() { "request.post" } else { "request.get" },
            "url": url,
            "maxTimeout": self.max_timeout.as_millis() as u64,
            "cookies": cookies,
        });
        if let Some(p) = post_data {
            body["postData"] = p.into();
        }
        let resp = self
            .http
            .post(self.endpoint.clone())
            .timeout(self.max_timeout + Duration::from_secs(5))
            .json(&body)
            .send()
            .await?;
        let r: FsResponse = resp
            .json()
            .await
            .map_err(|e| IndexerError::Cloudflare(format!("bad FlareSolverr reply: {e}")))?;
        match (r.status.as_str(), r.solution) {
            ("ok", Some(s)) => Ok(Solution {
                url: s.url,
                status: s.status,
                body: s.response,
                cookies: s.cookies.into_iter().map(|c| (c.name, c.value)).collect(),
                user_agent: s.user_agent,
            }),
            _ => Err(IndexerError::Cloudflare(if r.message.is_empty() {
                r.status
            } else {
                r.message
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guard_blocks_private_literals() {
        let g = AddressGuard { allow_private: false };
        for u in [
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://[::1]/",
            "http://localhost:9117/",
            "http://100.64.0.1/",
            "ftp://example.com/",
        ] {
            assert!(g.check(&Url::parse(u).unwrap()).await.is_err(), "{u}");
        }
        assert!(
            g.check_literal(&Url::parse("https://93.184.216.34/").unwrap())
                .is_ok()
        );
        assert!(
            AddressGuard { allow_private: true }
                .check_literal(&Url::parse("http://192.168.1.2/").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn cookie_jar() {
        let mut j = CookieJar::default();
        j.parse_header("uid=1; pass=abc");
        j.set("uid", "2");
        assert_eq!(j.header().as_deref(), Some("uid=2; pass=abc"));
    }

    #[test]
    fn proxy_url() {
        let p = ProxyConfig {
            kind: ProxyKind::Socks5,
            host: "p".into(),
            port: 1080,
            username: None,
            password: None,
        };
        assert_eq!(p.url(), "socks5h://p:1080");
        assert!(p.to_reqwest().is_ok());
    }
}
