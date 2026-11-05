//! Generic podcast RSS feeds: any show with a feed.
//!
//! The show's external id is its feed URL. Series come from `itunes:season` (`season:N`);
//! episodes without one go to series 1, and `itunes:episodeType` `trailer`/`bonus` go to the
//! specials. Episode numbers come from `itunes:episode`; the service numbers the rest by first
//! broadcast. Search is not supported (directories such as Podcast Index do that and hand
//! back feed URLs); any http(s) link that returns a feed previews — an HTML page that
//! advertises its feed (`<link rel="alternate" type="application/rss+xml">`) works too.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDateTime, Utc};
use reelix_api_types::radio::RadioSeriesKind;

use crate::provider::{
    EpisodeData, ProviderError, RadioProvider, SeriesData, ShowData, ShowFetch, async_trait,
};
use crate::xml::{self, Node};

pub const ID: &str = "rss";

/// Largest feed read (some long-running shows have feeds of several MB).
pub const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;

pub struct RssProvider {
    http: reqwest::Client,
    /// Used inside [`restricted`]: reaches public addresses only.
    public_http: Option<reqwest::Client>,
}

tokio::task_local! {
    static RESTRICTED: bool;
}

/// Runs `f` with feed reads limited to public addresses (the client given with
/// [`RssProvider::with_public_client`]), for links that come from users who may not reach the
/// server's network: redirects and a page's advertised feed are checked on every hop.
pub async fn restricted<F: std::future::Future>(f: F) -> F::Output {
    RESTRICTED.scope(true, f).await
}

fn is_restricted() -> bool {
    RESTRICTED.try_with(|r| *r).unwrap_or(false)
}

impl RssProvider {
    /// `http` is the server's shared client (user agent, timeouts).
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            public_http: None,
        }
    }

    /// The client used for reads inside [`restricted`]; without one those reads are refused.
    pub fn with_public_client(mut self, client: reqwest::Client) -> Self {
        self.public_http = Some(client);
        self
    }

    /// GETs `url` and returns the body, refusing more than [`MAX_FEED_BYTES`].
    async fn fetch(&self, url: &str) -> Result<String, ProviderError> {
        let net = |e: reqwest::Error| ProviderError::Network(e.without_url().to_string());
        let http = if is_restricted() {
            self.public_http.as_ref().ok_or_else(|| {
                ProviderError::Network(
                    "this link cannot be read here; ask a library manager to add the show".into(),
                )
            })?
        } else {
            &self.http
        };
        let mut res = http
            .get(url)
            .header(
                reqwest::header::ACCEPT,
                "application/rss+xml, application/xml;q=0.9, text/xml;q=0.9, text/html;q=0.5, */*;q=0.1",
            )
            .send()
            .await
            .map_err(net)?;
        let status = res.status();
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            return Err(ProviderError::NotFound(format!("{url} returned {status}")));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = res
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(std::time::Duration::from_secs);
            return Err(ProviderError::RateLimited { retry_after });
        }
        if !status.is_success() {
            return Err(ProviderError::Network(format!("{url} returned {status}")));
        }
        if res.content_length().is_some_and(|n| n > MAX_FEED_BYTES as u64) {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(net)? {
            if body.len() + chunk.len() > MAX_FEED_BYTES {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Fetches and parses `url`; for an HTML page, follows its advertised feed once.
    /// `Ok(None)` = not a feed.
    pub(crate) async fn read(&self, url: &str) -> Result<Option<ShowData>, ProviderError> {
        let body = self.fetch(url).await?;
        if let Some(show) = parse_feed(&body, url)? {
            return Ok(Some(show));
        }
        let Some(feed) = advertised_feed(&body, url) else {
            return Ok(None);
        };
        let body = self.fetch(&feed).await?;
        parse_feed(&body, &feed)
    }
}

fn too_large() -> ProviderError {
    ProviderError::Parse(format!(
        "the feed is larger than {} MB",
        MAX_FEED_BYTES / (1024 * 1024)
    ))
}

fn is_http(link: &str) -> bool {
    url::Url::parse(link.trim()).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
}

#[async_trait]
impl RadioProvider for RssProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Podcast feed"
    }

    fn is_feed(&self) -> bool {
        true
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        if !is_http(link) {
            return Ok(None);
        }
        self.read(link.trim()).await
    }

    async fn show(&self, external_id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        if !is_http(external_id) {
            return Err(ProviderError::NotFound(format!(
                "{external_id} is not a feed address"
            )));
        }
        self.read(external_id)
            .await?
            .ok_or_else(|| ProviderError::Parse(format!("{external_id} is no longer a podcast feed")))
    }
}

/// The feed a web page advertises with `<link rel="alternate" type="application/rss+xml" href=…>`.
fn advertised_feed(html: &str, base: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<link") {
        let start = from + i;
        let end = start + lower[start..].find('>')?;
        let tag = &html[start..end];
        let tag_l = &lower[start..end];
        from = end;
        if !(tag_l.contains("application/rss+xml") && tag_l.contains("alternate")) {
            continue;
        }
        let href = attr_value(tag, tag_l, "href")?;
        let base = url::Url::parse(base).ok()?;
        let feed = base.join(&href).ok()?;
        if matches!(feed.scheme(), "http" | "https") {
            return Some(feed.to_string());
        }
    }
    None
}

