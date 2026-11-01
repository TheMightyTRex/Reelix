//! Torznab/Newznab aggregator: other apps (Sonarr, Radarr, Lidarr, Readarr, LazyLibrarian…)
//! can add Reelix as one indexer and search every indexer configured here.
//!
//! - `/api/torznab/all/api` searches the enabled torrent indexers, `/api/newznab/all/api` the
//!   Usenet ones; `/api/{torznab|newznab}/{id}/api` searches a single indexer.
//! - Authenticated with the API key (`?apikey=` or `X-Api-Key`) of a user with MANAGE_LIBRARY.
//! - Download links point back at Reelix (`.../dl?link=`) so indexer credentials and cookies
//!   never leave this server. The link is sealed with the server's secret key, so it can only
//!   fetch URLs that an indexer search returned.

// Helpers return a ready Newznab error `Response` as their error; it's built once per request.
#![allow(clippy::result_large_err)]

use std::fmt::Write as _;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::Utc;
use reelix_core::{Permissions, Protocol};
use reelix_indexers::{Category, IndexerError, ReleaseInfo, SearchKind, SearchQuery, categories};
use reelix_providers::ProviderService;
use serde::Deserialize;

use crate::auth::MaybeUser;
use crate::state::AppState;

const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1000;
/// Download links stay valid this long (long enough for delayed grabs).
const LINK_TTL_SECS: i64 = 30 * 24 * 3600;
const LINK_PURPOSE: &str = "torznab-dl";
const CAPS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/torznab/{target}/api", get(api_torrent))
        .route("/torznab/{target}", get(api_torrent))
        .route("/newznab/{target}/api", get(api_usenet))
        .route("/newznab/{target}", get(api_usenet))
        .route("/torznab/{target}/dl", get(download))
        .route("/newznab/{target}/dl", get(download))
}

/// Torznab query parameters (names as in the spec).
#[derive(Debug, Default, Deserialize)]
pub struct TorznabParams {
    t: Option<String>,
    q: Option<String>,
    cat: Option<String>,
    imdbid: Option<String>,
    tmdbid: Option<String>,
    tvdbid: Option<String>,
    season: Option<String>,
    ep: Option<String>,
    year: Option<String>,
    author: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
    apikey: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DownloadParams {
    link: String,
}

/// A Newznab error document (`<error code=".." description=".."/>`).
fn error(status: StatusCode, code: u32, description: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"{code}\" description=\"{}\"/>\n",
        esc(description)
    );
    xml(status, body)
}

fn xml(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Escapes text for XML content and attribute values (dropping characters XML 1.0 forbids).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

fn svc(state: &AppState) -> Result<Arc<ProviderService>, Response> {
    state
        .try_ext::<ProviderService>()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, 900, "indexers are not available"))
}

fn authorize(user: &MaybeUser) -> Result<(), Response> {
    match &user.0 {
        None => Err(error(StatusCode::UNAUTHORIZED, 100, "Invalid API key")),
        Some(u) if u.require(Permissions::MANAGE_LIBRARY).is_err() => Err(error(
            StatusCode::FORBIDDEN,
            101,
            "This API key's user can't search indexers (needs Manage library)",
        )),
        Some(_) => Ok(()),
    }
}

/// Indexers a target covers: every enabled indexer of the protocol (`all`) or one by id.
fn indexer_ids(svc: &ProviderService, protocol: Protocol, target: &str) -> Result<Vec<i64>, Response> {
    let indexers = svc.indexers();
    let matching = |id: Option<i64>| -> Vec<i64> {
        indexers
            .iter()
            .filter(|(row, ix)| ix.protocol() == protocol && id.is_none_or(|id| row.id == id))
            .map(|(row, _)| row.id)
            .collect()
    };
    if target.eq_ignore_ascii_case("all") {
        return Ok(matching(None));
    }
    let id: i64 = target
        .parse()
        .map_err(|_| error(StatusCode::NOT_FOUND, 201, "Unknown indexer"))?;
    let ids = matching(Some(id));
    if ids.is_empty() {
        let other = if protocol == Protocol::Torrent {
            "newznab"
        } else {
            "torznab"
        };
        let elsewhere = indexers.iter().any(|(row, _)| row.id == id);
        let msg = if elsewhere {
            format!("Indexer {id} uses the other protocol; use /api/{other}/{id}/api")
        } else {
            format!("Indexer {id} not found or disabled")
        };
        return Err(error(StatusCode::NOT_FOUND, 201, &msg));
    }
    Ok(ids)
}

