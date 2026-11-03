//! Runtime for a loaded [`Definition`]: login, search requests, row/field extraction.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Utc};
use reelix_core::Protocol;
use reqwest::Method;
use reqwest::header::{CONTENT_TYPE, COOKIE, LOCATION, USER_AGENT};
use scraper::{ElementRef, Html, Node};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use super::model::{Definition, ErrorDef, FieldDef, FilterDef, OrderedMap, PathDef, Scalar, StrList};
use super::selector::{self, Selector};
use super::template::{Context, TValue, render, render_urlencoded};
use super::{filters, jsonsel, schema};
use crate::http::{
    AddressGuard, CookieJar, FlareSolverr, build_client, is_cloudflare_challenge, status_error,
};
use crate::model::{
    Category, IndexerCapabilities, ReleaseInfo, SearchKind, SearchMode, SearchParam, SearchQuery,
};
use crate::newznab::magnet_hash;
use crate::util::{SettingsExt, parse_size};
use crate::{BuildContext, Indexer, IndexerError, categories};

/// Fields whose failure never drops a row (Cardigann's `OptionalFields`).
const OPTIONAL_FIELDS: &[&str] = &[
    "imdb",
    "imdbid",
    "tmdbid",
    "rageid",
    "tvdbid",
    "tvmazeid",
    "traktid",
    "doubanid",
    "poster",
    "banner",
    "description",
    "genre",
];

#[derive(Default)]
struct Session {
    jar: CookieJar,
    logged_in: bool,
    /// User agent FlareSolverr solved the challenge with (clearance cookies are bound to it).
    user_agent: Option<String>,
    /// When the last request was sent (`requestDelay`).
    last_request: Option<Instant>,
}

struct Page {
    url: Url,
    body: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Html,
    Json,
    Xml,
}

/// An indexer driven by a YAML definition.
pub struct DefinitionIndexer {
    def: Arc<Definition>,
    indexer_id: i64,
    name: String,
    base: Url,
    config: Vec<(String, TValue)>,
    protocol: Protocol,
    caps: IndexerCapabilities,
    /// `(site category id, newznab id)`.
    cats: Vec<(String, u32)>,
    client: reqwest::Client,
    guard: AddressGuard,
    flaresolverr: Option<FlareSolverr>,
    request_delay: Option<Duration>,
    session: Mutex<Session>,
}

