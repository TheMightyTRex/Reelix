//! ProviderService flows against wiremock (no real network).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use reelix_api_types::HealthLevel;
use reelix_api_types::providers::{
    IndexerSettingsDto, MetadataSettingsDto, ProviderInput, ProviderKind, ProviderTestRequest, ProxyDto,
    SECRET_MASK,
};
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_indexers::SearchQuery;
use reelix_notify::EventKind;
use reelix_providers::{ProviderError, ProviderOptions, ProviderService};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Env {
    svc: Arc<ProviderService>,
    db: Db,
    _dir: tempfile::TempDir,
}

async fn env_with(f: impl FnOnce(&mut ProviderOptions)) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let mut opts = ProviderOptions::new(dir.path());
    opts.test_timeout = std::time::Duration::from_secs(5);
    opts.search_timeout = std::time::Duration::from_secs(5);
    f(&mut opts);
    let svc = ProviderService::new(
        db.clone(),
        SecretBox::from_key([9; 32]),
        EventBus::default(),
        reqwest::Client::new(),
        opts,
    )
    .await
    .unwrap();
    Env { svc, db, _dir: dir }
}

async fn env() -> Env {
    env_with(|_| {}).await
}

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

fn xml(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/rss+xml")
}

async fn torznab_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(xml(fixture("torznab_caps.xml")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "search"))
        .respond_with(xml(fixture("torznab_search.xml")))
        .mount(&server)
        .await;
    server
}

async fn allow_private(svc: &ProviderService) {
    svc.set_indexer_settings(IndexerSettingsDto {
        allow_private_addresses: true,
        ..Default::default()
    })
    .await
    .unwrap();
}

fn indexer_input(base: &str, name: &str) -> ProviderInput {
    ProviderInput {
        implementation: "torznab".into(),
        name: name.into(),
        enabled: true,
        priority: 25,
        tags: vec![],
        settings: json!({"base_url": base, "api_key": "sekrit"}),
    }
}

#[tokio::test]
async fn indexer_crud_secrets_and_ssrf_guard() {
    let e = env().await;
    let server = torznab_server().await;

    // Private (loopback) target is blocked by default.
    let err = e
        .svc
        .create(ProviderKind::Indexer, &indexer_input(&server.uri(), "Local"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProviderError::Validation(ref m) if m.contains("blocked")),
        "{err}"
    );

    allow_private(&e.svc).await;
    let row = e
        .svc
        .create(ProviderKind::Indexer, &indexer_input(&server.uri(), "Local"))
        .await
        .unwrap();
    assert_eq!(row.setting_str("api_key"), Some("sekrit"));
    assert_eq!(row.settings["enable_rss"], true, "augmented defaults filled in");

    // Encrypted at rest.
    let raw: String = sqlx::query_scalar("SELECT settings FROM providers WHERE id = ?")
        .bind(row.id)
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    assert!(!raw.contains("sekrit") && raw.contains("enc:"), "{raw}");

    // Masked on read; mask round-trip keeps the secret.
    let dto = e.svc.to_dto(&row);
    assert_eq!(dto.settings["api_key"], SECRET_MASK);
    assert_eq!(dto.protocol, Some(reelix_core::Protocol::Torrent));
    let mut input = indexer_input(&server.uri(), "Renamed");
    input.settings = dto.settings.clone();
    let updated = e.svc.update(ProviderKind::Indexer, row.id, &input).await.unwrap();
    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.setting_str("api_key"), Some("sekrit"));

    // Duplicate names conflict; validation errors surface.
    let err = e
        .svc
        .create(ProviderKind::Indexer, &indexer_input(&server.uri(), "renamed"))
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Conflict(_)));
    let mut bad = indexer_input(&server.uri(), "X");
    bad.settings = json!({});
    assert!(matches!(
        e.svc.create(ProviderKind::Indexer, &bad).await,
        Err(ProviderError::Validation(_))
    ));
    bad.implementation = "nope".into();
    assert!(matches!(
        e.svc.create(ProviderKind::Indexer, &bad).await,
        Err(ProviderError::Validation(_))
    ));

    // Live instance registered; search works and returns releases.
    assert_eq!(e.svc.indexers().len(), 1);
    let results = e.svc.search(&SearchQuery::basic("anything"), None, true).await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].result.as_ref().unwrap().is_empty());
    assert_eq!(e.svc.indexer_manager().stats()[0].queries, 1);

    // Saved test passes.
    let t = e.svc.test_saved(ProviderKind::Indexer, row.id).await.unwrap();
    assert!(t.ok, "{t:?}");

    // Disable → unregistered; delete → gone.
    input.enabled = false;
    e.svc.update(ProviderKind::Indexer, row.id, &input).await.unwrap();
    assert!(e.svc.indexers().is_empty());
    assert!(e.svc.indexer_manager().ids().is_empty());
    assert!(e.svc.delete(ProviderKind::Indexer, row.id).await.unwrap());
    assert!(!e.svc.delete(ProviderKind::Indexer, row.id).await.unwrap());
    assert!(e.svc.list(ProviderKind::Indexer).await.unwrap().is_empty());
}

