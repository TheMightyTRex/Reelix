//! Cardigann-compatibility features of the definition engine, exercised with original fictional
//! definitions (`fixtures/definitions-compat`) served by wiremock.

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::{fixture, fixture_path};
use reelix_indexers::{BuildContext, DefinitionStore, Indexer, IndexerError, SearchQuery, build};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn store() -> Arc<DefinitionStore> {
    let (store, errors) = DefinitionStore::load_dir(&fixture_path("definitions-compat")).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(store)
}

fn ctx(name: &str) -> BuildContext {
    let mut c = BuildContext::new(3, name);
    c.allow_private_addresses = true;
    c.definitions = Some(store());
    c
}

fn html(rel: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(rel), "text/html; charset=utf-8")
}

fn redirect(to: &str) -> ResponseTemplate {
    ResponseTemplate::new(302).insert_header("Location", to)
}

fn indexer(id: &str, server: &MockServer, extra: serde_json::Value) -> Box<dyn Indexer> {
    let mut settings = json!({"base_url": server.uri()});
    settings
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    build(&format!("definition:{id}"), &settings, &ctx(id)).unwrap()
}

async fn heron_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/login.php"))
        .and(query_param("pk", "s3cret"))
        .respond_with(redirect("/index.php").insert_header("Set-Cookie", "hb=1; Path=/"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/login.php"))
        .and(query_param("pk", "banned"))
        .respond_with(html("html-compat/heron_banned.html"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/index.php"))
        .and(header("cookie", "hb=1"))
        .respond_with(html("html-compat/heron_index.html"))
        .mount(&server)
        .await;
    // The search path template is URL-encoded and redirects to the real results page.
    Mock::given(method("GET"))
        .and(path("/search/Quiet+Harbor"))
        .and(header("x-heron-key", "s3cret"))
        .respond_with(redirect("/browse.php?q=Quiet+Harbor"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/browse.php"))
        .and(header("cookie", "hb=1"))
        .respond_with(html("html-compat/heron_browse.html"))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn oneurl_login_merged_rows_dateheaders_and_andmatch() {
    let server = heron_server().await;
    let idx = indexer("heronbay", &server, json!({"passkey": "s3cret"}));
    let started = Instant::now();
    let r = idx.search(&SearchQuery::basic("Quíet Harbor")).await.unwrap();
    // login + index + search + browse = 4 requests, 50ms apart.
    assert!(started.elapsed().as_millis() >= 150, "requestDelay is honoured");
    let titles: Vec<&str> = r.iter().map(|x| x.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Quiet Harbor 2025 1080p WEB-DL MULTi",
            "Quiet Harbor S01E01 720p",
            "Quiet Harbor 2025 2160p MULTI.VFF"
        ],
        "andmatch drops the unrelated row; the lookahead regex only rewrites a bare MULTI"
    );
    let a = &r[0];
    assert_eq!(
        a.download_url.as_deref(),
        Some(format!("{}/get.php?id=11", server.uri()).as_str())
    );
    assert_eq!(a.size, Some(4_509_715_661));
    assert_eq!(
        (a.seeders, a.peers),
        (Some(12), Some(15)),
        "fields read from the merged second row"
    );
    assert_eq!(a.download_volume_factor, 0.0, "td:contains(\"FREE\")");
    assert_eq!(a.publish_date.unwrap().to_rfc3339(), "2026-04-28T00:00:00+00:00");
    let b = &r[1];
    assert_eq!(b.categories, [5040]);
    assert_eq!(b.download_volume_factor, 1.0);
    assert_eq!(b.publish_date.unwrap().to_rfc3339(), "2026-04-28T00:00:00+00:00");
    let c = &r[2];
    assert_eq!(c.download_volume_factor, 0.0, "img.free");
    assert_eq!((c.seeders, c.peers), (Some(30), Some(35)));
    assert_eq!(c.publish_date.unwrap().to_rfc3339(), "2026-04-27T00:00:00+00:00");
}

#[tokio::test]
async fn templated_row_selector_follows_settings() {
    let server = heron_server().await;
    let idx = indexer(
        "heronbay",
        &server,
        json!({"passkey": "s3cret", "freeleech": true}),
    );
    let r = idx.search(&SearchQuery::basic("Quiet Harbor")).await.unwrap();
    let ids: Vec<&str> = r.iter().map(|x| x.download_url.as_deref().unwrap()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids[0].ends_with("id=11") && ids[1].ends_with("id=14"));
}

#[tokio::test]
async fn root_contains_login_error() {
    let server = heron_server().await;
    let idx = indexer("heronbay", &server, json!({"passkey": "banned"}));
    match idx.test().await {
        Err(IndexerError::Auth(m)) => assert_eq!(m, "banned"),
        other => panic!("{other:?}"),
    }
}

async fn reed_server(login_page: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/signin"))
        .respond_with(html(login_page).insert_header("Set-Cookie", "rw=pre; Path=/"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/signin/submit"))
        .and(query_param("nonce", "n0nce"))
        .and(header("cookie", "rw=pre"))
        .and(body_string_contains("u_7f=warbler"))
        .and(body_string_contains("p_3c=reeds"))
        .and(body_string_contains("keep=1"))
        .and(body_string_contains("token=tok123"))
        .respond_with(redirect("/my").insert_header("Set-Cookie", "rw=ok; Path=/"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/my"))
        .and(header("cookie", "rw=ok"))
        .respond_with(html("html-compat/reed_my.html"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/torrents"))
        .and(header("cookie", "rw=ok"))
        .respond_with(|req: &Request| {
            // `genre` renders empty and is dropped (no allowEmptyInputs).
            if req.url.query_pairs().any(|(k, _)| k == "genre") {
                ResponseTemplate::new(400)
            } else {
                html("html-compat/reed_torrents.html")
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/details"))
        .and(query_param("id", "7"))
        .respond_with(html("html-compat/reed_details.html"))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn form_login_selector_inputs_and_infohash_download() {
    let server = reed_server("html-compat/reed_signin.html").await;
    Mock::given(method("POST"))
        .and(path("/thanks"))
        .and(body_string_contains("id=7"))
        .respond_with(ResponseTemplate::new(200).set_body_string("thanked"))
        .expect(1)
        .mount(&server)
        .await;
    let idx = indexer(
        "reedwarbler",
        &server,
        json!({"username": "warbler", "password": "reeds"}),
    );
    let r = idx.search(&SearchQuery::basic("reed")).await.unwrap();
    assert_eq!(
        r.len(),
        1,
        "nuked and link-less cards are excluded by :has()/:not()"
    );
    assert_eq!(r[0].title, "Reed Song 2026 1080p");
    assert_eq!(
        r[0].size,
        Some(3_221_225_472),
        ":containsOwn ignores text in child elements"
    );
    match idx.download(r[0].download_url.as_deref().unwrap()).await {
        Err(IndexerError::MagnetRedirect(m)) => assert_eq!(
            m,
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Reed+Song_+2026_1080p"
        ),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn login_captcha_is_reported_as_unsupported() {
    let server = reed_server("html-compat/reed_signin_captcha.html").await;
    let idx = indexer(
        "reedwarbler",
        &server,
        json!({"username": "warbler", "password": "reeds"}),
    );
    match idx.test().await {
        Err(e @ IndexerError::Unsupported(_)) => assert!(e.to_string().contains("captcha"), "{e}"),
        other => panic!("{other:?}"),
    }
}

fn lark_body() -> serde_json::Value {
    json!({
        "meta": {"total": 2},
        "data": [
            {"id": 1, "attributes": {
                "name": "Lark Film 2026 1080p", "category_id": 1, "details_link": "/t/1",
                "download_link": "/dl/1", "size": 1024, "seeders": 7, "leechers": 2,
                "created_at": "2026-04-20T10:00:00Z", "featured": true, "free": true,
                "genres": ["Drama", "Mystery"], "meta": {"imdb": "1234567"}}},
            {"id": 2, "attributes": {
                "name": "Lark Show S01", "category_id": 2, "details_link": "/t/2",
                "download_link": "/dl/2", "size": 2048, "seeders": 1, "leechers": 0,
                "created_at": 1776679200, "featured": false, "free": false,
                "genres": [], "meta": {"imdb": null}}}
        ]
    })
}

#[tokio::test]
async fn json_api_headers_attribute_rows_and_bool_cases() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/torrents"))
        .and(query_param("name", "lark"))
        .and(header("authorization", "Bearer k3y"))
        .respond_with(ResponseTemplate::new(200).set_body_json(lark_body()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/torrents"))
        .and(query_param("name", "empty"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/torrents"))
        .and(query_param("name", "zero"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"meta": {"total": 0}, "data": [{"bogus": 1}]})),
        )
        .mount(&server)
        .await;

    let idx = indexer("marshlark", &server, json!({"apikey": "k3y"}));
    let r = idx.search(&SearchQuery::basic("lark")).await.unwrap();
    assert_eq!(r.len(), 2);
    let (a, b) = (&r[0], &r[1]);
    assert_eq!(a.title, "Lark Film 2026 1080p");
    assert_eq!(a.categories, [2000]);
    assert_eq!(
        a.download_url.as_deref(),
        Some(format!("{}/dl/1", server.uri()).as_str())
    );
    assert_eq!((a.download_volume_factor, a.upload_volume_factor), (0.0, 2.0));
    assert_eq!(a.imdb_id.as_deref(), Some("tt1234567"));
    assert_eq!(a.publish_date.unwrap().to_rfc3339(), "2026-04-20T10:00:00+00:00");
    assert_eq!((b.download_volume_factor, b.upload_volume_factor), (1.0, 1.0));
    assert_eq!(b.publish_date.unwrap().timestamp(), 1_776_679_200);
    assert!(b.imdb_id.is_none());

    let free = indexer("marshlark", &server, json!({"apikey": "k3y", "freeleech": true}));
    let r = free.search(&SearchQuery::basic("lark")).await.unwrap();
    assert_eq!(r.len(), 1, "templated JSON rows filter");
    assert_eq!(r[0].title, "Lark Film 2026 1080p");

    assert!(
        idx.search(&SearchQuery::basic("empty")).await.unwrap().is_empty(),
        "noResultsMessage"
    );
    assert!(
        idx.search(&SearchQuery::basic("zero")).await.unwrap().is_empty(),
        "rows.count"
    );
}

#[tokio::test]
async fn json_multiple_rows_read_parent_fields() {
    let server = MockServer::start().await;
    Mock::given(path("/api/list.json"))
        .and(query_param("query_term", "dove"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"movies": [
            {"title": "Dove Story", "year": 2024, "url": "https://dovenest.invalid/m/1", "encodes": [
                {"quality": "720p", "url": "https://dovenest.invalid/t/a", "hash": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "size_bytes": 100, "seeds": 3},
                {"quality": "1080p", "url": "https://dovenest.invalid/t/b", "hash": "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", "size_bytes": 200, "seeds": 9}
            ]}
        ]}})))
        .mount(&server)
        .await;
    let idx = indexer("dovenest", &server, json!({}));
    let r = idx.search(&SearchQuery::basic("dove")).await.unwrap();
    let titles: Vec<&str> = r.iter().map(|x| x.title.as_str()).collect();
    assert_eq!(titles, ["Dove Story 2024 720p", "Dove Story 2024 1080p"]);
    assert_eq!(
        r[1].info_hash.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert_eq!(r[1].seeders, Some(9));
    assert_ne!(
        r[0].download_url, r[1].download_url,
        "encodes sharing a details page are kept apart"
    );
}

#[tokio::test]
async fn xml_feed_is_parsed_as_xml() {
    let server = MockServer::start().await;
    let rss = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><title>feed</title>
<item><title>Kingfisher Dive 2026 2160p</title><comments>https://kingfisher.invalid/i/9</comments>
<link>https://kingfisher.invalid/ignored</link>
<pubDate>Tue, 28 Apr 2026 07:15:00 +0000</pubDate>
<enclosure url="https://kingfisher.invalid/d/9.torrent" length="5000" type="application/x-bittorrent"/>
<torznab:attr name="category" value="2000"/><torznab:attr name="seeders" value="41"/></item>
</channel></rss>"#;
    Mock::given(path("/rss.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(rss, "application/rss+xml"))
        .mount(&server)
        .await;
    let idx = indexer("kingfisher", &server, json!({}));
    let r = idx.search(&SearchQuery::basic("dive")).await.unwrap();
    assert_eq!(r.len(), 1);
    let a = &r[0];
    assert_eq!(a.title, "Kingfisher Dive 2026 2160p");
    assert_eq!(
        a.download_url.as_deref(),
        Some("https://kingfisher.invalid/d/9.torrent")
    );
    assert_eq!(
        (a.size, a.seeders, a.categories.as_slice()),
        (Some(5000), Some(41), &[2000][..])
    );
    assert_eq!(a.publish_date.unwrap().to_rfc3339(), "2026-04-28T07:15:00+00:00");
}

#[test]
fn unsupported_features_fail_at_load_with_a_clear_message() {
    let base = fixture("definitions-compat/kingfisher.yml");
    let cases = [
        (
            base.replace(
                "selector: title",
                "selector: title\n      filters:\n        - name: rot13",
            ),
            "unsupported: filter `rot13`",
        ),
        (
            base.replace("q: \"{{ .Keywords }}\"", "q: \"{{ printf .Keywords }}\""),
            "unsupported: template function `printf`",
        ),
        (
            base.replace("selector: comments", "selector: \"comments:visible\""),
            "unsupported pseudo-class `:visible`",
        ),
        (
            format!("{base}login:\n  method: twofactor\n  path: x\n"),
            "unsupported: login method `twofactor`",
        ),
    ];
    for (yaml, want) in cases {
        let mut s = DefinitionStore::default();
        let err = s.add_yaml(&yaml).unwrap_err().to_string();
        assert!(err.contains(want), "{err} should contain {want}");
    }
}

/// Measures compatibility with a local copy of the community catalog:
/// `REELIX_DEFINITIONS_DIR=/path/to/definitions cargo test -p reelix-indexers -- --ignored`.
#[test]
#[ignore = "needs REELIX_DEFINITIONS_DIR"]
fn upstream_catalog_loads() {
    let Ok(dir) = std::env::var("REELIX_DEFINITIONS_DIR") else {
        eprintln!("REELIX_DEFINITIONS_DIR unset; skipping");
        return;
    };
    let (store, errors) = DefinitionStore::load_dir(std::path::Path::new(&dir)).unwrap();
    let loaded = store.list().len();
    let total = loaded + errors.len();
    assert!(total > 0, "no definitions in {dir}");
    for e in &errors {
        eprintln!("{}: {}", e.path.display(), e.message);
    }
    let pct = loaded as f64 * 100.0 / total as f64;
    assert!(pct >= 95.0, "only {loaded}/{total} ({pct:.1}%) definitions load");
}