impl DefinitionIndexer {
    /// Validates `settings` against the definition's schema and builds the indexer.
    pub fn from_settings(
        def: Arc<Definition>,
        settings: &Value,
        ctx: &BuildContext,
    ) -> Result<Self, IndexerError> {
        let s = schema(&def).validate(settings).map_err(IndexerError::Config)?;
        let mut base_s = s.str_opt("base_url").unwrap_or_else(|| def.links[0].clone());
        if !base_s.ends_with('/') {
            base_s.push('/');
        }
        let base = Url::parse(&base_s).map_err(|e| IndexerError::Config(format!("site URL: {e}")))?;
        let guard = ctx.guard();
        guard.check_literal(&base)?;
        let mut config = vec![("sitelink".to_string(), TValue::Str(base.to_string()))];
        for d in def.settings.iter().filter(|d| !d.kind.starts_with("info")) {
            let v = match (d.kind.as_str(), s.get(&d.name)) {
                ("checkbox", Some(Value::Bool(b))) => TValue::Bool(*b),
                ("checkbox", Some(Value::String(v))) => TValue::Bool(v == "true"),
                ("multi-select", Some(Value::Array(a))) => TValue::List(
                    a.iter()
                        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                        .collect(),
                ),
                ("checkbox", _) => TValue::Bool(false),
                _ => s.str_opt(&d.name).map_or(TValue::Str(String::new()), TValue::Str),
            };
            config.push((d.name.clone(), v));
        }
        let protocol = match def.protocol.as_deref() {
            Some("usenet") => Protocol::Usenet,
            _ => Protocol::Torrent,
        };
        let (caps, cats) = build_caps(&def);
        let flaresolverr = match &ctx.flaresolverr {
            Some(u) => Some(FlareSolverr::new(ctx.http.clone(), u)?),
            None => None,
        };
        let request_delay = def
            .request_delay
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| Duration::from_secs_f64(d.min(60.0)));
        Ok(Self {
            def,
            indexer_id: ctx.indexer_id,
            name: ctx.name.clone(),
            base,
            config,
            protocol,
            caps,
            cats,
            client: build_client(&ctx.http_options(), false)?,
            guard,
            flaresolverr,
            request_delay,
            session: Mutex::new(Session::default()),
        })
    }

    fn base_ctx(&self) -> Context {
        let mut c = Context::default();
        for (k, v) in &self.config {
            c.set(format!("Config.{k}"), v.clone());
        }
        base_vars(&mut c, Utc::now());
        c
    }

    fn join(&self, path: &str) -> Result<Url, IndexerError> {
        self.base
            .join(path)
            .map_err(|e| IndexerError::Config(format!("bad path `{path}`: {e}")))
    }

    /// Whether `url` is on the tracker itself (its base URL or one of the definition's links,
    /// or a subdomain of either). Session cookies and definition headers are only sent there,
    /// so a download link or redirect pointing elsewhere can't capture them.
    fn own_host(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let related = |known: &str| crate::http::host_within(host, known);
        self.base.host_str().is_some_and(related)
            || self
                .def
                .links
                .iter()
                .filter_map(|l| Url::parse(l).ok())
                .any(|l| l.host_str().is_some_and(related))
    }

    /// Sends a request, following redirects manually so cookies set on 3xx are kept.
    async fn send(
        &self,
        s: &mut Session,
        method: Method,
        url: Url,
        body: Option<String>,
        headers: &[(String, String)],
    ) -> Result<(Url, reqwest::Response), IndexerError> {
        let (mut method, mut url, mut body) = (method, url, body);
        let custom_ct = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
        for _ in 0..10 {
            self.guard.check(&url).await?;
            if let Some(delay) = self.request_delay {
                if let Some(wait) = s.last_request.map(|t| delay.saturating_sub(t.elapsed()))
                    && !wait.is_zero()
                {
                    tokio::time::sleep(wait).await;
                }
                s.last_request = Some(Instant::now());
            }
            let mut rb = self.client.request(method.clone(), url.clone());
            let own = self.own_host(&url);
            for (k, v) in headers.iter().filter(|_| own) {
                if k.eq_ignore_ascii_case("cookie") {
                    continue;
                }
                rb = rb.header(k.as_str(), v.as_str());
            }
            let mut cookie = if own { s.jar.header() } else { None };
            if let Some((_, c)) = headers
                .iter()
                .filter(|_| own)
                .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
            {
                cookie = Some(match cookie {
                    Some(j) => format!("{c}; {j}"),
                    None => c.clone(),
                });
            }
            if let Some(c) = cookie {
                rb = rb.header(COOKIE, c);
            }
            if let Some(ua) = &s.user_agent {
                rb = rb.header(USER_AGENT, ua);
            }
            if let Some(b) = &body {
                if !custom_ct {
                    rb = rb.header(CONTENT_TYPE, "application/x-www-form-urlencoded");
                }
                rb = rb.body(b.clone());
            }
            let resp = rb.send().await?;
            if own {
                s.jar.absorb(resp.headers());
            }
            if !resp.status().is_redirection() {
                return Ok((url, resp));
            }
            let Some(loc) = resp.headers().get(LOCATION).and_then(|l| l.to_str().ok()) else {
                return Ok((url, resp));
            };
            if loc.starts_with("magnet:") {
                return Err(IndexerError::MagnetRedirect(loc.to_string()));
            }
            let next = url
                .join(loc)
                .map_err(|e| IndexerError::Parse(format!("bad redirect: {e}")))?;
            (method, body) = redirect_request(resp.status().as_u16(), method, body, self.own_host(&next));
            url = next;
        }
        Err(IndexerError::Parse("too many redirects".into()))
    }

    /// Fetches a page as text, solving Cloudflare challenges through FlareSolverr.
    async fn request(
        &self,
        s: &mut Session,
        method: Method,
        url: Url,
        body: Option<String>,
        headers: &[(String, String)],
    ) -> Result<Page, IndexerError> {
        let (final_url, resp) = self.send(s, method, url, body.clone(), headers).await?;
        let status = resp.status().as_u16();
        let resp_headers = resp.headers().clone();
        let err = status_error(&resp);
        let charset = resp_headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|ct| ct.split(';').find_map(|p| p.trim().strip_prefix("charset=")))
            .map(|c| c.trim_matches('"').to_string());
        let bytes = crate::http::read_limited(resp, crate::http::MAX_PAGE_BYTES).await?;
        let text = decode(&bytes, charset.as_deref().unwrap_or(&self.def.encoding));
        // Only the tracker itself is solved: a redirect elsewhere must not get the session
        // cookies or the login body, nor plant cookies in the tracker's jar.
        if is_cloudflare_challenge(status, &resp_headers, &text) && self.own_host(&final_url) {
            let fs = self.flaresolverr.as_ref().ok_or_else(|| {
                IndexerError::Cloudflare("challenge detected; configure FlareSolverr".into())
            })?;
            let sol = fs.solve(final_url.as_str(), body.as_deref(), &s.jar).await?;
            let url = Url::parse(&sol.url).unwrap_or(final_url);
            if self.own_host(&url) {
                for (n, v) in &sol.cookies {
                    s.jar.set(n, v);
                }
                if sol.user_agent.is_some() {
                    s.user_agent = sol.user_agent;
                }
            }
            return Ok(Page { url, body: sol.body });
        }
        if let Some(e) = err {
            return Err(e);
        }
        Ok(Page {
            url: final_url,
            body: text,
        })
    }

    fn render_headers(&self, h: &OrderedMap<StrList>, ctx: &Context) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, vals) in h.iter() {
            for v in &vals.0 {
                match render(v, ctx) {
                    Ok(v) if !v.is_empty() => out.push((k.to_string(), v)),
                    Ok(_) => {}
                    Err(e) => tracing::debug!(header = k, error = %e, "definition header template failed"),
                }
            }
        }
        out
    }

    fn search_headers(&self, ctx: &Context) -> Vec<(String, String)> {
        self.render_headers(&self.def.search.headers, ctx)
    }

    /// Renders inputs; `$raw` values are appended verbatim (with URL-encoded substitutions).
    fn render_inputs(
        &self,
        inputs: &[(&str, &Scalar)],
        ctx: &Context,
        keep_empty: bool,
    ) -> Result<(Vec<(String, String)>, String), IndexerError> {
        let mut pairs = Vec::new();
        let mut raw = String::new();
        for (k, v) in inputs {
            if *k == "$raw" {
                let val = render_urlencoded(&v.0, ctx).map_err(|e| IndexerError::Config(e.to_string()))?;
                if !raw.is_empty() && !val.is_empty() && !raw.ends_with('&') {
                    raw.push('&');
                }
                raw.push_str(&val);
            } else {
                let val = render(&v.0, ctx).map_err(|e| IndexerError::Config(e.to_string()))?;
                if keep_empty || !val.trim().is_empty() {
                    pairs.push((k.to_string(), val));
                }
            }
        }
        Ok((pairs, raw))
    }

    fn encode(&self, pairs: &[(String, String)]) -> String {
        encode_with(pairs, &self.def.encoding)
    }

    async fn login(&self, s: &mut Session) -> Result<(), IndexerError> {
        let Some(login) = &self.def.login else {
            s.logged_in = true;
            return Ok(());
        };
        let ctx = self.base_ctx();
        for c in &login.cookies {
            s.jar.parse_header(&render(c, &ctx).unwrap_or_else(|_| c.clone()));
        }
        let headers = self.render_headers(&login.headers, &ctx);
        let inputs: Vec<(&str, &Scalar)> = login.inputs.iter().collect();
        let (pairs, _) = self.render_inputs(&inputs, &ctx, true)?;
        let path = render(login.path.as_deref().unwrap_or(""), &ctx)
            .map_err(|e| IndexerError::Config(e.to_string()))?;
        let method = login.method.as_deref().unwrap_or("post").to_ascii_lowercase();
        match method.as_str() {
            "cookie" => {
                for (_, v) in &pairs {
                    s.jar.parse_header(v);
                }
                if s.jar.is_empty() {
                    return Err(IndexerError::Config("a cookie is required".into()));
                }
            }
            "get" => {
                let mut u = self.join(&path)?;
                if !pairs.is_empty() {
                    append_query(&mut u, &self.encode(&pairs));
                }
                let page = self.request(s, Method::GET, u, None, &headers).await?;
                check_errors(&page.body, &login.error, &ctx).map_err(IndexerError::Auth)?;
            }
            "oneurl" => {
                let one = pairs
                    .iter()
                    .find(|p| p.0 == "oneurl")
                    .map(|p| p.1.clone())
                    .unwrap_or_default();
                let page = self
                    .request(
                        s,
                        Method::GET,
                        self.join(&format!("{path}{one}"))?,
                        None,
                        &headers,
                    )
                    .await?;
                check_errors(&page.body, &login.error, &ctx).map_err(IndexerError::Auth)?;
            }
            "form" => {
                let page = self
                    .request(s, Method::GET, self.join(&path)?, None, &headers)
                    .await?;
                let form_sel = login.form.as_deref().unwrap_or("form");
                let form = parse_form(&page.body, form_sel, &ctx, login, &pairs).map_err(|e| {
                    match e.strip_prefix("unsupported: ") {
                        Some(m) => IndexerError::Unsupported(m.to_string()),
                        None => IndexerError::Parse(e),
                    }
                })?;
                let target = match &login.submitpath {
                    Some(p) => self.join(&render(p, &ctx).unwrap_or_else(|_| p.clone()))?,
                    None => page
                        .url
                        .join(&form.action)
                        .map_err(|e| IndexerError::Parse(e.to_string()))?,
                };
                let mut target = target;
                if !form.query.is_empty() {
                    append_query(&mut target, &self.encode(&form.query));
                }
                let body = self.encode(&form.fields);
                let resp = if form.method.eq_ignore_ascii_case("get") {
                    append_query(&mut target, &body);
                    self.request(s, Method::GET, target, None, &headers).await?
                } else {
                    self.request(s, Method::POST, target, Some(body), &headers)
                        .await?
                };
                check_errors(&resp.body, &login.error, &ctx).map_err(IndexerError::Auth)?;
            }
            "post" => {
                let page = self
                    .request(
                        s,
                        Method::POST,
                        self.join(&path)?,
                        Some(self.encode(&pairs)),
                        &headers,
                    )
                    .await?;
                check_errors(&page.body, &login.error, &ctx).map_err(IndexerError::Auth)?;
            }
            other => return Err(IndexerError::Unsupported(format!("login method `{other}`"))),
        }
        if let Some(test) = &login.test {
            let page = self
                .request(
                    s,
                    Method::GET,
                    self.join(&render(&test.path, &ctx).unwrap_or_default())?,
                    None,
                    &headers,
                )
                .await?;
            let ok = match &test.selector {
                Some(sel) => html_has(&page.body, sel, &ctx),
                None => !self.is_login_page(&page.url),
            };
            if !ok {
                return Err(IndexerError::Auth("login test failed (not logged in)".into()));
            }
        }
        s.logged_in = true;
        Ok(())
    }

    fn is_login_page(&self, url: &Url) -> bool {
        let Some(login) = &self.def.login else {
            return false;
        };
        if matches!(login.method.as_deref(), Some("cookie")) {
            return false;
        }
        login
            .path
            .as_deref()
            .filter(|p| !p.is_empty() && !p.contains("{{"))
            .and_then(|p| self.join(p).ok())
            .is_some_and(|l| l.path() == url.path() && l.path() != self.base.path())
    }

    fn query_ctx(&self, q: &SearchQuery) -> Result<(Context, Vec<String>), IndexerError> {
        let mut ctx = self.base_ctx();
        let now = Utc::now();
        let kw = filters::apply(q.keywords(), &self.def.search.keywordsfilters, &ctx, now)
            .map_err(IndexerError::Config)?;
        ctx.set("Keywords", kw.clone());
        ctx.set("Query.Keywords", kw);
        ctx.set("Query.Q", q.term.clone().unwrap_or_default());
        ctx.set("Query.Type", self.query_type(q.kind));
        let opt = |v: Option<String>| v.map_or(TValue::Null, TValue::Str);
        ctx.set("Query.IMDBID", opt(q.imdb_digits().map(|d| format!("tt{d}"))));
        ctx.set("Query.IMDBIDShort", opt(q.imdb_digits()));
        ctx.set("Query.TMDBID", opt(q.tmdb_id.map(|v| v.to_string())));
        ctx.set("Query.TVDBID", opt(q.tvdb_id.map(|v| v.to_string())));
        ctx.set("Query.Season", opt(q.season.map(|v| v.to_string())));
        ctx.set("Query.Ep", opt(q.episode.clone()));
        ctx.set("Query.Year", opt(q.year.map(|v| v.to_string())));
        ctx.set("Query.Limit", opt(q.limit.map(|v| v.to_string())));
        ctx.set("Query.Offset", opt(q.offset.map(|v| v.to_string())));
        ctx.set("Query.Author", opt(q.author.clone()));
        ctx.set("Query.Title", opt(q.title.clone()));
        ctx.set("Query.Artist", opt(q.artist.clone()));
        ctx.set("Query.Album", opt(q.album.clone()));
        let wanted = self.categories_for(q);
        // Book/music: a parent (7000, 3000) doesn't pull in Mags, Audio/Video, … unless asked for.
        let excluded: &[u32] = if matches!(q.kind, SearchKind::Book | SearchKind::Music) {
            categories::excluded_for(q.content_kind())
        } else {
            &[]
        };
        let mut site: Vec<String> = Vec::new();
        for (sid, nz) in &self.cats {
            if excluded.contains(nz) && !wanted.contains(nz) {
                continue;
            }
            if wanted.iter().any(|&w| categories::matches(w, *nz)) && !site.contains(sid) {
                site.push(sid.clone());
            }
        }
        ctx.set("Categories", TValue::List(site.clone()));
        ctx.set("Query.Categories", TValue::List(site.clone()));
        Ok((ctx, site))
    }

    /// Cardigann `Query.Type`: `book`/`music` only when the definition declares
    /// `book-search`/`music-search`, otherwise a plain `search`.
    fn query_type(&self, kind: SearchKind) -> &'static str {
        match kind {
            SearchKind::Basic => "search",
            SearchKind::Movie => "movie",
            SearchKind::Tv => "tvsearch",
            SearchKind::Book if self.caps.book_search.available => "book",
            SearchKind::Music if self.caps.audio_search.available => "music",
            SearchKind::Book | SearchKind::Music => "search",
        }
    }

    /// Requested Newznab categories; book/music queries without any use the kind's defaults.
    fn categories_for(&self, q: &SearchQuery) -> Vec<u32> {
        if q.categories.is_empty() && matches!(q.kind, SearchKind::Book | SearchKind::Music) {
            return categories::defaults_for(q.content_kind()).to_vec();
        }
        q.categories.clone()
    }

    /// False for a book/music query against a site with categories but none of the wanted
    /// ones (a movie-only tracker is not searched for books).
    fn serves(&self, q: &SearchQuery) -> bool {
        if !matches!(q.kind, SearchKind::Book | SearchKind::Music) || self.cats.is_empty() {
            return true;
        }
        let wanted = self.categories_for(q);
        self.cats
            .iter()
            .any(|(_, nz)| wanted.iter().any(|&w| categories::matches(w, *nz)))
    }

    fn paths(&self, site_cats: &[String]) -> Vec<PathDef> {
        let s = &self.def.search;
        if s.paths.is_empty() {
            return vec![PathDef {
                path: s.path.clone().unwrap_or_default(),
                method: None,
                categories: Vec::new(),
                inputs: OrderedMap::default(),
                inheritinputs: true,
                response: None,
                followredirect: false,
            }];
        }
        let matching: Vec<PathDef> = s
            .paths
            .iter()
            .filter(|p| {
                p.categories.is_empty()
                    || site_cats.is_empty()
                    || p.categories.iter().any(|c| site_cats.contains(&c.0))
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            vec![s.paths[0].clone()]
        } else {
            matching
        }
    }

    async fn fetch_path(&self, s: &mut Session, p: &PathDef, ctx: &Context) -> Result<Page, IndexerError> {
        let path = render_urlencoded(&p.path, ctx).map_err(|e| IndexerError::Config(e.to_string()))?;
        let mut url = self.join(&path)?;
        let mut inputs: Vec<(&str, &Scalar)> = Vec::new();
        if p.inheritinputs {
            inputs.extend(self.def.search.inputs.iter());
        }
        for (k, v) in p.inputs.iter() {
            inputs.retain(|e| e.0 != k);
            inputs.push((k, v));
        }
        let (pairs, raw) = self.render_inputs(&inputs, ctx, self.def.search.allow_empty_inputs)?;
        let mut qs = self.encode(&pairs);
        let raw = raw.trim_matches('&');
        if !raw.is_empty() {
            if !qs.is_empty() {
                qs.push('&');
            }
            qs.push_str(raw);
        }
        let headers = self.search_headers(ctx);
        if p.method
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("post"))
        {
            return self.request(s, Method::POST, url, Some(qs), &headers).await;
        }
        if !qs.is_empty() {
            append_query(&mut url, &qs);
        }
        self.request(s, Method::GET, url, None, &headers).await
    }

    async fn run_search(&self, q: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let (ctx, site_cats) = self.query_ctx(q)?;
        let mut s = self.session.lock().await;
        if !s.logged_in {
            self.login(&mut s).await?;
        }
        let mut out: Vec<ReleaseInfo> = Vec::new();
        let mut seen = HashSet::new();
        for p in self.paths(&site_cats) {
            let mut page = self.fetch_path(&mut s, &p, &ctx).await?;
            if self.is_login_page(&page.url) {
                // Session expired: log in again once and retry.
                s.logged_in = false;
                s.jar = CookieJar::default();
                self.login(&mut s).await?;
                page = self.fetch_path(&mut s, &p, &ctx).await?;
                if self.is_login_page(&page.url) {
                    return Err(IndexerError::Auth("redirected to login page".into()));
                }
            }
            if let Some(msg) = p.response.as_ref().and_then(|r| r.no_results_message.as_deref())
                && page.body.trim() == msg.trim()
            {
                continue;
            }
            let format = match p
                .response
                .as_ref()
                .map(|r| r.kind.to_ascii_lowercase())
                .as_deref()
            {
                Some("json") => Format::Json,
                Some("xml") => Format::Xml,
                _ => Format::Html,
            };
            for r in self.parse_page(&page, format, &ctx, q)? {
                let key = (r.guid.clone(), r.download_url.clone(), r.magnet_url.clone());
                if seen.insert(key) {
                    out.push(r);
                }
            }
        }
        Ok(out)
    }

    fn parse_page(
        &self,
        page: &Page,
        format: Format,
        ctx: &Context,
        q: &SearchQuery,
    ) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let now = Utc::now();
        let maps = match format {
            Format::Json => self.parse_json(&page.body, ctx, now)?,
            Format::Html | Format::Xml => {
                let mut body = page.body.clone();
                if !self.def.search.preprocessingfilters.is_empty() {
                    body = filters::apply(body, &self.def.search.preprocessingfilters, ctx, now)
                        .map_err(IndexerError::Parse)?;
                }
                let doc = if format == Format::Xml {
                    parse_xml(&body).map_err(IndexerError::Parse)?
                } else {
                    Html::parse_document(&body)
                };
                self.parse_html(doc, ctx, now)?
            }
        };
        let andmatch = self.def.search.rows.filters.iter().find(|f| f.name == "andmatch");
        Ok(maps
            .into_iter()
            .filter_map(|m| self.to_release(&m, now))
            .filter(|r| {
                andmatch.is_none_or(|f| {
                    // Keywords after `keywordsfilters`, like the request itself.
                    let kw = ctx.vars.get("Keywords").map(TValue::render).unwrap_or_default();
                    q.imdb_digits().is_some() || and_match(&r.title, &kw, f)
                })
            })
            .collect())
    }

    fn parse_html(
        &self,
        mut doc: Html,
        ctx: &Context,
        now: DateTime<Utc>,
    ) -> Result<Vec<Vec<(String, String)>>, IndexerError> {
        let rows_def = &self.def.search.rows;
        if let Some(msg) = first_error(&doc, &self.def.search.error, ctx) {
            return Err(IndexerError::Api {
                code: 0,
                description: msg,
            });
        }
        if let Some(count) = &rows_def.count
            && let Some(n) = html_value(doc.root_element(), count, ctx)
                .and_then(|v| filters::apply(v, &count.filters, ctx, now).ok())
                .and_then(|v| v.trim().replace([',', '.'], "").parse::<i64>().ok())
            && n < 1
        {
            return Ok(Vec::new());
        }
        let rows_sel = sel(&rows_def.selector, ctx).map_err(IndexerError::Config)?;
        let mut ids: Vec<ego_tree::NodeId> = rows_sel.select(doc.tree.root(), None).map(|e| e.id()).collect();
        if rows_def.after > 0 {
            let mut kept = Vec::new();
            let mut i = 0;
            while i < ids.len() {
                let cur = ids[i];
                for j in 1..=rows_def.after {
                    if let (Some(&m), Some(mut node)) = (ids.get(i + j), doc.tree.get_mut(cur)) {
                        node.reparent_from_id_append(m);
                    }
                }
                kept.push(cur);
                i += rows_def.after + 1;
            }
            ids = kept;
        }
        let mut maps = Vec::new();
        for id in ids {
            let Some(row) = doc.tree.get(id).and_then(ElementRef::wrap) else {
                continue;
            };
            let Some(mut m) = extract_fields(&self.def.search.fields, ctx, now, |f, c| html_value(row, f, c))
            else {
                continue;
            };
            if let Some(dh) = &rows_def.dateheaders {
                match date_header(row, dh, ctx, now) {
                    Some(d) => m.push(("date".into(), d)),
                    None if dh.optional => {}
                    None => {
                        tracing::debug!("no date header row found; skipping row");
                        continue;
                    }
                }
            }
            maps.push(m);
        }
        Ok(maps)
    }

    fn parse_json(
        &self,
        body: &str,
        ctx: &Context,
        now: DateTime<Utc>,
    ) -> Result<Vec<Vec<(String, String)>>, IndexerError> {
        let rows_def = &self.def.search.rows;
        let v: Value = serde_json::from_str(body).map_err(|e| IndexerError::Parse(format!("JSON: {e}")))?;
        if let Some(count) = &rows_def.count
            && let Some(sel_src) = &count.selector
            && let Some(n) = render(sel_src, ctx)
                .ok()
                .and_then(|s| jsonsel::select(&v, s.trim_start_matches('.')).and_then(jsonsel::field_value))
                .and_then(|s| s.trim().parse::<f64>().ok())
            && n < 1.0
        {
            return Ok(Vec::new());
        }
        let rows_sel = render(&rows_def.selector, ctx).map_err(|e| IndexerError::Config(e.to_string()))?;
        let mut maps = Vec::new();
        for row in jsonsel::rows(&v, &rows_sel) {
            let sel_obj = match &rows_def.attribute {
                Some(a) => match jsonsel::path(row, a) {
                    Some(x) => x,
                    None => continue,
                },
                None => row,
            };
            let items: Vec<&Value> = if rows_def.multiple {
                match sel_obj {
                    Value::Array(a) => a.iter().collect(),
                    Value::Object(o) => o.values().collect(),
                    _ => Vec::new(),
                }
            } else {
                vec![sel_obj]
            };
            let parent = rows_def.attribute.as_ref().map(|_| row);
            for item in items {
                if let Some(m) = extract_fields(&self.def.search.fields, ctx, now, |f, c| {
                    json_value(item, parent, f, c)
                }) {
                    maps.push(m);
                }
            }
        }
        Ok(maps)
    }

    /// Relative links resolve against the site link (as Cardigann does), not the page URL.
    fn to_release(&self, m: &[(String, String)], now: DateTime<Utc>) -> Option<ReleaseInfo> {
        let get = |k: &str| {
            m.iter()
                .rev()
                .find(|e| e.0 == k)
                .map(|e| e.1.trim())
                .filter(|v| !v.is_empty())
        };
        let num = |k: &str| get(k).and_then(parse_num);
        let abs = |v: &str| self.base.join(v).ok().map(|u| u.to_string());
        let mut r = ReleaseInfo::new(self.protocol, self.indexer_id, &self.name);
        r.title = get("title")?.split_whitespace().collect::<Vec<_>>().join(" ");
        r.info_url = get("details").or_else(|| get("comments")).and_then(abs);
        match get("download") {
            Some(d) if d.starts_with("magnet:") => r.magnet_url = Some(d.to_string()),
            Some(d) => r.download_url = abs(d),
            None => {}
        }
        if let Some(mg) = get("magnet") {
            r.magnet_url = Some(mg.to_string());
        }
        r.info_hash = get("infohash")
            .map(str::to_string)
            .or_else(|| r.magnet_url.as_deref().and_then(magnet_hash));
        if r.download_url.is_none() && r.magnet_url.is_none() {
            let h = r.info_hash.as_ref()?;
            let dn: String = url::form_urlencoded::byte_serialize(r.title.as_bytes()).collect();
            r.magnet_url = Some(format!("magnet:?xt=urn:btih:{h}&dn={dn}"));
        }
        r.size = get("size").and_then(parse_size);
        r.seeders = num("seeders").map(|v| v as u32);
        let leechers = num("leechers").map(|v| v as u32);
        r.peers = num("peers").map(|v| v as u32).or(match (r.seeders, leechers) {
            (Some(s), Some(l)) => Some(s + l),
            (s, l) => s.or(l),
        });
        r.grabs = num("grabs").map(|v| v as u32);
        r.files = num("files").map(|v| v as u32);
        r.publish_date = get("date").and_then(|d| filters::from_unknown(d, now));
        let mut cats: Vec<u32> = Vec::new();
        for (k, sid) in m.iter().filter(|e| e.0 == "category" || e.0 == "categorydesc") {
            let sid = sid.trim();
            if sid.is_empty() {
                continue;
            }
            if k == "category" && sid == "\u{0}noappend" {
                cats.clear();
                continue;
            }
            let before = cats.len();
            for (s, nz) in &self.cats {
                if s.eq_ignore_ascii_case(sid) && !cats.contains(nz) {
                    cats.push(*nz);
                }
            }
            if cats.len() == before
                && let Some(nz) = categories::by_name(sid)
                && !cats.contains(&nz)
            {
                cats.push(nz);
            }
        }
        r.categories = cats;
        r.imdb_id = get("imdb")
            .or_else(|| get("imdbid"))
            .map(|v| v.rsplit('/').find(|s| !s.is_empty()).unwrap_or(v).to_string())
            .and_then(|v| {
                let digits: String = v.chars().filter(char::is_ascii_digit).collect();
                (!digits.trim_start_matches('0').is_empty()).then(|| format!("tt{digits:0>7}"))
            });
        r.tmdb_id = num("tmdbid").map(|v| v as u32).filter(|&v| v > 0);
        r.tvdb_id = num("tvdbid").map(|v| v as u32).filter(|&v| v > 0);
        r.download_volume_factor = num("downloadvolumefactor").unwrap_or(1.0);
        r.upload_volume_factor = num("uploadvolumefactor").unwrap_or(1.0);
        r.minimum_ratio = num("minimumratio");
        r.minimum_seed_time = num("minimumseedtime").map(|v| v as u64);
        r.guid = r
            .info_url
            .clone()
            .or_else(|| r.download_url.clone())
            .or_else(|| r.magnet_url.clone())
            .unwrap_or_default();
        r.finish();
        Some(r)
    }

    /// Variables for `download.before` / `download.infohash`: `.DownloadUri.*`.
    fn download_ctx(&self, link: &Url) -> Context {
        let mut c = self.base_ctx();
        c.set("DownloadUri.AbsoluteUri", link.to_string());
        c.set("DownloadUri.AbsolutePath", link.path().to_string());
        let pq = match link.query() {
            Some(q) => format!("{}?{q}", link.path()),
            None => link.path().to_string(),
        };
        c.set("DownloadUri.PathAndQuery", pq);
        for (k, v) in link.query_pairs() {
            c.set(format!("DownloadUri.Query.{k}"), v.into_owned());
        }
        c
    }

    async fn do_download(&self, url: &str) -> Result<Vec<u8>, IndexerError> {
        if url.starts_with("magnet:") {
            return Err(IndexerError::MagnetRedirect(url.to_string()));
        }
        let mut s = self.session.lock().await;
        if !s.logged_in {
            self.login(&mut s).await?;
        }
        let mut target = self.join(url)?;
        let ctx = self.download_ctx(&target);
        let dl = self.def.download.as_ref();
        let mut headers = self.search_headers(&ctx);
        if let Some(d) = dl {
            headers.extend(self.render_headers(&d.headers, &ctx));
        }
        let mut before_page = None;
        if let Some(b) = dl.and_then(|d| d.before.as_ref()) {
            let path = render(&b.path, &ctx).map_err(|e| IndexerError::Config(e.to_string()))?;
            let mut u = self.join(&path)?;
            let inputs: Vec<(&str, &Scalar)> = b.inputs.iter().collect();
            let (pairs, raw) = self.render_inputs(&inputs, &ctx, true)?;
            let mut qs = self.encode(&pairs);
            if !raw.is_empty() {
                if !qs.is_empty() {
                    qs.push('&');
                }
                qs.push_str(raw.trim_matches('&'));
            }
            let page = if b
                .method
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("post"))
            {
                self.request(&mut s, Method::POST, u, Some(qs), &headers).await?
            } else {
                if !qs.is_empty() {
                    append_query(&mut u, &qs);
                }
                self.request(&mut s, Method::GET, u, None, &headers).await?
            };
            before_page = Some(page);
        }
        if let Some(ih) = dl.and_then(|d| d.infohash.as_ref()) {
            let page = match before_page.filter(|_| ih.usebeforeresponse) {
                Some(p) => p,
                None => {
                    self.request(&mut s, Method::GET, target.clone(), None, &headers)
                        .await?
                }
            };
            let doc = Html::parse_document(&page.body);
            let now = Utc::now();
            let pick = |f: &FieldDef| {
                html_value(doc.root_element(), f, &ctx)
                    .and_then(|v| filters::apply(v, &f.filters, &ctx, now).ok())
            };
            let hash = pick(&ih.hash)
                .filter(|h| !h.trim().is_empty())
                .ok_or_else(|| IndexerError::Parse("info hash not found on details page".into()))?;
            let mut magnet = format!("magnet:?xt=urn:btih:{}", hash.trim());
            if let Some(title) = ih.title.as_ref().and_then(pick).filter(|t| !t.is_empty()) {
                let dn: String = url::form_urlencoded::byte_serialize(title.as_bytes()).collect();
                magnet.push_str(&format!("&dn={dn}"));
            }
            return Err(IndexerError::MagnetRedirect(magnet));
        }
        if let Some(d) = dl.filter(|d| !d.selectors.is_empty()) {
            let page = self
                .request(&mut s, Method::GET, target.clone(), None, &headers)
                .await?;
            let now = Utc::now();
            let link = {
                let doc = Html::parse_document(&page.body);
                d.selectors.iter().find_map(|f| {
                    html_value(doc.root_element(), f, &ctx)
                        .and_then(|v| filters::apply(v, &f.filters, &ctx, now).ok())
                        .filter(|v| !v.trim().is_empty())
                })
            }
            .ok_or_else(|| IndexerError::Parse("download link not found on details page".into()))?;
            if link.starts_with("magnet:") {
                return Err(IndexerError::MagnetRedirect(link));
            }
            target = page
                .url
                .join(link.trim())
                .map_err(|e| IndexerError::Parse(e.to_string()))?;
        }
        let method = match dl.and_then(|d| d.method.as_deref()) {
            Some(m) if m.eq_ignore_ascii_case("post") => Method::POST,
            _ => Method::GET,
        };
        let (_, resp) = if method == Method::POST {
            let body = target.query().unwrap_or_default().to_string();
            let mut t = target.clone();
            t.set_query(None);
            self.send(&mut s, method, t, Some(body), &headers).await?
        } else {
            self.send(&mut s, method, target, None, &headers).await?
        };
        if let Some(e) = status_error(&resp) {
            return Err(e);
        }
        let bytes = crate::http::read_limited(resp, crate::http::MAX_DOWNLOAD_BYTES).await?;
        if bytes.is_empty() {
            return Err(IndexerError::Parse("empty download".into()));
        }
        Ok(bytes)
    }
}

