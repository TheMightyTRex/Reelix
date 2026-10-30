//! Book and music server adapters (Audiobookshelf, Kavita, Calibre-Web, Navidrome) against
//! mocked HTTP.

use std::path::PathBuf;

use reelix_core::ContentKind;
use reelix_mediaserver::{BuildContext, MediaServer, MediaServerKind, MsError, build, navidrome};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn ctx() -> BuildContext {
    BuildContext::new(reqwest::Client::new(), "reelix-test-client")
}

fn server(imp: &str, settings: Value) -> Box<dyn MediaServer> {
    build(imp, &settings, &ctx()).unwrap()
}

// ---------------------------------------------------------------- Audiobookshelf

fn abs_libraries() -> Value {
    json!({ "libraries": [
        { "id": "lib-audio", "name": "Audiobooks", "mediaType": "book",
          "folders": [{ "id": "f1", "fullPath": "/audiobooks" }] },
        { "id": "lib-ebooks", "name": "Ebooks", "mediaType": "book",
          "folders": [{ "id": "f2", "fullPath": "/ebooks" }] },
        { "id": "lib-pod", "name": "Podcasts", "mediaType": "podcast",
          "folders": [{ "id": "f3", "fullPath": "/podcasts" }] }
    ]})
}

async fn abs_mock() -> MockServer {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/libraries"))
        .and(header("Authorization", "Bearer abstoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(abs_libraries()))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/libraries"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "isInit": true, "serverVersion": "2.17.2" })),
        )
        .mount(&ms)
        .await;
    ms
}

#[tokio::test]
async fn audiobookshelf_test_and_bad_token() {
    let ms = abs_mock().await;
    let abs = server(
        "audiobookshelf",
        json!({ "url": ms.uri(), "api_key": "abstoken" }),
    );
    assert_eq!(abs.kind(), MediaServerKind::Audiobookshelf);
    assert_eq!(abs.content_kinds(), [ContentKind::Audiobook]);
    let info = abs.test().await.unwrap();
    assert_eq!(info.version, "2.17.2");
    assert!(abs.library_items().await.unwrap().is_empty());
    assert!(matches!(
        abs.authenticate("a", "b").await,
        Err(MsError::Unsupported(_))
    ));

    let bad = server("audiobookshelf", json!({ "url": ms.uri(), "api_key": "nope" }));
    assert!(matches!(bad.test().await, Err(MsError::Unauthorized)));

    let with_ebooks = server(
        "audiobookshelf",
        json!({ "url": ms.uri(), "api_key": "abstoken", "ebooks": true }),
    );
    assert_eq!(
        with_ebooks.content_kinds(),
        [ContentKind::Audiobook, ContentKind::Ebook]
    );
}

#[tokio::test]
async fn audiobookshelf_scans_the_library_containing_the_path() {
    let ms = abs_mock().await;
    Mock::given(method("POST"))
        .and(path("/api/libraries/lib-audio/scan"))
        .and(header("Authorization", "Bearer abstoken"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/libraries/lib-ebooks/scan"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/libraries/lib-pod/scan"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&ms)
        .await;
    let abs = server(
        "audiobookshelf",
        json!({
            "url": ms.uri(),
            "api_key": "abstoken",
            "path_mappings": ["/data/audiobooks=/audiobooks"]
        }),
    );
    // Two files of the same book → one scan; an unrelated path → none.
    abs.refresh_library(&[
        PathBuf::from("/data/audiobooks/Frank Herbert/Dune/part1.m4b"),
        PathBuf::from("/data/audiobooks/Frank Herbert/Dune/part2.m4b"),
        PathBuf::from("/data/movies/Dune (2021)"),
    ])
    .await
    .unwrap();
    // Empty paths → every book library (not podcasts).
    abs.refresh_library(&[]).await.unwrap();
}

// ---------------------------------------------------------------- Kavita

async fn kavita_mock() -> MockServer {
    let ms = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/Plugin/authenticate"))
        .and(query_param("apiKey", "kkey"))
        .and(query_param("pluginName", "Reelix"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "admin", "token": "jwt-1", "refreshToken": "r", "apiKey": "kkey",
            "kavitaVersion": "0.8.4.0"
        })))
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/Plugin/authenticate"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/Server/server-info-slim"))
        .and(header("Authorization", "Bearer jwt-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kavitaVersion": "0.8.5.11", "installId": "install-123", "isDocker": true
        })))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/Library/libraries"))
        .and(header("Authorization", "Bearer jwt-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "Manga", "type": 0, "folders": ["/manga"] },
            { "id": 2, "name": "Books", "type": 2, "folders": ["/books/", "/more-books"] }
        ])))
        .mount(&ms)
        .await;
    ms
}