#[tokio::test]
async fn failing_indexer_status_is_persisted_and_reported() {
    let e = env().await;
    allow_private(&e.svc).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let row = e
        .svc
        .create(ProviderKind::Indexer, &indexer_input(&server.uri(), "Broken"))
        .await
        .unwrap();
    let results = e.svc.search(&SearchQuery::basic("x"), None, true).await;
    assert!(results[0].result.is_err());
    let failures: i64 = sqlx::query_scalar("SELECT failure_count FROM indexer_status WHERE provider_id = ?")
        .bind(row.id)
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    assert_eq!(failures, 1);
    let dto = e.svc.to_dto(&row);
    assert!(dto.status.unwrap().disabled_until.is_some());
    let health = e.svc.health();
    assert!(
        health.iter().any(|h| h.source == "indexers"
            && h.message.contains("Broken")
            && h.level == HealthLevel::Warning)
    );
    assert!(health.iter().any(|h| h.message.contains("All indexers")));

    // Health job re-tests (still failing) and keeps persisting.
    e.svc.check_indexers().await;
    let failures: i64 = sqlx::query_scalar("SELECT failure_count FROM indexer_status WHERE provider_id = ?")
        .bind(row.id)
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    assert_eq!(failures, 2);

    // Restart restores the status.
    let svc2 = ProviderService::new(
        e.db.clone(),
        SecretBox::from_key([9; 32]),
        EventBus::default(),
        reqwest::Client::new(),
        ProviderOptions::new(e._dir.path()),
    )
    .await
    .unwrap();
    assert_eq!(svc2.indexer_manager().status(row.id).unwrap().failure_count, 2);
}

#[tokio::test]
async fn unsaved_test_uses_stored_secrets_with_id() {
    let e = env().await;
    allow_private(&e.svc).await;
    let server = torznab_server().await;
    let row = e
        .svc
        .create(ProviderKind::Indexer, &indexer_input(&server.uri(), "A"))
        .await
        .unwrap();
    let mut input = indexer_input(&server.uri(), "A");
    input.settings["api_key"] = json!(SECRET_MASK);
    let res = e
        .svc
        .test_input(
            ProviderKind::Indexer,
            &ProviderTestRequest {
                id: Some(row.id),
                provider: input.clone(),
            },
        )
        .await
        .unwrap();
    assert!(res.ok, "{res:?}");
    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter()
            .all(|r| r.url.query().unwrap_or("").contains("apikey=sekrit"))
    );

    // Invalid settings come back as a failed check, not an error.
    input.settings = json!({"base_url": "not a url"});
    let res = e
        .svc
        .test_input(
            ProviderKind::Indexer,
            &ProviderTestRequest {
                id: None,
                provider: input,
            },
        )
        .await
        .unwrap();
    assert!(!res.ok);
    assert_eq!(res.checks[0].name, "settings");
}

#[tokio::test]
async fn unsaved_test_with_changed_host_refuses_stored_secret() {
    let e = env().await;
    allow_private(&e.svc).await;
    let server = torznab_server().await;
    let attacker = torznab_server().await;
    let row = e
        .svc
        .create(ProviderKind::Indexer, &indexer_input(&server.uri(), "A"))
        .await
        .unwrap();
    // Same host, different port: the stored API key must not follow.
    let mut input = indexer_input(&attacker.uri(), "A");
    input.settings["api_key"] = json!(SECRET_MASK);
    let res = e
        .svc
        .test_input(
            ProviderKind::Indexer,
            &ProviderTestRequest {
                id: Some(row.id),
                provider: input.clone(),
            },
        )
        .await
        .unwrap();
    assert!(!res.ok);
    assert_eq!(res.checks[0].name, "settings");
    assert!(res.message.unwrap().contains("re-enter"));
    assert!(attacker.received_requests().await.unwrap().is_empty());

    // Re-entering the secret makes the test go through.
    input.settings["api_key"] = json!("other");
    let res = e
        .svc
        .test_input(
            ProviderKind::Indexer,
            &ProviderTestRequest {
                id: Some(row.id),
                provider: input,
            },
        )
        .await
        .unwrap();
    assert!(res.ok, "{res:?}");
}