/// Variables every template sees besides `.Config.*`.
pub(super) fn base_vars(c: &mut Context, now: DateTime<Utc>) {
    c.set("Today.Year", now.year().to_string());
    c.set("Today.Month", now.month().to_string());
    c.set("Today.Day", now.day().to_string());
    c.set("True", "True");
    c.set("False", TValue::Null);
}

fn parse_num(v: &str) -> Option<f64> {
    let t: String = v.chars().filter(|c| !c.is_whitespace() && *c != ',').collect();
    if let Ok(n) = t.parse::<f64>() {
        return Some(n);
    }
    // Leading number of strings such as `12 seeders` or `1.5x`.
    let lead: String = t
        .chars()
        .skip_while(|c| !c.is_ascii_digit() && *c != '-')
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    lead.parse().ok()
}

/// Cardigann's `andmatch` row filter: every query word must appear in the title.
fn and_match(title: &str, query: &str, f: &FilterDef) -> bool {
    let mut query = query.to_string();
    if let Some(limit) = f
        .args()
        .first()
        .and_then(|a| a.trim().parse::<usize>().ok())
        .filter(|&l| l > 0)
        && limit < query.chars().count()
    {
        query = query.chars().take(limit).collect();
    }
    const COMMON: &[&str] = &["and", "the", "an", "a", "of"];
    let split = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let title_words = split(title);
    split(&query)
        .iter()
        .filter(|w| !COMMON.contains(&w.as_str()))
        .all(|w| title_words.contains(w))
}