#[tokio::test]
async fn kavita_exchanges_api_key_for_jwt() {
    let ms = kavita_mock().await;
    let k = server("kavita", json!({ "url": ms.uri(), "api_key": "kkey" }));
    assert_eq!(k.kind(), MediaServerKind::Kavita);
    assert_eq!(k.content_kinds(), [ContentKind::Ebook]);
    let info = k.test().await.unwrap();
    assert_eq!(info.id, "install-123");
    assert_eq!(info.version, "0.8.5.11");
    assert_eq!(info.name, "Kavita (admin)");

    let bad = server("kavita", json!({ "url": ms.uri(), "api_key": "wrong" }));
    assert!(matches!(bad.test().await, Err(MsError::Unauthorized)));
}

#[tokio::test]
async fn kavita_scans_matching_library_or_all() {
    let ms = kavita_mock().await;
    Mock::given(method("POST"))
        .and(path("/api/Library/scan"))
        .and(query_param("libraryId", "2"))
        .and(header("Authorization", "Bearer jwt-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/Library/scan"))
        .and(query_param("libraryId", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/Library/scan-all"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    let k = server("kavita", json!({ "url": ms.uri(), "api_key": "kkey" }));
    k.refresh_library(&[PathBuf::from("/books/Frank Herbert/Dune (1965)/Dune.epub")])
        .await
        .unwrap();
    k.refresh_library(&[]).await.unwrap();
}

#[tokio::test]
async fn kavita_renews_an_expired_jwt_once() {
    let ms = MockServer::start().await;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    Mock::given(method("POST"))
        .and(path("/api/Plugin/authenticate"))
        .respond_with(move |_: &Request| {
            let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            ResponseTemplate::new(200)
                .set_body_json(json!({ "username": "admin", "token": format!("jwt-{n}") }))
        })
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/Library/scan-all"))
        .and(header("Authorization", "Bearer jwt-2"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/Library/scan-all"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    let k = server("kavita", json!({ "url": ms.uri(), "api_key": "kkey" }));
    k.refresh_library(&[]).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------- Calibre-Web

const OPDS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Calibre-Web</title></feed>"#;

const LOGIN_PAGE: &str = r#"<html><form method="post">
<input id="csrf_token" name="csrf_token" type="hidden" value="csrf-abc">
<input name="username"><input name="password" type="password"></form></html>"#;

// "alice:secret"
const BASIC: &str = "Basic YWxpY2U6c2VjcmV0";

#[tokio::test]
async fn calibreweb_test_uses_opds_basic_auth() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/opds"))
        .and(header("Authorization", BASIC))
        .respond_with(ResponseTemplate::new(200).set_body_string(OPDS))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/opds"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    let cw = server(
        "calibreweb",
        json!({ "url": ms.uri(), "username": "alice", "password": "secret" }),
    );
    assert_eq!(cw.kind(), MediaServerKind::CalibreWeb);
    assert_eq!(cw.content_kinds(), [ContentKind::Ebook]);
    assert_eq!(cw.test().await.unwrap().name, "Calibre-Web");
    let bad = server(
        "calibreweb",
        json!({ "url": ms.uri(), "username": "alice", "password": "wrong" }),
    );
    assert!(matches!(bad.test().await, Err(MsError::Unauthorized)));
}

async fn calibreweb_login_mock() -> MockServer {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Set-Cookie", "session=pre; Path=/")
                .set_body_string(LOGIN_PAGE),
        )
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .and(body_string_contains("csrf_token=csrf-abc"))
        .and(body_string_contains("password=secret"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "/")
                .insert_header("Set-Cookie", "session=authed; Path=/"),
        )
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOGIN_PAGE))
        .mount(&ms)
        .await;
    ms
}

