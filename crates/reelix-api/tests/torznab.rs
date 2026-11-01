//! The Torznab/Newznab aggregator: caps, search, protocol split, proxied downloads and auth.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_api_types::providers::{IndexerSettingsDto, ProviderInput, ProviderKind};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_providers::{ProviderOptions, ProviderService};
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps><limits max="100" default="100" /><searching>
<search available="yes" supportedParams="q" />
<tv-search available="yes" supportedParams="q,tvdbid,season,ep" />
<movie-search available="yes" supportedParams="q,imdbid" />
</searching><categories><category id="2000" name="Movies"><subcat id="2040" name="Movies/HD"/></category><category id="5000" name="TV" /></categories></caps>"#;

struct TestApp {
    router: Router,
    _usenet: MockServer,
    _torrent: MockServer,
    usenet_id: i64,
    torrent_id: i64,
    admin_key: String,
    user_key: String,
    _dir: tempfile::TempDir,
}

fn usenet_feed(base: &str) -> String {
    let date = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc2822();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel>
<item><title>Old.Film.1999.1080p.BluRay.x264-GRP</title><guid>u1</guid><link>{base}/getnzb/u1.nzb?apikey=SECRET</link><pubDate>{date}</pubDate><newznab:attr name="size" value="1000" /><newznab:attr name="category" value="2040" /></item>
</channel></rss>"#
    )
}

fn torrent_feed(base: &str) -> String {
    let newer = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc2822();
    let older = (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc2822();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
<item><title>Old.Film.1999.2160p.UHD.BluRay.x265-T&amp;GRP</title><guid>t1</guid><link>{base}/dl/t1.torrent?passkey=SECRET</link><pubDate>{newer}</pubDate><size>2000</size>
<torznab:attr name="seeders" value="12" /><torznab:attr name="peers" value="15" /><torznab:attr name="category" value="2040" /><torznab:attr name="infohash" value="ABCDEF0123456789ABCDEF0123456789ABCDEF01" /><torznab:attr name="downloadvolumefactor" value="0" /></item>
<item><title>Old.Film.1999.720p.WEB.x264-OTHER</title><guid>t2</guid><link>{base}/dl/t2.torrent?passkey=SECRET</link><pubDate>{older}</pubDate><size>500</size><torznab:attr name="seeders" value="1" /></item>
</channel></rss>"#
    )
}

async fn mock_indexer(feed: impl Fn(&str) -> String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CAPS, "application/xml"))
        .mount(&server)
        .await;
    let body = feed(&server.uri());
    for t in ["search", "movie", "tvsearch"] {
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("t", t))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "application/rss+xml"))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex("^/(dl|getnzb)/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"d8:announce0:e".to_vec(), "application/x-bittorrent"),
        )
        .mount(&server)
        .await;
    server
}

async fn add_key(db: &Db, username: &str, permissions: Permissions) -> String {
    let user = db
        .users()
        .create(NewUser {
            username: username.into(),
            email: None,
            display_name: None,
            password_hash: None,
            auth_source: AuthSource::Local,
            permissions,
        })
        .await
        .unwrap();
    let key = reelix_api::auth::random_token();
    db.api_keys()
        .create(user.id, "test", &key[..8], &reelix_api::auth::sha256_hex(&key))
        .await
        .unwrap();
    key
}