/// Runs every field rule against one row; `None` when a required field is missing.
fn extract_fields(
    fields: &OrderedMap<FieldDef>,
    ctx: &Context,
    now: DateTime<Utc>,
    value: impl Fn(&FieldDef, &Context) -> Option<String>,
) -> Option<Vec<(String, String)>> {
    let mut row_ctx = ctx.clone();
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, f) in fields.iter() {
        let mut parts = name.split('|');
        let key = parts.next().unwrap_or(name);
        let mods: Vec<&str> = parts.collect();
        let optional = f.optional || mods.contains(&"optional") || OPTIONAL_FIELDS.contains(&key);
        let raw = match &f.text {
            Some(t) => render(&t.0, &row_ctx).ok(),
            None => value(f, &row_ctx),
        };
        let val = match raw.map(|v| filters::apply(v, &f.filters, &row_ctx, now)) {
            Some(Ok(v)) if !v.is_empty() || f.default.is_none() => Some(v),
            Some(Err(e)) => {
                tracing::debug!(field = key, error = %e, "definition field filter failed");
                None
            }
            _ => None,
        };
        let mut val = match val {
            Some(v) => v,
            None => match &f.default {
                Some(d) => render(&d.0, &row_ctx).unwrap_or_default(),
                None if optional => String::new(),
                None => {
                    tracing::debug!(field = key, "required field missing; skipping row");
                    return None;
                }
            },
        };
        if mods.contains(&"append")
            && let Some(prev) = out.iter().rev().find(|e| e.0 == key)
        {
            val = format!("{}{val}", prev.1);
        }
        if key == "category" && mods.contains(&"noappend") {
            out.push((key.to_string(), "\u{0}noappend".into()));
        }
        row_ctx.set(
            format!("Result.{key}"),
            if val.is_empty() {
                TValue::Null
            } else {
                TValue::Str(val.clone())
            },
        );
        out.push((key.to_string(), val));
    }
    Some(out)
}

