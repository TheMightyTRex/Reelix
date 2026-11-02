//! HTTP app assembly: API + static web UI (SPA fallback, CSP), URL-base nesting and common layers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reelix_api_types::ApiErrorBody;
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// The literal base tag the web UI's `index.html` must contain.
pub const BASE_TAG: &str = r#"<base href="/" />"#;

/// The web UI being served: either a built Trunk `dist` directory or a built-in placeholder page.
pub enum WebUi {
    Built {
        serve_dir: ServeDir,
        index_html: String,
        csp: HeaderValue,
    },
    Missing {
        page: String,
        csp: HeaderValue,
    },
}

/// Picks the web directory: configured, else `./web` if present, else `crates/reelix-web/dist`.
pub fn resolve_web_dir(configured: Option<&Path>) -> PathBuf {
    if let Some(dir) = configured {
        return dir.to_path_buf();
    }
    let local = PathBuf::from("web");
    if local.is_dir() {
        local
    } else {
        PathBuf::from("crates/reelix-web/dist")
    }
}

impl WebUi {
    /// Loads `index.html` from `dir`, falling back to the placeholder page if it can't be read.
    pub fn load(dir: &Path, url_base: &str) -> WebUi {
        match std::fs::read_to_string(dir.join("index.html")) {
            Ok(raw) => {
                if url_base.is_empty() || raw.contains(BASE_TAG) {
                    tracing::info!(dir = %dir.display(), "serving web UI");
                } else {
                    tracing::warn!(dir = %dir.display(), "index.html lacks `{BASE_TAG}`; url_base won't apply to UI assets");
                }
                Self::from_index(dir, &raw, url_base)
            }
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "web UI not found; serving placeholder page");
                Self::missing(url_base)
            }
        }
    }

    /// Builds a `Built` UI from raw `index.html` contents.
    pub fn from_index(dir: &Path, raw_index: &str, url_base: &str) -> WebUi {
        let index_html = inject_base(raw_index, url_base);
        let csp = HeaderValue::from_str(&content_security_policy(&inline_script_hashes(&index_html)))
            .expect("CSP is ASCII");
        WebUi::Built {
            serve_dir: ServeDir::new(dir).append_index_html_on_directories(false),
            index_html,
            csp,
        }
    }

    /// The built-in "UI not built" page.
    pub fn missing(url_base: &str) -> WebUi {
        let page = format!(
            r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Reelix</title>
<style>body{{font-family:system-ui,sans-serif;background:#111318;color:#e6e8ee;display:grid;place-items:center;min-height:100vh;margin:0}}main{{max-width:36rem;padding:2rem}}a{{color:#7aa2ff}}code{{background:#22252d;padding:.1em .3em;border-radius:4px}}</style>
</head>
<body><main>
<h1>Reelix is running</h1>
<p>The web UI has not been built. Build it with <code>trunk build --release</code> in <code>crates/reelix-web</code>, or point <code>REELIX_WEB_DIR</code> at a built UI.</p>
<p>The API is available; see the <a href="{url_base}/api/docs">API documentation</a>.</p>
</main></body>
</html>
"#
        );
        let csp = HeaderValue::from_str(&content_security_policy(&[])).expect("CSP is ASCII");
        WebUi::Missing { page, csp }
    }

    /// Response for `index.html` (and SPA routes).
    pub fn index_response(&self) -> Response {
        let (html, csp) = match self {
            WebUi::Built { index_html, csp, .. } => (index_html.clone(), csp.clone()),
            WebUi::Missing { page, csp } => (page.clone(), csp.clone()),
        };
        (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                (header::CONTENT_SECURITY_POLICY, csp),
            ],
            html,
        )
            .into_response()
    }

    /// Fallback for every path no route matched.
    async fn fallback(&self, req: Request) -> Response {
        let path = req.uri().path();
        if path == "/api" || path.starts_with("/api/") {
            return api_not_found(path);
        }
        let WebUi::Built { serve_dir, .. } = self else {
            return self.index_response();
        };
        if req.method() != Method::GET && req.method() != Method::HEAD {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let res = match serve_dir.clone().oneshot(req).await {
            Ok(res) => res,
            Err(never) => match never {},
        };
        if res.status() == StatusCode::NOT_FOUND {
            // SPA route: let the client-side router handle it.
            self.index_response()
        } else {
            res.map(Body::new)
        }
    }
}

fn api_not_found(path: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "not_found".into(),
            message: format!("No API route for {path}"),
        }),
    )
        .into_response()
}