async fn api_torrent(
    state: State<AppState>,
    user: MaybeUser,
    Path(target): Path<String>,
    headers: HeaderMap,
    Query(p): Query<TorznabParams>,
) -> Response {
    api(state, user, target, headers, p, Protocol::Torrent).await
}

async fn api_usenet(
    state: State<AppState>,
    user: MaybeUser,
    Path(target): Path<String>,
    headers: HeaderMap,
    Query(p): Query<TorznabParams>,
) -> Response {
    api(state, user, target, headers, p, Protocol::Usenet).await
}

async fn api(
    State(state): State<AppState>,
    user: MaybeUser,
    target: String,
    headers: HeaderMap,
    p: TorznabParams,
    protocol: Protocol,
) -> Response {
    let t = p.t.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    // Caps is public in the spec, but some clients send it without a key; ours still needs one
    // so the endpoint doesn't reveal which indexers exist.
    if let Err(r) = authorize(&user) {
        return r;
    }
    let svc = match svc(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let ids = match indexer_ids(&svc, protocol, &target) {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let kind = match t.as_str() {
        "caps" => return caps(&svc, &ids).await,
        "search" => SearchKind::Basic,
        "tvsearch" | "tv" => SearchKind::Tv,
        "movie" | "movie-search" => SearchKind::Movie,
        "book" | "book-search" => SearchKind::Book,
        "music" | "audio" | "music-search" | "audio-search" => SearchKind::Music,
        "" => return error(StatusCode::BAD_REQUEST, 200, "Missing parameter t"),
        _ => return error(StatusCode::BAD_REQUEST, 202, "No such function"),
    };
    let query = match query(kind, &p) {
        Ok(q) => q,
        Err(msg) => return error(StatusCode::BAD_REQUEST, 201, &msg),
    };
    let base = base_url(&headers, &state);
    let apikey = apikey(&p, &headers);
    let secrets = state.secrets();
    let seal = |plain: &str| secrets.encrypt(plain).ok();
    let feed = Feed {
        base: &base,
        target: &target,
        protocol,
        ns: if protocol == Protocol::Torrent {
            "torznab"
        } else {
            "newznab"
        },
        apikey: &apikey,
        seal: &seal,
    };
    if ids.is_empty() {
        return xml(StatusCode::OK, rss(&[], &feed));
    }
    let results = svc.search(&query, Some(&ids), true).await;
    let mut releases: Vec<ReleaseInfo> = Vec::new();
    let mut failures = Vec::new();
    for r in results {
        match r.result {
            Ok(list) => releases.extend(list.into_iter().filter(|r| r.protocol == protocol)),
            Err(e) => failures.push(format!("{}: {e}", r.name)),
        }
    }
    // A single indexer that failed is an error for the client; partial failures aren't.
    if releases.is_empty() && !failures.is_empty() && failures.len() == ids.len() {
        return error(StatusCode::OK, 900, &failures.join("; "));
    }
    releases.sort_by_key(|r| std::cmp::Reverse(r.publish_date));
    dedupe(&mut releases);
    let offset = query.offset.unwrap_or(0) as usize;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT) as usize;
    let page: Vec<ReleaseInfo> = releases.into_iter().skip(offset).take(limit).collect();
    xml(StatusCode::OK, rss(&page, &feed))
}

/// Drops repeats of the same release returned by several indexers (same info hash, or same
/// title and size), keeping the first (newest/highest-priority) one.
fn dedupe(releases: &mut Vec<ReleaseInfo>) {
    let mut seen = std::collections::HashSet::new();
    releases.retain(|r| {
        let key = match &r.info_hash {
            Some(h) if !h.is_empty() => format!("h:{}", h.to_ascii_lowercase()),
            _ => format!("t:{}:{}", r.title.to_ascii_lowercase(), r.size.unwrap_or(0)),
        };
        seen.insert(key)
    });
}

fn trimmed(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn number<T: std::str::FromStr>(s: &Option<String>, what: &str) -> Result<Option<T>, String> {
    match trimmed(s) {
        None => Ok(None),
        Some(v) => v.parse().map(Some).map_err(|_| format!("Invalid {what} `{v}`")),
    }
}

fn query(kind: SearchKind, p: &TorznabParams) -> Result<SearchQuery, String> {
    let categories = trimmed(&p.cat)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| c.parse::<u32>().map_err(|_| format!("Invalid category `{c}`")))
        .collect::<Result<Vec<_>, _>>()?;
    // Some clients send the IMDb id without `tt`, some with it.
    let imdb_id = trimmed(&p.imdbid).map(|i| if i.starts_with("tt") { i } else { format!("tt{i}") });
    Ok(SearchQuery {
        kind,
        term: trimmed(&p.q),
        imdb_id,
        tmdb_id: number(&p.tmdbid, "tmdbid")?,
        tvdb_id: number(&p.tvdbid, "tvdbid")?,
        year: number(&p.year, "year")?,
        season: number(&p.season, "season")?,
        episode: trimmed(&p.ep),
        categories,
        limit: Some(
            number::<u32>(&p.limit, "limit")?
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(1, MAX_LIMIT),
        ),
        offset: number(&p.offset, "offset")?,
        author: trimmed(&p.author),
        title: trimmed(&p.title),
        artist: trimmed(&p.artist),
        album: trimmed(&p.album),
        book_format: None,
    })
}

fn apikey(p: &TorznabParams, headers: &HeaderMap) -> String {
    p.apikey
        .clone()
        .or_else(|| {
            headers
                .get(reelix_api_types::API_KEY_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

/// The externally visible base URL (`scheme://host[/url_base]`), honouring reverse-proxy headers.
fn base_url(headers: &HeaderMap, state: &AppState) -> String {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').next().unwrap_or(v).trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let scheme = get("x-forwarded-proto").unwrap_or_else(|| "http".into());
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .unwrap_or_else(|| "localhost".into());
    format!("{scheme}://{host}{}", state.config().url_base)
}

async fn caps(svc: &ProviderService, ids: &[i64]) -> Response {
    // Categories: the union of what the covered indexers report (capabilities are cached
    // after the first fetch), else the standard list.
    let lookups = svc
        .indexers()
        .into_iter()
        .filter(|(row, _)| ids.contains(&row.id))
        .map(|(_, ix)| async move {
            tokio::time::timeout(CAPS_TIMEOUT, ix.capabilities())
                .await
                .ok()
                .and_then(Result::ok)
        });
    let mut cats: Vec<Category> = Vec::new();
    for found in futures::future::join_all(lookups).await.into_iter().flatten() {
        for c in found.categories {
            match cats.iter_mut().find(|e| e.id == c.id) {
                Some(existing) => {
                    for s in c.subcategories {
                        if !existing.subcategories.iter().any(|e| e.id == s.id) {
                            existing.subcategories.push(s);
                        }
                    }
                }
                None => cats.push(c),
            }
        }
    }
    if cats.is_empty() {
        cats = categories::standard();
    }
    cats.sort_by_key(|c| c.id);
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<caps>\n");
    let _ = writeln!(
        s,
        "  <server version=\"{}\" title=\"Reelix\" strapline=\"Reelix indexer aggregator\"/>",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(s, "  <limits max=\"{MAX_LIMIT}\" default=\"{DEFAULT_LIMIT}\"/>");
    s.push_str("  <searching>\n");
    s.push_str("    <search available=\"yes\" supportedParams=\"q\"/>\n");
    s.push_str("    <tv-search available=\"yes\" supportedParams=\"q,season,ep,imdbid,tvdbid,tmdbid\"/>\n");
    s.push_str("    <movie-search available=\"yes\" supportedParams=\"q,imdbid,tmdbid,year\"/>\n");
    s.push_str("    <music-search available=\"yes\" supportedParams=\"q,artist,album\"/>\n");
    s.push_str("    <audio-search available=\"yes\" supportedParams=\"q,artist,album\"/>\n");
    s.push_str("    <book-search available=\"yes\" supportedParams=\"q,author,title\"/>\n");
    s.push_str("  </searching>\n  <categories>\n");
    for c in &cats {
        if c.subcategories.is_empty() {
            let _ = writeln!(s, "    <category id=\"{}\" name=\"{}\"/>", c.id, esc(&c.name));
        } else {
            let _ = writeln!(s, "    <category id=\"{}\" name=\"{}\">", c.id, esc(&c.name));
            for sub in &c.subcategories {
                let _ = writeln!(s, "      <subcat id=\"{}\" name=\"{}\"/>", sub.id, esc(&sub.name));
            }
            s.push_str("    </category>\n");
        }
    }
    s.push_str("  </categories>\n</caps>\n");
    xml(StatusCode::OK, s)
}

fn attr(s: &mut String, name: &str, value: impl std::fmt::Display) {
    let _ = writeln!(
        s,
        "      <torznab:attr name=\"{name}\" value=\"{}\"/>",
        esc(&value.to_string())
    );
}

/// What the RSS writer needs besides the releases.
struct Feed<'a> {
    base: &'a str,
    target: &'a str,
    protocol: Protocol,
    ns: &'a str,
    apikey: &'a str,
    /// Seals an indexer download URL for [`download`].
    seal: &'a (dyn Fn(&str) -> Option<String> + Sync),
}

fn rss(releases: &[ReleaseInfo], feed: &Feed<'_>) -> String {
    let (base, target, protocol, ns) = (feed.base, feed.target, feed.protocol, feed.ns);
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        s,
        "<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" xmlns:torznab=\"http://torznab.com/schemas/2015/feed\" xmlns:newznab=\"http://www.newznab.com/DTD/2010/feeds/attributes/\">"
    );
    s.push_str("  <channel>\n");
    let _ = writeln!(s, "    <title>Reelix ({})</title>", esc(target));
    let _ = writeln!(s, "    <description>Reelix {ns} feed</description>");
    let _ = writeln!(s, "    <link>{}</link>", esc(base));
    let _ = writeln!(
        s,
        "    <newznab:response offset=\"0\" total=\"{}\"/>",
        releases.len()
    );
    for r in releases {
        s.push_str("    <item>\n");
        let _ = writeln!(s, "      <title>{}</title>", esc(&r.title));
        let _ = writeln!(s, "      <guid>{}</guid>", esc(&r.guid));
        let _ = writeln!(
            s,
            "      <jackettindexer id=\"{}\">{}</jackettindexer>",
            r.indexer_id,
            esc(&r.indexer)
        );
        let _ = writeln!(
            s,
            "      <prowlarrindexer id=\"{}\">{}</prowlarrindexer>",
            r.indexer_id,
            esc(&r.indexer)
        );
        if let Some(info) = &r.info_url {
            let _ = writeln!(s, "      <comments>{}</comments>", esc(info));
        }
        if let Some(d) = r.publish_date {
            let _ = writeln!(s, "      <pubDate>{}</pubDate>", d.to_rfc2822());
        }
        let size = r.size.unwrap_or(0);
        if r.size.is_some() {
            let _ = writeln!(s, "      <size>{size}</size>");
        }
        let link = match (&r.download_url, &r.magnet_url) {
            (Some(url), _) => Some(proxy_link(feed, r.indexer_id, url)),
            (None, Some(magnet)) => Some(magnet.clone()),
            (None, None) => None,
        };
        if let Some(link) = &link {
            let _ = writeln!(s, "      <link>{}</link>", esc(link));
            let mime = if protocol == Protocol::Torrent {
                "application/x-bittorrent"
            } else {
                "application/x-nzb"
            };
            let _ = writeln!(
                s,
                "      <enclosure url=\"{}\" length=\"{size}\" type=\"{mime}\"/>",
                esc(link)
            );
        }
        for c in &r.categories {
            let _ = writeln!(s, "      <category>{c}</category>");
        }
        for c in &r.categories {
            attr(&mut s, "category", c);
        }
        if r.size.is_some() {
            attr(&mut s, "size", size);
        }
        if let Some(v) = r.grabs {
            attr(&mut s, "grabs", v);
        }
        if let Some(v) = r.files {
            attr(&mut s, "files", v);
        }
        if let Some(v) = &r.imdb_id {
            attr(&mut s, "imdbid", v.trim_start_matches("tt"));
            attr(&mut s, "imdb", v.trim_start_matches("tt"));
        }
        if let Some(v) = r.tmdb_id {
            attr(&mut s, "tmdbid", v);
        }
        if let Some(v) = r.tvdb_id {
            attr(&mut s, "tvdbid", v);
        }
        if protocol == Protocol::Torrent {
            if let Some(v) = r.seeders {
                attr(&mut s, "seeders", v);
            }
            if let Some(v) = r.peers {
                attr(&mut s, "peers", v);
            }
            if let Some(v) = &r.info_hash {
                attr(&mut s, "infohash", v);
            }
            if let Some(v) = &r.magnet_url {
                attr(&mut s, "magneturl", v);
            }
            attr(&mut s, "downloadvolumefactor", r.download_volume_factor);
            attr(&mut s, "uploadvolumefactor", r.upload_volume_factor);
            if let Some(v) = r.minimum_ratio {
                attr(&mut s, "minimumratio", v);
            }
            if let Some(v) = r.minimum_seed_time {
                attr(&mut s, "minimumseedtime", v);
            }
        }
        s.push_str("    </item>\n");
    }
    s.push_str("  </channel>\n</rss>\n");
    if ns == "newznab" {
        s = s.replace("<torznab:attr ", "<newznab:attr ");
    }
    s
}

/// A link to [`download`] carrying the indexer URL sealed with the server secret.
fn proxy_link(feed: &Feed<'_>, indexer_id: i64, url: &str) -> String {
    // Sealing can only fail on an RNG error; fall back to the original URL then.
    let Some(sealed) = (feed.seal)(&format!(
        "{LINK_PURPOSE}\n{}\n{indexer_id}\n{url}",
        Utc::now().timestamp()
    )) else {
        return url.to_owned();
    };
    let (base, ns, apikey) = (feed.base, feed.ns, feed.apikey);
    let mut u = format!("{base}/api/{ns}/{indexer_id}/dl?link={}", urlencoding(&sealed));
    if !apikey.is_empty() {
        u.push_str("&apikey=");
        u.push_str(&urlencoding(apikey));
    }
    u
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn download(
    State(state): State<AppState>,
    user: MaybeUser,
    Path(target): Path<String>,
    Query(p): Query<DownloadParams>,
) -> Response {
    if let Err(r) = authorize(&user) {
        return r;
    }
    let svc = match svc(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Ok(opened) = state.secrets().decrypt(&p.link) else {
        return error(StatusCode::BAD_REQUEST, 201, "Invalid or expired link");
    };
    // `torznab-dl \n <issued unix secs> \n <indexer id> \n <url>`: the purpose prefix keeps
    // other sealed values (provider secrets) from being usable as links.
    let mut parts = opened.splitn(4, '\n');
    let (Some(LINK_PURPOSE), Some(issued), Some(id), Some(url)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return error(StatusCode::BAD_REQUEST, 201, "Invalid link");
    };
    let (Ok(issued), Ok(id)) = (issued.parse::<i64>(), id.parse::<i64>()) else {
        return error(StatusCode::BAD_REQUEST, 201, "Invalid link");
    };
    if Utc::now().timestamp() - issued > LINK_TTL_SECS {
        return error(
            StatusCode::GONE,
            201,
            "This download link has expired; search again",
        );
    }
    let url = url.to_owned();
    if target != "all" && target.parse::<i64>().ok() != Some(id) {
        return error(StatusCode::BAD_REQUEST, 201, "Link belongs to another indexer");
    }
    match svc.indexer_manager().download(id, &url).await {
        Ok(bytes) => {
            let torrent = svc
                .indexers()
                .iter()
                .any(|(row, ix)| row.id == id && ix.protocol() == Protocol::Torrent);
            let (mime, ext) = if torrent {
                ("application/x-bittorrent", "torrent")
            } else {
                ("application/x-nzb", "nzb")
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.to_owned()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!(
                            "attachment; filename=\"reelix-{}.{ext}\"",
                            Utc::now().timestamp_millis()
                        ),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(IndexerError::MagnetRedirect(magnet)) => Redirect::to(&magnet).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, 900, &e.to_string()),
    }
}