/// Renders a (possibly templated) selector and compiles it (cached by rendered string).
fn sel(src: &str, ctx: &Context) -> Result<Arc<Selector>, String> {
    let rendered = render(src, ctx).map_err(|e| e.to_string())?;
    selector::compile(rendered.trim())
}

/// Element selected by a field rule: the row itself without a selector, else the row when it
/// matches or its first matching descendant (Cardigann's `HandleSelector`).
fn select_el<'a>(row: ElementRef<'a>, f: &FieldDef, ctx: &Context) -> Option<ElementRef<'a>> {
    match &f.selector {
        Some(s) => sel(s, ctx).ok()?.first_self_or_descendant(row),
        None => Some(row),
    }
}

/// Raw (unfiltered) value of a field rule against an HTML row.
fn html_value(row: ElementRef<'_>, f: &FieldDef, ctx: &Context) -> Option<String> {
    let el = select_el(row, f, ctx)?;
    if !f.case.is_empty() {
        for (k, v) in f.case.iter() {
            let hit = k == "*" || sel(k, ctx).is_ok_and(|s| s.first_self_or_descendant(el).is_some());
            if hit {
                return render(&v.0, ctx).ok();
            }
        }
        return None;
    }
    match &f.attribute {
        Some(a) => el
            .value()
            .attrs()
            .find(|(k, _)| k.eq_ignore_ascii_case(a))
            .map(|(_, v)| v.to_string()),
        None => Some(element_text(el, f.remove.as_deref(), ctx)),
    }
}

/// Raw value of a field rule against a JSON row (`..x` reads the parent row in `multiple` mode).
fn json_value(row: &Value, parent: Option<&Value>, f: &FieldDef, ctx: &Context) -> Option<String> {
    let src = render(f.selector.as_deref()?, ctx).ok()?;
    let (target, path) = match (src.strip_prefix(".."), parent) {
        (Some(rest), Some(p)) => (p, rest.trim_start_matches('.')),
        _ => (row, src.trim_start_matches('.')),
    };
    let v = jsonsel::select(target, path).and_then(jsonsel::field_value)?;
    if f.case.is_empty() {
        return Some(v);
    }
    let is_bool = |s: &str| s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false");
    f.case
        .iter()
        .find(|(k, _)| *k == "*" || *k == v || (is_bool(k) && k.eq_ignore_ascii_case(&v)))
        .and_then(|(_, val)| render(&val.0, ctx).ok())
}

