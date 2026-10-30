//! Media server adapters against mocked HTTP.

use std::path::PathBuf;

use reelix_core::MediaKind;
use reelix_mediaserver::plextv::PlexTv;
use reelix_mediaserver::{BuildContext, MediaServer, MediaServerKind, MsError, build, schemas};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

fn ctx() -> BuildContext {
    BuildContext::new(reqwest::Client::new(), "reelix-test-client")
}

fn server(imp: &str, settings: Value) -> Box<dyn MediaServer> {
    build(imp, &settings, &ctx()).unwrap()
}

fn jf_auth() -> String {
    format!(
        "MediaBrowser Client=\"Reelix\", Device=\"Reelix\", DeviceId=\"reelix-test-client\", Version=\"{}\", Token=\"jfkey\"",
        reelix_core::VERSION
    )
}

#[tokio::test]
async fn jellyfin_test_users_and_auth() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jf/System/Info"))
        .and(header_exists("Authorization"))
        .respond_with(ok("jellyfin_system_info.json"))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/jf/Users"))
        .respond_with(ok("jellyfin_users.json"))
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/jf/Users/AuthenticateByName"))
        .and(body_json(json!({ "Username": "alice", "Pw": "secret" })))
        .respond_with(ok("jellyfin_auth.json"))
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/jf/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    let jf = server(
        "jellyfin",
        json!({ "url": format!("{}/jf/", ms.uri()), "api_key": "jfkey" }),
    );
    assert_eq!(jf.kind(), MediaServerKind::Jellyfin);
    let info = jf.test().await.unwrap();
    assert_eq!(info.name, "living-room");
    assert_eq!(info.version, "10.10.3");

    let users = jf.users().await.unwrap();
    assert_eq!(users.len(), 2);
    assert!(users[0].is_admin);
    assert!(
        users[1]
            .avatar_url
            .as_deref()
            .unwrap()
            .ends_with("/Images/Primary?tag=9f8e7d6c5b4a")
    );

    let alice = jf.authenticate("alice", "secret").await.unwrap();
    assert_eq!(alice.id, "a1b2c3d4e5f60718293a4b5c6d7e8f90");
    assert_eq!(
        alice.access_token.as_deref(),
        Some("8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a")
    );
    assert!(!alice.is_admin);
    assert!(matches!(
        jf.authenticate("alice", "wrong").await,
        Err(MsError::InvalidCredentials)
    ));
    // The login request must not carry the API key.
    let reqs = ms.received_requests().await.unwrap();
    assert_eq!(reqs[0].headers["authorization"].to_str().unwrap(), jf_auth());
    assert!(reqs[0].headers.get("x-emby-token").is_none());
    let login = reqs
        .iter()
        .find(|r| r.url.path().ends_with("AuthenticateByName"))
        .unwrap();
    assert!(
        !login.headers["authorization"]
            .to_str()
            .unwrap()
            .contains("Token=")
    );
}