fn attr_value(tag: &str, tag_l: &str, name: &str) -> Option<String> {
    let at = tag_l.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let quote = rest.chars().next()?;
    let v = if quote == '"' || quote == '\'' {
        let rest = &rest[1..];
        &rest[..rest.find(quote)?]
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '/').next()?
    };
    Some(v.replace("&amp;", "&"))
}

/// Parses an RSS 2.0 podcast feed fetched from `url`. `Ok(None)` when the document is not a
/// feed (an HTML page, an Atom document, …).
pub fn parse_feed(body: &str, url: &str) -> Result<Option<ShowData>, ProviderError> {
    let trimmed = body.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('<') {
        return Ok(None);
    }
    let root = match xml::parse(trimmed) {
        Ok(r) => r,
        // An HTML page usually is not well-formed XML; only a document that claims to be RSS
        // is an error.
        Err(e) if trimmed.contains("<rss") => return Err(ProviderError::Parse(e)),
        Err(_) => return Ok(None),
    };
    if !root.local().eq_ignore_ascii_case("rss") {
        return Ok(None);
    }
    let Some(channel) = root.child_ns("", "channel") else {
        return Err(ProviderError::Parse("the feed has no channel".into()));
    };
    Ok(Some(channel_data(channel, url)))
}

fn channel_data(ch: &Node, url: &str) -> ShowData {
    let title = ch
        .child_text("title")
        .map(clean_line)
        .unwrap_or_else(|| url.to_owned());
    let synopsis = ch
        .child_text("description")
        .or_else(|| ch.text_ns("itunes", "summary"))
        .map(plain_text)
        .filter(|s| !s.is_empty());
    let artwork_url = ch
        .child_ns("itunes", "image")
        .and_then(|i| i.attr("href"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            ch.child_ns("", "image")
                .and_then(|i| i.child_text("url"))
                .map(str::to_owned)
        });
    let author = ch
        .text_ns("itunes", "author")
        .or_else(|| ch.child_text("managingEditor"))
        .map(clean_line);
    let web_url = ch.child_text("link").map(str::to_owned).filter(|l| is_http(l));

    let mut episodes = Vec::new();
    // series external id → (kind, number)
    let mut series: BTreeMap<String, (RadioSeriesKind, Option<u32>)> = BTreeMap::new();
    for item in ch.children_named("item") {
        let Some(ep) = item_data(item) else { continue };
        let kind = if matches!(
            item.text_ns("itunes", "episodeType")
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("trailer" | "bonus")
        ) {
            RadioSeriesKind::Specials
        } else {
            RadioSeriesKind::Series
        };
        let season = item.text_ns("itunes", "season").and_then(number);
        let (sid, number) = match (kind, season) {
            (RadioSeriesKind::Specials, _) => ("specials".to_owned(), None),
            (_, Some(n)) => (format!("season:{n}"), Some(n)),
            // No season: series 1, numbered by Reelix (number None, filled by the service).
            (_, None) => ("season:1".to_owned(), None),
        };
        let entry = series.entry(sid.clone()).or_insert((kind, number));
        if entry.1.is_none() {
            entry.1 = number;
        }
        episodes.push(EpisodeData {
            series_external_id: sid,
            ..ep
        });
    }
    // `season:1` also gets its number when only some items say `itunes:season` 1.
    let series = series
        .into_iter()
        .map(|(external_id, (kind, number))| SeriesData {
            number: match kind {
                RadioSeriesKind::Specials => None,
                RadioSeriesKind::Series => number,
            },
            external_id,
            kind,
            ..SeriesData::default()
        })
        .collect();
    let first_broadcast = episodes.iter().filter_map(|e| e.first_broadcast).min();
    ShowData {
        external_id: url.to_owned(),
        title,
        synopsis,
        author,
        web_url,
        feed_url: Some(url.to_owned()),
        artwork_url,
        first_broadcast,
        station: None,
        series,
        episodes,
        provider: None,
        complete: true,
    }
}

/// One `<item>`; `None` without any usable id. The series is filled in by the caller.
fn item_data(item: &Node) -> Option<EpisodeData> {
    let enclosure = item.child_ns("", "enclosure");
    let enclosure_url = enclosure
        .and_then(|e| e.attr("url"))
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_owned);
    let guid = item.child_text("guid").map(str::to_owned);
    let title = item
        .child_text("title")
        .or_else(|| item.text_ns("itunes", "title"))
        .map(clean_line);
    let external_id = guid
        .clone()
        .or_else(|| enclosure_url.clone())
        .or_else(|| item.child_text("link").map(str::to_owned))
        .or_else(|| title.clone())?;
    Some(EpisodeData {
        external_id,
        series_external_id: String::new(),
        number: item.text_ns("itunes", "episode").and_then(number),
        title: title.unwrap_or_else(|| "Untitled episode".into()),
        synopsis: item
            .child_text("description")
            .or_else(|| item.text_ns("itunes", "summary"))
            .or_else(|| item.text_ns("content", "encoded"))
            .map(plain_text)
            .filter(|s| !s.is_empty()),
        first_broadcast: item.child_text("pubDate").and_then(parse_date),
        duration_secs: item.text_ns("itunes", "duration").and_then(parse_duration),
        enclosure_url,
        guid,
    })
}