/// Walks back from `row` (previous siblings, then the parent's previous siblings) to the first
/// element yielding a date header value.
fn date_header(row: ElementRef<'_>, dh: &FieldDef, ctx: &Context, now: DateTime<Utc>) -> Option<String> {
    fn prev(e: ElementRef<'_>) -> Option<ElementRef<'_>> {
        e.prev_siblings().find_map(ElementRef::wrap).or_else(|| {
            e.parent()
                .and_then(ElementRef::wrap)
                .and_then(|p| p.prev_siblings().find_map(ElementRef::wrap))
        })
    }
    let mut cur = prev(row);
    while let Some(c) = cur {
        if let Some(v) = html_value(c, dh, ctx).and_then(|v| filters::apply(v, &dh.filters, ctx, now).ok())
            && let Some(d) = filters::from_unknown(&v, now)
        {
            return Some(d.to_rfc3339());
        }
        cur = prev(c);
    }
    None
}

/// Element text with whitespace collapsed, skipping descendants matching `remove`.
fn element_text(el: ElementRef<'_>, remove: Option<&str>, ctx: &Context) -> String {
    let remove = remove.and_then(|r| sel(r, ctx).ok());
    let mut parts: Vec<&str> = Vec::new();
    for n in el.descendants() {
        let Some(t) = n.value().as_text() else { continue };
        if let Some(rm) = &remove {
            let removed = n
                .ancestors()
                .take_while(|a| a.id() != el.id())
                .filter_map(ElementRef::wrap)
                .any(|a| rm.matches(a, Some(el)));
            if removed {
                continue;
            }
        }
        parts.push(t);
    }
    parts.concat().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_has(body: &str, s: &str, ctx: &Context) -> bool {
    let doc = Html::parse_document(body);
    sel(s, ctx).is_ok_and(|s| s.first(doc.tree.root(), None).is_some())
}

fn first_error(doc: &Html, defs: &[ErrorDef], ctx: &Context) -> Option<String> {
    let now = Utc::now();
    for d in defs {
        let Ok(s) = sel(&d.selector, ctx) else { continue };
        if let Some(el) = s.first(doc.tree.root(), None) {
            let msg = match &d.message {
                Some(m) => {
                    let raw = match &m.text {
                        Some(t) => render(&t.0, ctx).ok(),
                        None => html_value(doc.root_element(), m, ctx),
                    };
                    raw.map(|v| filters::apply(v.clone(), &m.filters, ctx, now).unwrap_or(v))
                        .unwrap_or_default()
                }
                None => element_text(el, None, ctx),
            };
            return Some(if msg.is_empty() { "error".into() } else { msg });
        }
    }
    None
}

fn check_errors(body: &str, defs: &[ErrorDef], ctx: &Context) -> Result<(), String> {
    if defs.is_empty() {
        return Ok(());
    }
    match first_error(&Html::parse_document(body), defs, ctx) {
        Some(m) => Err(m),
        None => Ok(()),
    }
}

struct LoginForm {
    action: String,
    method: String,
    fields: Vec<(String, String)>,
    /// `getselectorinputs`, sent in the submit URL's query string.
    query: Vec<(String, String)>,
}

/// Reads the login form, merges the definition's inputs and the scraped `selectorinputs`.
fn parse_form(
    body: &str,
    form_sel: &str,
    ctx: &Context,
    login: &super::model::LoginDef,
    pairs: &[(String, String)],
) -> Result<LoginForm, String> {
    let doc = Html::parse_document(body);
    if let Some(c) = &login.captcha
        && !c.selector.is_empty()
        && sel(&c.selector, ctx).is_ok_and(|s| s.first(doc.tree.root(), None).is_some())
    {
        return Err(format!(
            "unsupported: the login page shows a {} captcha, which Reelix cannot solve; \
             log in with a cookie instead if the site allows it",
            if c.kind.is_empty() {
                "login"
            } else {
                c.kind.as_str()
            }
        ));
    }
    let form = sel(form_sel, ctx)?
        .first(doc.tree.root(), None)
        .ok_or_else(|| format!("login form `{form_sel}` not found"))?;
    let inputs = selector::compile("input[name], textarea[name], select[name]")?;
    let mut fields = Vec::new();
    for i in inputs.select(*form, None) {
        let v = i.value();
        let ty = v.attr("type").unwrap_or("text").to_ascii_lowercase();
        if matches!(ty.as_str(), "submit" | "button" | "image" | "file" | "reset") {
            continue;
        }
        if matches!(ty.as_str(), "checkbox" | "radio") && v.attr("checked").is_none() {
            continue;
        }
        let name = v.attr("name").unwrap_or_default().to_string();
        let value = match v.name() {
            "textarea" => i.text().collect(),
            "select" => {
                let opt = selector::compile("option[selected], option")?;
                opt.first(*i, None)
                    .map(|o| {
                        o.value()
                            .attr("value")
                            .map_or_else(|| o.text().collect(), str::to_string)
                    })
                    .unwrap_or_default()
            }
            _ => v.attr("value").unwrap_or_default().to_string(),
        };
        fields.push((name, value));
    }
    let set =
        |fields: &mut Vec<(String, String)>, k: String, v: String| match fields.iter_mut().find(|f| f.0 == k)
        {
            Some(f) => f.1 = v,
            None => fields.push((k, v)),
        };
    for (k, v) in pairs {
        let name = if login.selectors {
            let el = sel(k, ctx)?
                .first(*form, Some(form))
                .ok_or_else(|| format!("login failed: no input found using selector `{k}`"))?;
            el.value().attr("name").unwrap_or_default().to_string()
        } else {
            k.clone()
        };
        set(&mut fields, name, v.clone());
    }
    let now = Utc::now();
    let scrape = |f: &FieldDef, name: &str| -> Result<Option<String>, String> {
        match html_value(doc.root_element(), f, ctx).map(|v| filters::apply(v, &f.filters, ctx, now)) {
            Some(Ok(v)) => Ok(Some(v)),
            _ if f.optional => Ok(None),
            Some(Err(e)) => Err(format!("login selector input `{name}`: {e}")),
            None => Err(format!(
                "login selector input `{name}` not found on the login page"
            )),
        }
    };
    for (name, f) in login.selectorinputs.iter() {
        if let Some(v) = scrape(f, name)? {
            set(&mut fields, name.to_string(), v);
        }
    }
    let mut query = Vec::new();
    for (name, f) in login.getselectorinputs.iter() {
        if let Some(v) = scrape(f, name)? {
            query.push((name.to_string(), v));
        }
    }
    Ok(LoginForm {
        action: form.value().attr("action").unwrap_or("").to_string(),
        method: form.value().attr("method").unwrap_or("post").to_string(),
        fields,
        query,
    })
}

/// Builds a scraper tree from XML (RSS/Torznab-style responses) so CSS selectors work on it;
/// HTML parsing would mangle elements such as `<link>`.
fn parse_xml(body: &str) -> Result<Html, String> {
    use html5ever::tendril::StrTendril;
    use html5ever::{LocalName, Namespace, QualName};
    use quick_xml::events::Event;
    let qual = |n: &[u8]| {
        QualName::new(
            None,
            Namespace::from(""),
            LocalName::from(String::from_utf8_lossy(n).as_ref()),
        )
    };
    let mut html = Html::new_document();
    let mut stack = vec![html.tree.root().id()];
    let mut reader = quick_xml::Reader::from_str(body);
    let push = |html: &mut Html, stack: &[ego_tree::NodeId], node: Node| -> Option<ego_tree::NodeId> {
        let parent = *stack.last()?;
        Some(html.tree.get_mut(parent)?.append(node).id())
    };
    loop {
        let ev = reader.read_event().map_err(|e| format!("XML: {e}"))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let attrs = e
                    .attributes()
                    .filter_map(Result::ok)
                    .map(|a| html5ever::Attribute {
                        name: qual(a.key.as_ref()),
                        value: StrTendril::from(
                            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .map(|v| v.into_owned())
                                .unwrap_or_default(),
                        ),
                    })
                    .collect();
                let el = scraper::node::Element::new(qual(e.name().as_ref()), attrs);
                let id = push(&mut html, &stack, Node::Element(el));
                if matches!(ev, Event::Start(_))
                    && let Some(id) = id
                {
                    stack.push(id);
                }
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            Event::Text(_) | Event::GeneralRef(_) => {
                let s = match &ev {
                    Event::Text(t) => t.xml10_content().map(|v| v.into_owned()).unwrap_or_default(),
                    Event::GeneralRef(r) => crate::xml::reference_text(r),
                    _ => unreachable!(),
                };
                push(
                    &mut html,
                    &stack,
                    Node::Text(scraper::node::Text {
                        text: s.as_str().into(),
                    }),
                );
            }
            Event::CData(t) => {
                let s = String::from_utf8_lossy(&t.into_inner()).into_owned();
                push(
                    &mut html,
                    &stack,
                    Node::Text(scraper::node::Text {
                        text: s.as_str().into(),
                    }),
                );
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(html)
}

/// Decodes a body with the named charset, falling back to lossy UTF-8.
fn decode(bytes: &[u8], label: &str) -> String {
    let enc = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    enc.decode(bytes).0.into_owned()
}

/// Form-encodes pairs in the site's charset (e.g. windows-1251 trackers).
fn encode_with(pairs: &[(String, String)], label: &str) -> String {
    let enc = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    if enc == encoding_rs::UTF_8 {
        return url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
    }
    let e = |s: &str| -> String {
        let (bytes, _, _) = enc.encode(s);
        url::form_urlencoded::byte_serialize(&bytes).collect()
    };
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", e(k), e(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn append_query(url: &mut Url, qs: &str) {
    if qs.is_empty() {
        return;
    }
    let q = match url.query() {
        Some(existing) if !existing.is_empty() => format!("{existing}&{qs}"),
        _ => qs.to_string(),
    };
    url.set_query(Some(&q));
}

fn build_caps(def: &Definition) -> (IndexerCapabilities, Vec<(String, u32)>) {
    let mut cats = Vec::new();
    let mut add = |sid: &str, name: &str| match categories::by_name(name).or_else(|| name.parse().ok()) {
        Some(nz) => cats.push((sid.to_string(), nz)),
        None => {
            tracing::warn!(definition = %def.id, category = name, "unknown Newznab category in definition")
        }
    };
    for m in &def.caps.categorymappings {
        add(&m.id.0, &m.cat);
    }
    for (sid, name) in def.caps.categories.iter() {
        add(sid, &name.0);
    }
    let mut caps = IndexerCapabilities::default();
    for (mode, params) in def.caps.modes.iter() {
        let m = SearchMode {
            available: true,
            params: SearchParam::parse_list(&params.join(",")),
        };
        match mode {
            "search" => caps.search = m,
            "tv-search" => caps.tv_search = m,
            "movie-search" => caps.movie_search = m,
            "book-search" => caps.book_search = m,
            "music-search" | "audio-search" => caps.audio_search = m,
            _ => {}
        }
    }
    let mut tree: Vec<Category> = Vec::new();
    for &(_, nz) in &cats {
        let p = categories::parent(nz);
        let idx = match tree.iter().position(|c| c.id == p) {
            Some(i) => i,
            None => {
                tree.push(Category::new(
                    p,
                    &categories::name(p).unwrap_or_else(|| p.to_string()),
                ));
                tree.len() - 1
            }
        };
        if nz != p && !tree[idx].subcategories.iter().any(|c| c.id == nz) {
            tree[idx].subcategories.push(Category::new(
                nz,
                &categories::name(nz).unwrap_or_else(|| nz.to_string()),
            ));
        }
    }
    caps.categories = tree;
    (caps, cats)
}

/// Contexts used to validate templated selectors at load time: setting defaults, and the same
/// with every checkbox ticked (so both sides of `{{ if .Config.x }}` get compiled).
fn validation_contexts(d: &Definition) -> Vec<Context> {
    let mut a = Context::default();
    let mut b = Context::default();
    a.set("Config.sitelink", d.links[0].clone());
    b.set("Config.sitelink", d.links[0].clone());
    for s in &d.settings {
        let def = s.default.as_ref().map(super::yaml_to_json).unwrap_or(Value::Null);
        let v = match (s.kind.as_str(), &def) {
            ("checkbox", Value::Bool(x)) => TValue::Bool(*x),
            ("checkbox", _) => TValue::Bool(false),
            ("multi-select", Value::Array(x)) => TValue::List(x.iter().map(jsonsel::to_text).collect()),
            (_, Value::Null) => TValue::Str(String::new()),
            (_, v) => TValue::Str(jsonsel::to_text(v)),
        };
        let flipped = if s.kind == "checkbox" {
            TValue::Bool(true)
        } else {
            v.clone()
        };
        a.set(format!("Config.{}", s.name), v);
        b.set(format!("Config.{}", s.name), flipped);
    }
    let now = Utc::now();
    base_vars(&mut a, now);
    base_vars(&mut b, now);
    vec![a, b]
}

/// Compiles every CSS selector, template and filter so bad definitions fail at load time with a
/// clear message (unsupported features are reported as `unsupported: …`).
pub(super) fn validate(d: &Definition) -> Result<(), String> {
    let ctxs = validation_contexts(d);
    let css = |s: &str| -> Result<(), String> {
        super::template::check(s).map_err(|e| format!("selector `{s}`: {e}"))?;
        for c in &ctxs {
            let r = render(s, c).map_err(|e| format!("selector `{s}`: {e}"))?;
            if r.trim().is_empty() && s.contains("{{") {
                continue;
            }
            selector::compile(r.trim())?;
        }
        Ok(())
    };
    let tpl = |s: &str| super::template::check(s).map_err(|e| format!("template `{s}`: {e}"));
    let filters_ok = |fs: &[FilterDef]| -> Result<(), String> {
        for f in fs {
            filters::check(f)?;
            for a in f.args() {
                tpl(&a)?;
            }
        }
        Ok(())
    };
    let field = |f: &FieldDef, html: bool| -> Result<(), String> {
        if let Some(s) = &f.selector {
            if html {
                css(s)?;
            } else {
                tpl(s)?;
            }
        }
        if let Some(r) = &f.remove {
            css(r)?;
        }
        for (k, v) in f.case.iter() {
            if html && k != "*" {
                css(k)?;
            }
            tpl(&v.0)?;
        }
        if let Some(t) = &f.text {
            tpl(&t.0)?;
        }
        if let Some(t) = &f.default {
            tpl(&t.0)?;
        }
        filters_ok(&f.filters)
    };
    let formats: Vec<String> = d
        .search
        .paths
        .iter()
        .map(|p| {
            p.response
                .as_ref()
                .map(|r| r.kind.to_ascii_lowercase())
                .unwrap_or_default()
        })
        .collect();
    let all_json = !formats.is_empty() && formats.iter().all(|f| f == "json");
    let any_json = formats.iter().any(|f| f == "json");
    if all_json {
        tpl(&d.search.rows.selector)?;
    } else {
        css(&d.search.rows.selector)?;
    }
    for (_, f) in d.search.fields.iter() {
        // Mixed HTML/JSON definitions don't exist in practice; validate as CSS unless all JSON.
        field(f, !all_json)?;
    }
    if let Some(c) = &d.search.rows.count {
        field(c, !any_json)?;
    }
    if let Some(dh) = &d.search.rows.dateheaders {
        field(dh, true)?;
    }
    filters_ok(&d.search.rows.filters)?;
    filters_ok(&d.search.keywordsfilters)?;
    filters_ok(&d.search.preprocessingfilters)?;
    for p in &d.search.paths {
        tpl(&p.path)?;
        for (_, v) in p.inputs.iter() {
            tpl(&v.0)?;
        }
    }
    for (_, v) in d.search.inputs.iter() {
        tpl(&v.0)?;
    }
    for (_, vs) in d.search.headers.iter() {
        vs.0.iter().try_for_each(|v| tpl(v))?;
    }
    for e in d
        .search
        .error
        .iter()
        .chain(d.login.iter().flat_map(|l| l.error.iter()))
    {
        css(&e.selector)?;
        if let Some(m) = &e.message {
            field(m, true)?;
        }
    }
    if let Some(l) = &d.login {
        let method = l.method.as_deref().unwrap_or("post").to_ascii_lowercase();
        if !matches!(method.as_str(), "post" | "form" | "get" | "cookie" | "oneurl") {
            return Err(format!("unsupported: login method `{method}`"));
        }
        if let Some(t) = l.test.as_ref().and_then(|t| t.selector.as_ref()) {
            css(t)?;
        }
        if let Some(f) = &l.form {
            css(f)?;
        }
        if let Some(c) = &l.captcha {
            css(&c.selector)?;
        }
        for (k, v) in l.inputs.iter() {
            if l.selectors {
                css(k)?;
            }
            tpl(&v.0)?;
        }
        for (_, f) in l.selectorinputs.iter().chain(l.getselectorinputs.iter()) {
            field(f, true)?;
        }
    }
    if let Some(dl) = &d.download {
        for f in &dl.selectors {
            field(f, true)?;
        }
        if let Some(ih) = &dl.infohash {
            field(&ih.hash, true)?;
            if let Some(t) = &ih.title {
                field(t, true)?;
            }
        }
        if let Some(b) = &dl.before {
            tpl(&b.path)?;
            for (_, v) in b.inputs.iter() {
                tpl(&v.0)?;
            }
        }
    }
    Ok(())
}

#[async_trait]
impl Indexer for DefinitionIndexer {
    fn protocol(&self) -> Protocol {
        self.protocol
    }

    fn owns_url(&self, url: &Url) -> bool {
        self.own_host(url)
    }

    async fn capabilities(&self) -> Result<IndexerCapabilities, IndexerError> {
        Ok(self.caps.clone())
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError> {
        if !self.serves(query) {
            return Ok(Vec::new());
        }
        let mut out = self.run_search(query).await?;
        categories::filter_excluded(query, &self.categories_for(query), &mut out);
        Ok(out)
    }

    async fn rss(&self, categories: &[u32]) -> Result<Vec<ReleaseInfo>, IndexerError> {
        self.run_search(&SearchQuery {
            categories: categories.to_vec(),
            ..SearchQuery::default()
        })
        .await
    }

    async fn test(&self) -> Result<(), IndexerError> {
        {
            let mut s = self.session.lock().await;
            s.logged_in = false;
            self.login(&mut s).await?;
        }
        self.rss(&[]).await.map(|_| ())
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, IndexerError> {
        self.do_download(url).await
    }
}

/// Method and body for the next hop of a redirect: 301–303 switch to a body-less GET; 307/308
/// keep both, but only towards the tracker itself (a form body such as login credentials is
/// never replayed to a foreign host).
fn redirect_request(
    status: u16,
    method: Method,
    body: Option<String>,
    next_is_own: bool,
) -> (Method, Option<String>) {
    if matches!(status, 301..=303) || (body.is_some() && !next_is_own) {
        (Method::GET, None)
    } else {
        (method, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_drop_body_towards_foreign_hosts() {
        let body = || Some("user=a&pass=b".to_string());
        assert_eq!(
            redirect_request(307, Method::POST, body(), true),
            (Method::POST, body())
        );
        assert_eq!(
            redirect_request(308, Method::POST, body(), false),
            (Method::GET, None)
        );
        assert_eq!(
            redirect_request(307, Method::POST, body(), false),
            (Method::GET, None)
        );
        assert_eq!(
            redirect_request(302, Method::POST, body(), true),
            (Method::GET, None)
        );
        assert_eq!(
            redirect_request(307, Method::GET, None, false),
            (Method::GET, None)
        );
    }

    #[test]
    fn xml_documents_keep_rss_elements() {
        let xml = r#"<?xml version="1.0"?><rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
            <item><title>A &amp; B</title><link>https://x.invalid/dl/1</link><pubDate>Tue, 28 Apr 2026 07:15:00 +0000</pubDate>
            <enclosure url="https://x.invalid/1.torrent" length="10"/><torznab:attr name="seeders" value="5"/>
            <description><![CDATA[<b>bold</b>]]></description></item></channel></rss>"#;
        let doc = parse_xml(xml).unwrap();
        let ctx = Context::default();
        let item = selector::compile("rss > channel > item")
            .unwrap()
            .first(doc.tree.root(), None)
            .unwrap();
        let f = |sel: &str, attr: Option<&str>| FieldDef {
            selector: Some(sel.into()),
            attribute: attr.map(str::to_string),
            ..FieldDef::default()
        };
        assert_eq!(
            html_value(item, &f("title", None), &ctx).as_deref(),
            Some("A & B")
        );
        assert_eq!(
            html_value(item, &f("link", None), &ctx).as_deref(),
            Some("https://x.invalid/dl/1")
        );
        assert_eq!(
            html_value(item, &f("pubDate", None), &ctx).as_deref(),
            Some("Tue, 28 Apr 2026 07:15:00 +0000")
        );
        assert_eq!(
            html_value(item, &f("enclosure", Some("url")), &ctx).as_deref(),
            Some("https://x.invalid/1.torrent")
        );
        assert_eq!(
            html_value(item, &f("[name=seeders]", Some("value")), &ctx).as_deref(),
            Some("5")
        );
        assert_eq!(
            html_value(item, &f("torznab\\:attr", Some("name")), &ctx).as_deref(),
            Some("seeders")
        );
        assert_eq!(
            html_value(item, &f("description", None), &ctx).as_deref(),
            Some("<b>bold</b>")
        );
        assert!(parse_xml("<a><b></a>").is_err());
    }

    #[test]
    fn andmatch_requires_every_word() {
        let f = FilterDef {
            name: "andmatch".into(),
            args: None,
        };
        let q = "The Matrix reloaded";
        assert!(and_match("Matrix.Reloaded.2003.1080p", q, &f));
        assert!(!and_match("Matrix.Revolutions.2003", q, &f));
        let limited = FilterDef {
            name: "andmatch".into(),
            args: Some(yaml_serde::Value::Number(10.into())),
        };
        assert!(
            and_match("Matrix 1999", q, &limited),
            "only the first 10 chars count"
        );
    }

    #[test]
    fn site_encoding_for_queries() {
        let pairs = vec![("q".to_string(), "фильм 1".to_string())];
        assert_eq!(encode_with(&pairs, "windows-1251"), "q=%F4%E8%EB%FC%EC+1");
        assert_eq!(encode_with(&pairs, "UTF-8"), "q=%D1%84%D0%B8%D0%BB%D1%8C%D0%BC+1");
    }

    #[test]
    fn field_modifiers_and_implicit_optional_fields() {
        let fields: OrderedMap<FieldDef> = yaml_serde::from_str(
            r#"
category: { text: "1" }
"category|noappend": { text: "2" }
title: { text: "Base" }
"title|append": { text: " Extra" }
poster: { selector: img }
_flag: { selector: missing, optional: true }
description: { text: "{{ if .Result._flag }}flagged{{ else }}plain{{ end }} {{ .Result.title }}" }
"#,
        )
        .unwrap();
        let now = Utc::now();
        let m = extract_fields(&fields, &Context::default(), now, |_, _| None).unwrap();
        let get = |k: &str| m.iter().rev().find(|e| e.0 == k).map(|e| e.1.as_str());
        assert_eq!(get("title"), Some("Base Extra"));
        assert_eq!(get("poster"), Some(""), "poster is implicitly optional");
        assert_eq!(get("description"), Some("plain Base Extra"));
        let cats: Vec<&str> = m
            .iter()
            .filter(|e| e.0 == "category")
            .map(|e| e.1.as_str())
            .collect();
        assert_eq!(cats, ["1", "\u{0}noappend", "2"]);
        let required: OrderedMap<FieldDef> = yaml_serde::from_str("title: { selector: h1 }").unwrap();
        assert!(extract_fields(&required, &Context::default(), now, |_, _| None).is_none());
    }

    #[test]
    fn templated_selectors_are_validated_for_both_checkbox_states() {
        let yaml = |sel: &str| {
            format!(
                "id: t\nname: T\nlinks: [https://t.invalid/]\nsettings:\n  - {{ name: fl, type: checkbox }}\n\
                 search:\n  path: s\n  rows:\n    selector: 'tr{sel}'\n  fields:\n    title: {{ selector: a }}\n    download: {{ selector: a }}\n"
            )
        };
        assert!(super::super::parse(&yaml("{{ if .Config.fl }}:has(img.free){{ else }}{{ end }}")).is_ok());
        let err = super::super::parse(&yaml("{{ if .Config.fl }}:bogus{{ else }}{{ end }}")).unwrap_err();
        assert!(err.contains("unsupported pseudo-class `:bogus`"), "{err}");
    }

    #[test]
    fn numbers_are_lenient() {
        assert_eq!(parse_num("1,234"), Some(1234.0));
        assert_eq!(parse_num("12 seeders"), Some(12.0));
        assert_eq!(parse_num("x"), None);
    }
}