/// Rewrites the `<base href="/" />` tag to point at the URL base.
pub fn inject_base(html: &str, url_base: &str) -> String {
    html.replacen(BASE_TAG, &format!(r#"<base href="{url_base}/" />"#), 1)
}

/// `sha256-…` CSP sources for the body of every inline `<script>` (scripts with `src` are skipped).
pub fn inline_script_hashes(html: &str) -> Vec<String> {
    // ASCII lowercasing keeps byte offsets identical to the original.
    let lower = html.to_ascii_lowercase();
    let mut hashes = Vec::new();
    let mut pos = 0;
    while let Some(start) = lower[pos..].find("<script").map(|i| i + pos) {
        let after_name = start + "<script".len();
        // Must be `<script>` or `<script …>`, not e.g. `<scripts>`.
        if !lower[after_name..].starts_with(|c: char| c == '>' || c.is_ascii_whitespace()) {
            pos = after_name;
            continue;
        }
        let Some(tag_end) = lower[after_name..].find('>').map(|i| i + after_name) else {
            break;
        };
        let attrs = &lower[after_name..tag_end];
        let body_start = tag_end + 1;
        let Some(body_end) = lower[body_start..].find("</script").map(|i| i + body_start) else {
            break;
        };
        if !has_src_attr(attrs) {
            let digest = Sha256::digest(&html.as_bytes()[body_start..body_end]);
            hashes.push(format!("'sha256-{}'", BASE64.encode(digest)));
        }
        pos = body_end;
    }
    hashes
}

fn has_src_attr(attrs: &str) -> bool {
    attrs
        .split(|c: char| c.is_ascii_whitespace())
        .any(|a| a == "src" || a.starts_with("src="))
}

/// Third-party artwork hosts: TMDB and TheTVDB posters, Open Library book covers / author photos and
/// Cover Art Archive album art (both of which redirect to `*.archive.org`).
const IMG_HOSTS: &str = "https://image.tmdb.org https://artworks.thetvdb.com https://covers.openlibrary.org \
    https://coverartarchive.org https://archive.org https://*.archive.org";

/// Embedded trailers: YouTube's privacy-enhanced (no-cookie) player only.
const FRAME_HOSTS: &str = "https://www.youtube-nocookie.com";

/// The CSP served with `index.html`.
pub fn content_security_policy(script_hashes: &[String]) -> String {
    let mut script_src = String::from("'self' 'wasm-unsafe-eval'");
    for h in script_hashes {
        script_src.push(' ');
        script_src.push_str(h);
    }
    format!(
        "default-src 'self'; script-src {script_src}; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: {IMG_HOSTS}; connect-src 'self'; frame-src {FRAME_HOSTS}; \
         frame-ancestors 'none'; base-uri 'self'"
    )
}

/// Assembles the full app: API router + web UI, nested under `url_base`, with common layers.
pub fn build_app(api: Router, web: Arc<WebUi>, url_base: &str) -> Router {
    let index = {
        let web = web.clone();
        move || {
            let web = web.clone();
            async move { web.index_response() }
        }
    };
    let app = api
        .route("/", get(index.clone()))
        .route("/index.html", get(index.clone()))
        .fallback(move |req: Request| {
            let web = web.clone();
            async move { web.fallback(req).await }
        });

    let app = if url_base.is_empty() {
        app
    } else {
        let target = format!("{url_base}/");
        // `nest` matches `/base` and `/base/x` but not `/base/` itself.
        Router::new()
            .nest(url_base, app)
            .route(&format!("{url_base}/"), get(index))
            .route("/", get(move || async move { Redirect::temporary(&target) }))
    };

    app.layer(SetResponseHeaderLayer::if_not_present(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("DENY"),
    ))
    .layer(CompressionLayer::new())
    .layer(TraceLayer::new_for_http().make_span_with(redacted_span))
}

/// Request span with the query string's secrets (`apikey`, `api_key`, `token`, `passkey`)
/// redacted, so debug logs never contain API keys passed as `?apikey=`.
fn redacted_span(req: &axum::http::Request<axum::body::Body>) -> tracing::Span {
    let uri = req.uri();
    let query = uri.query().map(|q| {
        q.split('&')
            .map(|pair| {
                let key = pair.split('=').next().unwrap_or_default();
                let secret = ["apikey", "api_key", "token", "passkey", "link"]
                    .iter()
                    .any(|k| key.eq_ignore_ascii_case(k));
                if secret {
                    format!("{key}=REDACTED")
                } else {
                    pair.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("&")
    });
    let shown = match query {
        Some(q) => format!("{}?{q}", uri.path()),
        None => uri.path().to_owned(),
    };
    tracing::debug_span!("request", method = %req.method(), uri = %shown, version = ?req.version())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt;

    const INDEX: &str = r#"<!DOCTYPE html><html lang="en" data-theme="dark"><head>
<base href="/" />
<link rel="modulepreload" href="./app.js">
<script type="module">import init from './app.js'; await init();</script>
<SCRIPT src="./ext.js"></SCRIPT>
<script>console.log(1)</script>
</head><body></body></html>"#;

    fn sha(s: &str) -> String {
        format!("'sha256-{}'", BASE64.encode(Sha256::digest(s.as_bytes())))
    }

    #[test]
    fn hashes_only_inline_scripts() {
        let hashes = inline_script_hashes(INDEX);
        assert_eq!(
            hashes,
            vec![
                sha("import init from './app.js'; await init();"),
                sha("console.log(1)")
            ]
        );
        assert!(inline_script_hashes("<html><scripts>x</scripts></html>").is_empty());
        assert!(inline_script_hashes("<script>unterminated").is_empty());
    }

    #[test]
    fn csp_contains_hashes_and_directives() {
        let csp = content_security_policy(&["'sha256-abc'".to_string()]);
        assert!(csp.contains("script-src 'self' 'wasm-unsafe-eval' 'sha256-abc';"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("frame-src https://www.youtube-nocookie.com;"));
        assert!(csp.contains("img-src 'self' data: https://image.tmdb.org"));
        for host in [
            "https://artworks.thetvdb.com",
            "https://covers.openlibrary.org",
            "https://coverartarchive.org",
            "https://*.archive.org",
        ] {
            assert!(csp.contains(host), "{host}");
        }
        assert!(csp.starts_with("default-src 'self';"));
    }

    #[test]
    fn base_injection() {
        assert!(inject_base(INDEX, "").contains(r#"<base href="/" />"#));
        let html = inject_base(INDEX, "/reelix");
        assert!(html.contains(r#"<base href="/reelix/" />"#));
        assert!(!html.contains(r#"<base href="/" />"#));
    }

    fn dist() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), INDEX).unwrap();
        std::fs::write(dir.path().join("app.js"), "export default function init() {}").unwrap();
        dir
    }

    fn api() -> Router {
        Router::new().route("/api/v1/ping", get(|| async { "pong" }))
    }

    async fn call(app: &Router, method: Method, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
        let req = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn serves_ui_spa_and_api_404_at_root() {
        let dir = dist();
        let app = build_app(api(), Arc::new(WebUi::load(dir.path(), "")), "");

        let (s, h, body) = call(&app, Method::GET, "/").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains(r#"<base href="/" />"#));
        assert_eq!(h[header::CACHE_CONTROL], "no-cache");
        assert!(
            h[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("'sha256-")
        );
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::REFERRER_POLICY], "same-origin");

        let (s, h, _) = call(&app, Method::GET, "/index.html").await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.contains_key(header::CONTENT_SECURITY_POLICY));

        let (s, h, body) = call(&app, Method::GET, "/app.js").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("export default"));
        assert!(!h.contains_key(header::CONTENT_SECURITY_POLICY));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

        let (s, _, body) = call(&app, Method::GET, "/movies/123").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("<base href"));

        let (s, _, body) = call(&app, Method::GET, "/api/v1/ping").await;
        assert_eq!((s, body.as_str()), (StatusCode::OK, "pong"));

        let (s, h, body) = call(&app, Method::GET, "/api/v1/nope").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(h[header::CONTENT_TYPE], "application/json");
        let err: ApiErrorBody = serde_json::from_str(&body).unwrap();
        assert_eq!(err.error, "not_found");

        let (s, _, _) = call(&app, Method::POST, "/api").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn nests_under_url_base() {
        let dir = dist();
        let app = build_app(api(), Arc::new(WebUi::load(dir.path(), "/reelix")), "/reelix");

        let (s, h, _) = call(&app, Method::GET, "/").await;
        assert_eq!(s, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(h[header::LOCATION], "/reelix/");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");

        for uri in [
            "/reelix",
            "/reelix/",
            "/reelix/index.html",
            "/reelix/system/status",
        ] {
            let (s, h, body) = call(&app, Method::GET, uri).await;
            assert_eq!(s, StatusCode::OK, "{uri}");
            assert!(body.contains(r#"<base href="/reelix/" />"#), "{uri}");
            assert!(h.contains_key(header::CONTENT_SECURITY_POLICY), "{uri}");
        }

        let (s, _, body) = call(&app, Method::GET, "/reelix/app.js").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("export default"));

        let (s, _, body) = call(&app, Method::GET, "/reelix/api/v1/ping").await;
        assert_eq!((s, body.as_str()), (StatusCode::OK, "pong"));

        let (s, _, body) = call(&app, Method::GET, "/reelix/api/v1/nope").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert!(body.contains("not_found"));

        let (s, _, _) = call(&app, Method::GET, "/api/v1/ping").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn placeholder_page_when_ui_missing() {
        let dir = tempfile::tempdir().unwrap();
        let app = build_app(api(), Arc::new(WebUi::load(&dir.path().join("nope"), "/x")), "/x");
        let (s, h, body) = call(&app, Method::GET, "/x/").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains(r#"href="/x/api/docs""#));
        assert!(h.contains_key(header::CONTENT_SECURITY_POLICY));
        let (s, _, body) = call(&app, Method::GET, "/x/settings").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("not been built"));
        let (s, _, _) = call(&app, Method::GET, "/x/api/v2").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[test]
    fn web_dir_resolution_prefers_configured() {
        assert_eq!(
            resolve_web_dir(Some(Path::new("/app/web"))),
            PathBuf::from("/app/web")
        );
    }
}