#[tokio::test]
async fn email_smtp_server_is_address_checked() {
    let e = env().await;
    let input = ProviderInput {
        implementation: "email".into(),
        name: "Mail".into(),
        enabled: true,
        priority: 25,
        tags: vec![],
        settings: json!({"server": "127.0.0.1", "from": "r@example.com", "to": ["a@example.com"]}),
    };
    let err = e
        .svc
        .create(ProviderKind::Notification, &input)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProviderError::Validation(ref m) if m.contains("blocked")),
        "{err}"
    );
}

#[tokio::test]
async fn notifications_fan_out_by_event_mask_and_ssrf() {
    let e = env().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let input = |name: &str, events: Value| ProviderInput {
        implementation: "webhook".into(),
        name: name.into(),
        enabled: true,
        priority: 25,
        tags: vec![],
        settings: json!({"url": format!("{}/hook", server.uri()), "events": events}),
    };
    // Loopback blocked until allowed.
    assert!(matches!(
        e.svc
            .create(ProviderKind::Notification, &input("g", json!(["grab"])))
            .await,
        Err(ProviderError::Validation(_))
    ));
    allow_private(&e.svc).await;
    e.svc
        .create(ProviderKind::Notification, &input("grabs", json!(["grab"])))
        .await
        .unwrap();
    e.svc
        .create(ProviderKind::Notification, &input("imports", json!(["import"])))
        .await
        .unwrap();
    assert_eq!(e.svc.notifiers().len(), 2);

    let grab = reelix_notify::samples::all()
        .into_iter()
        .find(|ev| ev.kind() == EventKind::Grab)
        .unwrap();
    e.svc.notify(&grab).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // A failing target never propagates.
    let dead = ProviderInput {
        settings: json!({"url": "http://127.0.0.1:9/hook", "events": ["grab"]}),
        ..input("dead", json!([]))
    };
    e.svc.create(ProviderKind::Notification, &dead).await.unwrap();
    e.svc.notify(&grab).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn tags_mappings_and_download_clients() {
    let e = env().await;
    let tag = e.svc.create_tag("4k").await.unwrap();
    assert!(matches!(
        e.svc.create_tag("4K").await,
        Err(ProviderError::Conflict(_))
    ));
    assert!(matches!(
        e.svc.create_tag("  ").await,
        Err(ProviderError::Validation(_))
    ));

    let client = |name: &str, priority: i32, tags: Vec<i64>| ProviderInput {
        implementation: "qbittorrent".into(),
        name: name.into(),
        enabled: true,
        priority,
        tags,
        settings: json!({"host": "nas", "port": 8080, "username": "admin", "password": "pw"}),
    };
    let b = e
        .svc
        .create(ProviderKind::DownloadClient, &client("B", 10, vec![tag.id]))
        .await
        .unwrap();
    e.svc
        .create(ProviderKind::DownloadClient, &client("A", 5, vec![]))
        .await
        .unwrap();
    assert!(matches!(
        e.svc
            .create(ProviderKind::DownloadClient, &client("C", 5, vec![999]))
            .await,
        Err(ProviderError::Validation(_))
    ));
    let clients = e.svc.download_clients();
    assert_eq!(
        clients.iter().map(|(r, _)| r.name.as_str()).collect::<Vec<_>>(),
        ["A", "B"]
    );
    let (_, inst) = e.svc.download_client(b.id).unwrap();
    assert_eq!(inst.info().name, "B");
    assert_eq!(inst.info().tags, ["4k"]);
    assert_eq!(e.svc.download_clients_for(reelix_core::Protocol::Usenet).len(), 0);

    // Deleting a tag strips it from providers.
    assert!(e.svc.delete_tag(tag.id).await.unwrap());
    let b = e
        .svc
        .get(ProviderKind::DownloadClient, b.id)
        .await
        .unwrap()
        .unwrap();
    assert!(b.tags.is_empty());

    // Remote path mappings. Local paths must be absolute on this OS.
    let (dl, x) = if cfg!(windows) {
        (r"C:\mnt\dl", r"C:\x")
    } else {
        ("/mnt/dl", "/x")
    };
    let m = e.svc.create_mapping("nas", "/downloads", dl).await.unwrap();
    assert!(matches!(
        e.svc.create_mapping("nas", "/downloads", x).await,
        Err(ProviderError::Conflict(_))
    ));
    assert!(matches!(
        e.svc.create_mapping("nas", "/x", "relative").await,
        Err(ProviderError::Validation(_))
    ));
    assert_eq!(
        e.svc.map_remote_path("nas", "/downloads/movies/a.mkv"),
        Path::new(dl).join("movies/a.mkv")
    );
    assert_eq!(
        e.svc.map_remote_path("other", "/downloads/a"),
        PathBuf::from("/downloads/a")
    );
    e.svc
        .update_mapping(m.id, "nas", "/downloads", "/data")
        .await
        .unwrap();
    assert_eq!(
        e.svc.map_remote_path("nas", "/downloads/a"),
        PathBuf::from("/data/a")
    );
    assert!(e.svc.delete_mapping(m.id).await.unwrap());

    // Unreachable client shows up in health after a client_health poll.
    e.svc.check_clients().await;
    let health = e.svc.health();
    assert!(
        health
            .iter()
            .any(|h| h.source == "download_clients" && h.level == HealthLevel::Error),
        "{health:?}"
    );
}

#[tokio::test]
async fn metadata_and_indexer_settings_masked() {
    let tmdb = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/genre/movie/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"genres": [{"id": 28, "name": "Action"}]})),
        )
        .mount(&tmdb)
        .await;
    let uri = tmdb.uri();
    let e = env_with(|o| o.tmdb_base_url = Some(uri)).await;
    assert!(e.svc.metadata().is_err());
    assert!(e.svc.health().iter().any(|h| h.source == "metadata"));
    assert!(e.svc.health().iter().any(|h| h.message.contains("No indexers")));

    let saved = e
        .svc
        .set_metadata_settings(MetadataSettingsDto {
            tmdb_api_key: Some("tmdbkey".into()),
            region: Some("us".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(saved.tmdb_api_key.as_deref(), Some(SECRET_MASK));
    assert_eq!(saved.tvdb_api_key, None);
    assert_eq!(saved.region.as_deref(), Some("US"));
    assert!(e.svc.metadata().is_ok());
    assert_eq!(e.svc.metadata_config().tmdb_api_key.as_deref(), Some("tmdbkey"));
    let raw: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'metadata.settings'")
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    assert!(!raw.contains("tmdbkey"));

    // Masked test uses the stored key.
    let res = e.svc.test_metadata(saved.clone()).await.unwrap();
    assert!(res.ok, "{res:?}");
    assert!(
        tmdb.received_requests().await.unwrap()[0]
            .url
            .query()
            .unwrap()
            .contains("api_key=tmdbkey")
    );
    // Saving the masked form back keeps the key.
    e.svc.set_metadata_settings(saved).await.unwrap();
    assert_eq!(e.svc.metadata_config().tmdb_api_key.as_deref(), Some("tmdbkey"));

    // Indexer settings: proxy password masked, validation.
    let s = e
        .svc
        .set_indexer_settings(IndexerSettingsDto {
            rss_interval_minutes: 30,
            proxy: Some(ProxyDto {
                kind: "socks5".into(),
                host: "proxy".into(),
                port: 1080,
                username: Some("u".into()),
                password: Some("pp".into()),
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(s.proxy.as_ref().unwrap().password.as_deref(), Some(SECRET_MASK));
    let again = e.svc.set_indexer_settings(s).await.unwrap();
    assert_eq!(again.proxy.unwrap().password.as_deref(), Some(SECRET_MASK));
    assert!(
        e.svc
            .set_indexer_settings(IndexerSettingsDto {
                rss_interval_minutes: 3,
                ..Default::default()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn schemas_and_definitions_dir() {
    let e = env().await;
    assert!(e._dir.path().join("definitions").is_dir());
    for k in ProviderKind::ALL {
        assert!(!e.svc.schemas(k).is_empty());
    }
    assert!(e.svc.schema(ProviderKind::MediaServer, "plex").is_some());
    assert!(e.svc.schema(ProviderKind::ImportList, "trakt").is_some());
}

#[tokio::test]
async fn plex_pin_flow() {
    let plex = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/pins"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 42, "code": "abcd"})))
        .mount(&plex)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/pins/42"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 42, "code": "abcd", "authToken": "tok"})),
        )
        .mount(&plex)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/resources"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"name": "Home", "clientIdentifier": "c1", "provides": "server", "owned": true,
             "connections": [{"uri": "https://relay", "relay": true}, {"uri": "http://10.0.0.2:32400", "local": true}]}
        ])))
        .mount(&plex)
        .await;
    let uri = plex.uri();
    let e = env_with(|o| o.plex_tv_url = Some(uri)).await;
    let pin = e.svc.plex_create_pin().await.unwrap();
    assert_eq!(pin.id, 42);
    assert!(pin.auth_url.contains("abcd"));
    let st = e.svc.plex_check_pin(42).await.unwrap();
    assert!(st.authorized);
    assert_eq!(st.token.as_deref(), Some("tok"));
    assert_eq!(st.servers[0].uris[0], "http://10.0.0.2:32400");
}