/// A positive whole number (`"3"`, `" 03 "`).
fn number(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

/// `itunes:duration`: `HH:MM:SS`, `MM:SS` or seconds (fractions dropped).
pub fn parse_duration(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        let p = p.trim();
        let v: u64 = if i + 1 == parts.len() {
            p.split('.').next()?.parse().ok()?
        } else {
            p.parse().ok()?
        };
        total = total.checked_mul(60)?.checked_add(v)?;
    }
    u32::try_from(total).ok().filter(|t| *t > 0)
}

/// RFC 2822 `pubDate`, with the usual feed variations (named zones, missing weekday, ISO).
pub fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    // "Tue, 05 Mar 2024 10:00:00 BST" and other zones chrono's RFC 2822 parser rejects:
    // read the local time and apply the zone when it is known.
    let (rest, zone) = s.rsplit_once(' ')?;
    let offset_h: i64 = match zone.to_ascii_uppercase().as_str() {
        "BST" | "CEST" => 1,
        "CET" => 1,
        "UTC" | "GMT" | "Z" | "UT" => 0,
        "EST" => -5,
        "EDT" => -4,
        "CST" => -6,
        "CDT" => -5,
        "MST" => -7,
        "MDT" => -6,
        "PST" => -8,
        "PDT" => -7,
        "AEST" => 10,
        "AEDT" => 11,
        _ => return None,
    };
    let offset_h = if zone.eq_ignore_ascii_case("CEST") {
        2
    } else {
        offset_h
    };
    let rest = rest.split_once(", ").map_or(rest, |(_, r)| r);
    let naive = ["%d %b %Y %H:%M:%S", "%d %b %Y %H:%M", "%d %B %Y %H:%M:%S"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(rest.trim(), f).ok())?;
    Some(naive.and_utc() - chrono::Duration::hours(offset_h))
}

/// Collapses whitespace of a one-line value (titles, authors).
fn clean_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// HTML description → plain text: tags dropped (`<br>`/`<p>` become line breaks), basic
/// entities decoded, whitespace collapsed per line.
pub fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            for t in chars.by_ref() {
                if t == '>' {
                    break;
                }
                tag.push(t);
            }
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(name.as_str(), "br" | "p" | "div" | "li") {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace('\u{a0}', " ");
    let lines: Vec<String> = out.lines().map(clean_line).filter(|l| !l.is_empty()).collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("01:02:03"), Some(3723));
        assert_eq!(parse_duration("45:10"), Some(2710));
        assert_eq!(parse_duration("1800"), Some(1800));
        assert_eq!(parse_duration("1800.75"), Some(1800));
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("1:2:3:4"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn dates() {
        let d = parse_date("Tue, 05 Mar 2024 10:00:00 +0000").unwrap();
        assert_eq!(d.to_rfc3339(), "2024-03-05T10:00:00+00:00");
        assert_eq!(parse_date("Tue, 05 Mar 2024 10:00:00 GMT"), Some(d));
        assert_eq!(
            parse_date("Tue, 05 Mar 2024 11:00:00 BST"),
            Some(d),
            "named zones chrono rejects"
        );
        assert_eq!(parse_date("05 Mar 2024 05:00:00 EST"), Some(d));
        assert_eq!(parse_date("2024-03-05T10:00:00Z"), Some(d));
        assert_eq!(parse_date("someday"), None);
    }

    #[test]
    fn html_to_text() {
        assert_eq!(
            plain_text("<p>Hello&nbsp;<b>world</b></p><p>Line  two &amp; more</p>"),
            "Hello world\nLine two & more"
        );
    }

    #[test]
    fn feed_links_in_pages() {
        let html = r#"<html><head><link rel="stylesheet" href="/a.css"><link rel="alternate" type="application/rss+xml" title="Feed" href="/feed.xml?a=1&amp;b=2"></head></html>"#;
        assert_eq!(
            advertised_feed(html, "https://example.com/show/").as_deref(),
            Some("https://example.com/feed.xml?a=1&b=2")
        );
        assert_eq!(advertised_feed("<html></html>", "https://example.com/"), None);
    }

    #[test]
    fn links() {
        assert!(is_http("https://feeds.example.com/x.xml"));
        assert!(!is_http("ftp://x/y"));
        assert!(!is_http("not a link"));
        assert!(!is_http("file:///etc/passwd"));
    }
}