async fn app() -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let system = JobSystem::builder(db.clone(), events.clone()).build();
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: dir.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    let mut opts = ProviderOptions::new(dir.path());
    opts.search_timeout = std::time::Duration::from_secs(5);
    let providers = ProviderService::new(
        db.clone(),
        SecretBox::from_key([5u8; 32]),
        events.clone(),
        reqwest::Client::new(),
        opts,
    )
    .await
    .unwrap();
    providers
        .set_indexer_settings(IndexerSettingsDto {
            allow_private_addresses: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let usenet = mock_indexer(usenet_feed).await;
    let torrent = mock_indexer(torrent_feed).await;
    let mut ids = Vec::new();
    for (implementation, name, server) in [("newznab", "Usenet", &usenet), ("torznab", "Tracker", &torrent)] {
        let row = providers
            .create(
                ProviderKind::Indexer,
                &ProviderInput {
                    implementation: implementation.into(),
                    name: name.into(),
                    enabled: true,
                    priority: 25,
                    tags: vec![],
                    settings: json!({"base_url": server.uri(), "api_key": "k"}),
                },
            )
            .await
            .unwrap();
        ids.push(row.id);
    }
    let admin_key = add_key(&db, "admin", Permissions::ADMIN).await;
    let user_key = add_key(&db, "viewer", Permissions::REQUEST_MOVIE).await;
    let state = AppState::new(
        db.clone(),
        events,
        system.queue(),
        SecretBox::from_key([5u8; 32]),
        config,
    )
    .with_extension(providers);
    TestApp {
        router: reelix_api::router(state),
        _usenet: usenet,
        _torrent: torrent,
        usenet_id: ids[0],
        torrent_id: ids[1],
        admin_key,
        user_key,
        _dir: dir,
    }
}

async fn get(app: &TestApp, uri: &str) -> (StatusCode, String, Option<String>) {
    let req = Request::builder()
        .uri(uri)
        .header(header::HOST, "reelix.test:7700")
        .body(Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let ctype = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned(), ctype)
}

/// The `<link>` values of a feed, unescaped.
fn links(xml: &str) -> Vec<String> {
    xml.split("<link>")
        .skip(2) // the channel link
        .filter_map(|s| s.split("</link>").next())
        .map(|s| s.replace("&amp;", "&"))
        .collect()
}

#[tokio::test]
async fn aggregates_searches_and_proxies_downloads() {
    let app = app().await;
    let key = &app.admin_key;

    // Caps lists the union of indexer categories and every search mode.
    let (status, body, ctype) = get(&app, &format!("/api/torznab/all/api?t=caps&apikey={key}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(ctype.unwrap().starts_with("application/xml"));
    assert!(body.contains(r#"<category id="2000" name="Movies">"#), "{body}");
    assert!(body.contains(r#"<subcat id="2040" name="Movies/HD"/>"#));
    assert!(body.contains(r#"<movie-search available="yes""#));

    // The torrent feed only has torrent results, newest first, with torznab attributes.
    let (status, body, _) = get(
        &app,
        &format!("/api/torznab/all/api?t=movie&imdbid=0133093&apikey={key}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("<item>").count(), 2, "{body}");
    let first = body.find("2160p").unwrap();
    let second = body.find("720p").unwrap();
    assert!(first < second, "newest first");
    assert!(body.contains("T&amp;GRP"), "titles are escaped");
    assert!(body.contains(r#"<torznab:attr name="seeders" value="12"/>"#));
    assert!(body.contains(r#"<torznab:attr name="downloadvolumefactor" value="0"/>"#));
    assert!(body.contains(r#"type="application/x-bittorrent""#));
    assert!(
        !body.contains("SECRET"),
        "indexer credentials never leave Reelix: {body}"
    );

    // Download links come back through Reelix and fetch the original file.
    let link = links(&body).into_iter().next().unwrap();
    let prefix = format!("http://reelix.test:7700/api/torznab/{}/dl?link=", app.torrent_id);
    assert!(link.starts_with(&prefix), "{link}");
    let local = link.trim_start_matches("http://reelix.test:7700");
    let (status, file, ctype) = get(&app, local).await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert_eq!(file, "d8:announce0:e");
    assert_eq!(ctype.as_deref(), Some("application/x-bittorrent"));

    // A tampered link is rejected rather than fetched.
    let tampered = local.replace("link=", "link=A");
    let (status, body, _) = get(&app, &tampered).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("<error code=\"201\""));

    // The Newznab feed has only the Usenet result.
    let (status, body, _) = get(&app, &format!("/api/newznab/all/api?t=search&q=old&apikey={key}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("<item>").count(), 1, "{body}");
    assert!(
        body.contains(r#"<newznab:attr name="category" value="2040"/>"#),
        "{body}"
    );
    assert!(body.contains("application/x-nzb"));

    // Single-indexer endpoints, and a pointer to the right protocol.
    let (status, body, _) = get(
        &app,
        &format!("/api/newznab/{}/api?t=search&q=old&apikey={key}", app.usenet_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("<item>").count(), 1);
    let (status, body, _) = get(
        &app,
        &format!("/api/torznab/{}/api?t=search&apikey={key}", app.usenet_id),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("/api/newznab/"), "{body}");

    // limit/offset page through the merged results.
    let (_, body, _) = get(
        &app,
        &format!("/api/torznab/all/api?t=search&q=old&limit=1&offset=1&apikey={key}"),
    )
    .await;
    assert_eq!(body.matches("<item>").count(), 1);
    assert!(body.contains("720p"));

    // Errors are Newznab error documents.
    let (status, body, _) = get(&app, &format!("/api/torznab/all/api?t=nope&apikey={key}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("<error code=\"202\""));
    let (_, body, _) = get(
        &app,
        &format!("/api/torznab/all/api?t=search&cat=abc&apikey={key}"),
    )
    .await;
    assert!(body.contains("Invalid category"));
}

#[tokio::test]
async fn needs_an_api_key_with_manage_library() {
    let app = app().await;
    let (status, body, _) = get(&app, "/api/torznab/all/api?t=caps").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("<error code=\"100\""), "{body}");
    let (status, _, _) = get(&app, "/api/torznab/all/api?t=caps&apikey=wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = get(
        &app,
        &format!("/api/torznab/all/api?t=search&apikey={}", app.user_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("<error code=\"101\""));
    let (status, _, _) = get(
        &app,
        &format!("/api/torznab/all/dl?link=x&apikey={}", app.user_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