#[tokio::test]
async fn calibreweb_refresh_logs_in_and_reconnects() {
    let ms = calibreweb_login_mock().await;
    Mock::given(method("GET"))
        .and(path("/reconnect"))
        .and(header("Cookie", "session=authed"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&ms)
        .await;
    let cw = server(
        "calibreweb",
        json!({ "url": ms.uri(), "username": "alice", "password": "secret" }),
    );
    cw.refresh_library(&[PathBuf::from("/books/x.epub")])
        .await
        .unwrap();

    let bad = server(
        "calibreweb",
        json!({ "url": ms.uri(), "username": "alice", "password": "wrong" }),
    );
    assert!(matches!(
        bad.refresh_library(&[]).await,
        Err(MsError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn calibreweb_refresh_without_reconnect_route_is_ok() {
    let ms = calibreweb_login_mock().await;
    Mock::given(method("GET"))
        .and(path("/reconnect"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&ms)
        .await;
    let cw = server(
        "calibreweb",
        json!({ "url": ms.uri(), "username": "alice", "password": "secret" }),
    );
    cw.refresh_library(&[]).await.unwrap();
}

// ---------------------------------------------------------------- Navidrome

/// Accepts only requests whose `t` equals md5(password + `s`).
fn subsonic_ok(password: &'static str, body: Value) -> impl Fn(&Request) -> ResponseTemplate {
    move |req: &Request| {
        let q: std::collections::HashMap<_, _> = req.url.query_pairs().into_owned().collect();
        let valid = q.get("u").map(String::as_str) == Some("admin")
            && q.get("v").map(String::as_str) == Some(navidrome::API_VERSION)
            && q.get("c").map(String::as_str) == Some("Reelix")
            && q.get("f").map(String::as_str) == Some("json")
            && !q.contains_key("p")
            && match (q.get("t"), q.get("s")) {
                (Some(t), Some(s)) => *t == navidrome::token(password, s),
                _ => false,
            };
        let inner = if valid {
            let mut b = json!({ "status": "ok", "version": "1.16.1", "type": "navidrome",
                                "serverVersion": "0.53.3 (13af8ed4)", "openSubsonic": true });
            if let (Some(o), Some(extra)) = (b.as_object_mut(), body.as_object()) {
                o.extend(extra.clone());
            }
            b
        } else {
            json!({ "status": "failed", "version": "1.16.1",
                    "error": { "code": 40, "message": "Wrong username or password" } })
        };
        ResponseTemplate::new(200).set_body_json(json!({ "subsonic-response": inner }))
    }
}

#[test]
fn navidrome_token_known_vector() {
    // From the Subsonic API documentation.
    assert_eq!(
        navidrome::token("sesame", "c19b2d"),
        "26719a1196d2a940705a59634eb18eab"
    );
}

#[tokio::test]
async fn navidrome_ping_with_token_auth() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/ping"))
        .respond_with(subsonic_ok("hunter2", json!({})))
        .mount(&ms)
        .await;
    let nd = server(
        "navidrome",
        json!({ "url": ms.uri(), "username": "admin", "password": "hunter2" }),
    );
    assert_eq!(nd.kind(), MediaServerKind::Navidrome);
    assert_eq!(nd.content_kinds(), [ContentKind::Music]);
    let info = nd.test().await.unwrap();
    assert_eq!(info.name, "Navidrome");
    assert_eq!(info.version, "0.53.3 (13af8ed4)");

    let bad = server(
        "navidrome",
        json!({ "url": ms.uri(), "username": "admin", "password": "wrong" }),
    );
    assert!(matches!(bad.test().await, Err(MsError::Unauthorized)));
}

#[tokio::test]
async fn navidrome_start_scan_and_status() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/startScan"))
        .respond_with(subsonic_ok(
            "hunter2",
            json!({ "scanStatus": { "scanning": true, "count": 10 } }),
        ))
        .expect(2)
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/getScanStatus"))
        .respond_with(subsonic_ok(
            "hunter2",
            json!({ "scanStatus": { "scanning": false, "count": 4242, "folderCount": 7 } }),
        ))
        .mount(&ms)
        .await;
    let settings = json!({
        "url": ms.uri(), "username": "admin", "password": "hunter2",
        "music_folders": ["/data/music"]
    });
    let nd = server("navidrome", settings.clone());
    nd.refresh_library(&[PathBuf::from("/data/music/Artist/Album (2020)")])
        .await
        .unwrap();
    // Outside the configured music folders: no scan.
    nd.refresh_library(&[PathBuf::from("/data/movies/Dune (2021)")])
        .await
        .unwrap();
    nd.refresh_library(&[]).await.unwrap();

    let typed = navidrome::Navidrome::from_settings(&settings, &ctx()).unwrap();
    let st = typed.scan_status().await.unwrap();
    assert!(!st.scanning);
    assert_eq!(st.count, 4242);
}

#[tokio::test]
async fn navidrome_non_admin_scan_is_reported() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/startScan"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "subsonic-response": {
            "status": "failed", "version": "1.16.1",
            "error": { "code": 50, "message": "User not authorized" } } })),
        )
        .mount(&ms)
        .await;
    let nd = server(
        "navidrome",
        json!({ "url": ms.uri(), "username": "admin", "password": "x" }),
    );
    assert!(matches!(
        nd.refresh_library(&[]).await,
        Err(MsError::Status { status: 403, .. })
    ));
}

#[test]
fn kind_helpers() {
    for k in [
        MediaServerKind::Audiobookshelf,
        MediaServerKind::Kavita,
        MediaServerKind::CalibreWeb,
        MediaServerKind::Navidrome,
    ] {
        assert!(!k.is_video());
        assert_eq!(
            serde_json::to_value(k).unwrap(),
            Value::String(k.implementation().into())
        );
    }
    assert!(MediaServerKind::Plex.is_video());
}