#[tokio::test]
async fn jellyfin_bad_key_is_unauthorized() {
    let ms = MockServer::start().await;
    Mock::given(path("/System/Info"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    let jf = server("jellyfin", json!({ "url": ms.uri(), "api_key": "nope" }));
    assert!(matches!(jf.test().await, Err(MsError::Unauthorized)));
}

#[tokio::test]
async fn jellyfin_library_items_and_path_refresh() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("IncludeItemTypes", "Movie,Series"))
        .and(query_param("Recursive", "true"))
        .and(query_param("StartIndex", "0"))
        .respond_with(ok("jellyfin_items.json"))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/Library/Media/Updated"))
        .and(body_json(json!({ "Updates": [
            { "Path": "/media/movies/The Lighthouse Keeper (2023)", "UpdateType": "Modified" }
        ]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/Library/Refresh"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&ms)
        .await;
    let jf = server(
        "jellyfin",
        json!({ "url": ms.uri(), "api_key": "jfkey", "path_mappings": ["/data/movies=/media/movies"] }),
    );
    let items = jf.library_items().await.unwrap();
    assert_eq!(items.len(), 2, "music videos are skipped");
    let movie = &items[0];
    assert_eq!(movie.kind, MediaKind::Movie);
    assert_eq!(movie.tmdb_id, Some(912_345));
    assert_eq!(movie.imdb_id.as_deref(), Some("tt9123450"));
    assert_eq!(
        movie.added_at.unwrap().to_rfc3339(),
        "2024-01-15T20:41:07.123456700+00:00"
    );
    let show = &items[1];
    assert_eq!((show.kind, show.tvdb_id), (MediaKind::Series, Some(380_001)));

    jf.refresh_library(&[PathBuf::from("/data/movies/The Lighthouse Keeper (2023)")])
        .await
        .unwrap();
    jf.refresh_library(&[]).await.unwrap();
}

#[tokio::test]
async fn jellyfin_paginates() {
    let ms = MockServer::start().await;
    let page = |start: usize, n: usize| {
        let items: Vec<Value> = (start..start + n)
            .map(|i| json!({ "Id": format!("id{i}"), "Name": format!("Movie {i}"), "Type": "Movie" }))
            .collect();
        json!({ "Items": items, "TotalRecordCount": 520 })
    };
    Mock::given(path("/Items"))
        .and(query_param("StartIndex", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(0, 500)))
        .mount(&ms)
        .await;
    Mock::given(path("/Items"))
        .and(query_param("StartIndex", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(500, 20)))
        .mount(&ms)
        .await;
    let jf = server("jellyfin", json!({ "url": ms.uri(), "api_key": "k" }));
    let items = jf.library_items().await.unwrap();
    assert_eq!(items.len(), 520);
    assert_eq!(items[519].id, "id519");
}

#[tokio::test]
async fn emby_headers_and_watch_history() {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/emby/Users/u42/Items"))
        .and(header("X-Emby-Token", "embykey"))
        .and(header_exists("X-Emby-Authorization"))
        .and(query_param("Filters", "IsPlayed"))
        .and(query_param("SortBy", "DatePlayed"))
        .respond_with(ok("emby_played.json"))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/emby/System/Info"))
        .and(header("X-Emby-Token", "embykey"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ServerName": "emby-box", "Version": "4.8.10.0", "Id": "e1"
        })))
        .mount(&ms)
        .await;
    let emby = server(
        "emby",
        json!({ "url": format!("{}/emby", ms.uri()), "api_key": "embykey" }),
    );
    assert_eq!(emby.test().await.unwrap().name, "emby-box");
    let h = emby.watch_history("u42").await.unwrap();
    assert_eq!(h.len(), 2);
    let ep = &h[0];
    assert_eq!(ep.kind, MediaKind::Series);
    assert_eq!(ep.series_title.as_deref(), Some("Harbor Lights"));
    assert_eq!(ep.series_id.as_deref(), Some("77001"));
    assert_eq!((ep.season, ep.episode, ep.play_count), (Some(2), Some(5), 2));
    assert!(ep.watched_at.is_some());
    assert_eq!(h[1].kind, MediaKind::Movie);
    assert_eq!(h[1].season, None);
    let reqs = ms.received_requests().await.unwrap();
    assert!(reqs[0].headers.get("authorization").is_none());
}

async fn plex_mock() -> MockServer {
    let ms = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("X-Plex-Token", "ownertoken"))
        .and(header("Accept", "application/json"))
        .respond_with(ok("plex_root.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/library/sections"))
        .respond_with(ok("plex_sections.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/library/sections/1/all"))
        .and(query_param("includeGuids", "1"))
        .and(query_param("X-Plex-Container-Start", "0"))
        .respond_with(ok("plex_movies.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/library/sections/2/all"))
        .respond_with(ok("plex_shows.json"))
        .mount(&ms)
        .await;
    ms
}

fn plex(ms: &MockServer) -> Box<dyn MediaServer> {
    server(
        "plex",
        json!({ "url": ms.uri(), "token": "ownertoken",
                "plex_tv_url": format!("{}/plextv", ms.uri()),
                "discover_url": format!("{}/discover", ms.uri()),
                "path_mappings": ["/data/tv=/media/tv"] }),
    )
}

#[tokio::test]
async fn plex_test_and_library_items() {
    let ms = plex_mock().await;
    let p = plex(&ms);
    let info = p.test().await.unwrap();
    assert_eq!(info.name, "Basement Plex");
    assert_eq!(info.id, "9c1e7f3a2b5d4c6e8f0a1b2c3d4e5f60718293a4");
    let items = p.library_items().await.unwrap();
    assert_eq!(items.len(), 2, "music section is ignored");
    let m = &items[0];
    assert_eq!(
        (m.kind, m.tmdb_id, m.tvdb_id),
        (MediaKind::Movie, Some(912_345), Some(55501))
    );
    assert!(m.path.as_deref().unwrap().ends_with("Bluray-1080p.mkv"));
    assert_eq!(m.added_at.unwrap().timestamp(), 1_705_351_267);
    assert_eq!(items[1].kind, MediaKind::Series);
    assert_eq!(items[1].imdb_id.as_deref(), Some("tt1200010"));
    assert!(matches!(
        p.authenticate("a", "b").await,
        Err(MsError::Unsupported(_))
    ));
}

#[tokio::test]
async fn plex_refresh_targets_matching_section_with_path() {
    let ms = plex_mock().await;
    Mock::given(path("/library/sections/2/refresh"))
        .and(query_param("path", "/media/tv/Harbor Lights/Season 02"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(path("/library/sections/1/refresh"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    let p = plex(&ms);
    // Mapped into section 2; an unknown path is skipped.
    p.refresh_library(&[
        PathBuf::from("/data/tv/Harbor Lights/Season 02"),
        PathBuf::from("/elsewhere/x"),
    ])
    .await
    .unwrap();
    let before = ms.received_requests().await.unwrap().len();
    // Full refresh hits each movie/show section (1 and 2), not music.
    Mock::given(path("/library/sections/2/refresh"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&ms)
        .await;
    p.refresh_library(&[]).await.unwrap();
    let reqs = ms.received_requests().await.unwrap();
    let refreshed: Vec<_> = reqs[before..]
        .iter()
        .filter(|r| r.url.path().ends_with("/refresh"))
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        refreshed,
        ["/library/sections/1/refresh", "/library/sections/2/refresh"]
    );
}

#[tokio::test]
async fn plex_history_and_users() {
    let ms = plex_mock().await;
    Mock::given(path("/status/sessions/history/all"))
        .and(query_param("accountID", "2"))
        .and(query_param("sort", "viewedAt:desc"))
        .respond_with(ok("plex_history.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/plextv/api/v2/user"))
        .and(header("X-Plex-Token", "ownertoken"))
        .respond_with(ok("plextv_user.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/plextv/api/v2/friends"))
        .respond_with(ok("plextv_friends.json"))
        .mount(&ms)
        .await;
    let p = plex(&ms);
    let h = p.watch_history("2").await.unwrap();
    assert_eq!(h.len(), 2, "tracks are skipped");
    assert_eq!(h[0].series_title.as_deref(), Some("Harbor Lights"));
    assert_eq!((h[0].season, h[0].episode), (Some(2), Some(5)));
    assert_eq!(h[1].kind, MediaKind::Movie);
    assert_eq!(h[1].watched_at.unwrap().timestamp(), 1_791_925_331);

    let users = p.users().await.unwrap();
    assert_eq!(users.len(), 3);
    assert!(users[0].is_admin && users[0].access_token.is_none());
    assert_eq!(users[0].username, "basementowner");
    assert_eq!(users[1].username, "alice_watches");
    assert_eq!(users[2].username, "Kids", "falls back to title");
}

#[tokio::test]
async fn plextv_pin_flow_account_and_servers() {
    let ms = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/pins"))
        .and(query_param("strong", "true"))
        .and(header("X-Plex-Client-Identifier", "reelix-test-client"))
        .and(header("X-Plex-Product", "Reelix"))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("plextv_pin.json")))
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/pins/1938475610"))
        .respond_with(ok("plextv_pin.json"))
        .up_to_n_times(1)
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/pins/1938475610"))
        .respond_with(ok("plextv_pin_authed.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/api/v2/user"))
        .and(header("X-Plex-Token", "pLxTok3nAbCdEf123456"))
        .respond_with(ok("plextv_user.json"))
        .mount(&ms)
        .await;
    Mock::given(path("/api/v2/resources"))
        .and(query_param("includeHttps", "1"))
        .respond_with(ok("plextv_resources.json"))
        .mount(&ms)
        .await;
    let tv = PlexTv::new(&ctx()).with_base_urls(&ms.uri(), &ms.uri());
    let pin = tv
        .create_pin(Some("https://reelix.example/login/plex"))
        .await
        .unwrap();
    assert_eq!(pin.code, "x7kq2m9vtb3w5r8ndcg6hjpls");
    assert!(
        pin.auth_url.starts_with(
            "https://app.plex.tv/auth#?clientID=reelix-test-client&code=x7kq2m9vtb3w5r8ndcg6hjpls"
        )
    );
    assert!(pin.auth_url.contains("context%5Bdevice%5D%5Bproduct%5D=Reelix"));
    assert!(
        pin.auth_url
            .contains("forwardUrl=https%3A%2F%2Freelix.example%2Flogin%2Fplex")
    );

    assert_eq!(tv.check_pin(pin.id).await.unwrap(), None);
    let token = tv.check_pin(pin.id).await.unwrap().unwrap();
    assert_eq!(token, "pLxTok3nAbCdEf123456");

    let acct = tv.account(&token).await.unwrap();
    assert_eq!(acct.id, "1203344");
    assert_eq!(acct.email.as_deref(), Some("owner@example.com"));

    let servers = tv.servers(&token).await.unwrap();
    assert_eq!(servers.len(), 1, "players are filtered out");
    assert!(servers[0].owned);
    assert_eq!(servers[0].access_token.as_deref(), Some("srvTok3n999"));
    assert_eq!(servers[0].connections.len(), 2);
    assert!(servers[0].connections[0].local);
}

#[tokio::test]
async fn plextv_watchlist_resolves_missing_ids() {
    let ms = MockServer::start().await;
    Mock::given(path("/library/sections/watchlist/all"))
        .and(query_param("includeGuids", "1"))
        .and(header("X-Plex-Token", "tok"))
        .respond_with(ok("discover_watchlist.json"))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(path("/library/metadata/5d776b59ad5437001f79c6f8"))
        .respond_with(ok("discover_metadata.json"))
        .expect(1)
        .mount(&ms)
        .await;
    let tv = PlexTv::new(&ctx()).with_base_urls(&ms.uri(), &ms.uri());
    let wl = tv.watchlist("tok", true).await.unwrap();
    assert_eq!(wl.len(), 2);
    assert_eq!((wl[0].kind, wl[0].tvdb_id), (MediaKind::Series, Some(440_044)));
    assert_eq!((wl[1].kind, wl[1].tmdb_id), (MediaKind::Movie, Some(770_011)));
    assert_eq!(wl[1].imdb_id.as_deref(), Some("tt7700110"));
}

#[test]
fn schemas_and_settings_validation() {
    let s = schemas();
    let ids: Vec<_> = s.iter().map(|s| s.implementation.as_str()).collect();
    assert_eq!(
        ids,
        [
            "jellyfin",
            "emby",
            "plex",
            "audiobookshelf",
            "kavita",
            "calibreweb",
            "navidrome"
        ]
    );
    for schema in &s {
        assert!(schema.secret_fields().count() >= 1);
        assert!(matches!(
            build(&schema.implementation, &json!({}), &ctx()),
            Err(MsError::InvalidSettings(_))
        ));
    }
    assert!(matches!(
        build("jellyfin", &json!({ "url": "ftp://x", "api_key": "k" }), &ctx()),
        Err(MsError::InvalidSettings(_))
    ));
    assert!(matches!(
        build(
            "plex",
            &json!({ "url": "http://x", "token": "t", "path_mappings": ["bad"] }),
            &ctx()
        ),
        Err(MsError::InvalidSettings(_))
    ));
    assert!(matches!(
        build("kodi", &json!({}), &ctx()),
        Err(MsError::UnknownImplementation(_))
    ));
}
