//! API flows for interactive search + grab, blocklist and pending releases.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_api_types::providers::{IndexerSettingsDto, ProviderInput, ProviderKind};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_pipeline::search::{SearchOptions, SearchService};
use reelix_pipeline::store::{blocklist, pending};
use reelix_providers::{ProviderOptions, ProviderService};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p@example.invalid" date="1777440000" subject="x [1/1] - &quot;x.rar&quot; yEnc (1/1)">
    <groups><group>alt.binaries.example</group></groups>
    <segments><segment bytes="768000" number="1">a@example.invalid</segment></segments>
  </file>
</nzb>"#;

const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps><limits max="100" default="100" /><searching>
<search available="yes" supportedParams="q" />
<tv-search available="yes" supportedParams="q,tvdbid,season,ep" />
<movie-search available="yes" supportedParams="q,imdbid" />
</searching><categories><category id="2000" name="Movies" /></categories></caps>"#;

struct TestApp {
    router: Router,
    state: AppState,
    db: Db,
    indexer: MockServer,
    _sab: MockServer,
    indexer_id: i64,
    _dir: tempfile::TempDir,
}

fn feed(base: &str, items: &[(&str, &str)]) -> String {
    let date = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc2822();
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel>"#,
    );
    for (title, guid) in items {
        s.push_str(&format!(
            r#"<item><title>{title}</title><guid isPermaLink="false">{guid}</guid><link>{base}/getnzb/{guid}.nzb</link><pubDate>{date}</pubDate><newznab:attr name="size" value="4294967296" /></item>"#
        ));
    }
    s.push_str("</channel></rss>");
    s
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
    let indexer = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CAPS, "application/xml"))
        .mount(&indexer)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "movie"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            feed(
                &indexer.uri(),
                &[
                    ("The.Matrix.1999.1080p.BluRay.x264-GRP", "good"),
                    ("Other.Film.2005.1080p.BluRay.x264-GRP", "other"),
                ],
            ),
            "application/rss+xml",
        ))
        .mount(&indexer)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/getnzb/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(NZB, "application/x-nzb"))
        .mount(&indexer)
        .await;
    let sab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex("/api$"))
        .and(query_param("mode", "addfile"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": true, "nzo_ids": ["SABnzbd_nzo_9"]})),
        )
        .mount(&sab)
        .await;
    let idx = providers
        .create(
            ProviderKind::Indexer,
            &ProviderInput {
                implementation: "newznab".into(),
                name: "Idx".into(),
                enabled: true,
                priority: 25,
                tags: vec![],
                settings: json!({"base_url": indexer.uri(), "api_key": "k"}),
            },
        )
        .await
        .unwrap();
    providers
        .create(
            ProviderKind::DownloadClient,
            &ProviderInput {
                implementation: "sabnzbd".into(),
                name: "SAB".into(),
                enabled: true,
                priority: 1,
                tags: vec![],
                settings: json!({"host": "127.0.0.1", "port": sab.address().port(), "api_key": "s"}),
            },
        )
        .await
        .unwrap();
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    let search = Arc::new(SearchService::new(
        db.clone(),
        events.clone(),
        providers.clone(),
        library.clone(),
        SearchOptions::default(),
    ));
    let state = AppState::new(
        db.clone(),
        events,
        system.queue(),
        SecretBox::from_key([5u8; 32]),
        config,
    )
    .with_extension(providers)
    .with_extension(library)
    .with_extension(search);
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        db,
        indexer,
        _sab: sab,
        indexer_id: idx.id,
        _dir: dir,
    }
}

async fn call(
    app: &TestApp,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header("x-reelix-request", "1");
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

async fn login(app: &TestApp, username: &str, password: &str) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": username, "password": password}).to_string(),
        ))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn admin(app: &TestApp) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": "admin", "password": "correct-horse-battery"}).to_string(),
        ))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn plain_user(app: &TestApp) -> String {
    let hash = reelix_api::auth::hash_password("user-password").unwrap();
    app.state
        .db()
        .users()
        .create(NewUser {
            username: "viewer".into(),
            email: None,
            display_name: None,
            password_hash: Some(hash),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_USER,
        })
        .await
        .unwrap();
    login(app, "viewer", "user-password").await
}

async fn add_movie(db: &Db) -> i64 {
    db.quality_profiles().list().await.unwrap();
    let profile: i64 = sqlx::query_scalar("SELECT id FROM quality_profiles WHERE name = 'HD-1080p'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let root: i64 = sqlx::query_scalar(
        "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES ('/m', 'movie', 0, 0) RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO movies (tmdb_id, imdb_id, title, sort_title, clean_title, year, status, runtime, monitored, \
         quality_profile_id, root_folder_id, path, created_at, updated_at) \
         VALUES (603, 'tt0133093', 'The Matrix', 'matrix', 'matrix', 1999, 'released', 120, 1, ?, ?, '/m/The Matrix (1999)', 0, 0) RETURNING id",
    )
    .bind(profile)
    .bind(root)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn interactive_search_and_grab() {
    let app = app().await;
    let admin = admin(&app).await;
    let viewer = plain_user(&app).await;
    let movie = add_movie(&app.db).await;

    let (s, _) = call(
        &app,
        Method::GET,
        &format!("/api/v1/releases?movie_id={movie}"),
        &viewer,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&app, Method::GET, "/api/v1/releases", &admin, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&app, Method::GET, "/api/v1/releases?movie_id=999", &admin, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, rows) = call(
        &app,
        Method::GET,
        &format!("/api/v1/releases?movie_id={movie}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rows}");
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["guid"], "good");
    assert_eq!(rows[0]["accepted"], true);
    assert_eq!(rows[0]["rank"], 2);
    assert_eq!(rows[0]["movie_id"], movie);
    assert_eq!(rows[1]["accepted"], false);
    assert_eq!(rows[1]["rejections"][0]["spec"], "mapping");

    // Rejected → 400 unless forced.
    let grab = |guid: &str, force: bool| json!({"guid": guid, "indexer_id": app.indexer_id, "force": force});
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/releases/grab",
        &admin,
        Some(grab("other", false)),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/releases/grab",
        &admin,
        Some(grab("unknown", false)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, r) = call(
        &app,
        Method::POST,
        "/api/v1/releases/grab",
        &admin,
        Some(grab("good", false)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["download_client"], "SAB");
    assert_eq!(r["download_id"], "SABnzbd_nzo_9");
    assert!(r["queue_id"].as_i64().unwrap() > 0);

    // Raw grab from the indexer search page.
    let (s, r) = call(
        &app,
        Method::POST,
        "/api/v1/releases/grab",
        &admin,
        Some(json!({
            "guid": "raw-1",
            "indexer_id": app.indexer_id,
            "download_url": format!("{}/getnzb/raw-1.nzb", app.indexer.uri()),
            "title": "Some.Release.2020.1080p.WEB-DL-X",
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
}

#[tokio::test]
async fn blocklist_and_pending_routes() {
    let app = app().await;
    let admin = admin(&app).await;
    let movie = add_movie(&app.db).await;
    let mut ids = Vec::new();
    for t in ["A", "B", "C"] {
        ids.push(
            blocklist::insert(
                &app.db,
                &blocklist::NewBlocklist {
                    movie_id: Some(movie),
                    source_title: t.into(),
                    message: Some("failed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
    }
    let (s, page) = call(
        &app,
        Method::GET,
        "/api/v1/blocklist?page=1&page_size=2",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["media"]["title"], "The Matrix");

    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/blocklist/{}", ids[0]),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/blocklist/{}", ids[0]),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::DELETE,
        "/api/v1/blocklist",
        &admin,
        Some(json!({"ids": [ids[1]]})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, page) = call(&app, Method::GET, "/api/v1/blocklist", &admin, None).await;
    assert_eq!(page["total"], 1);
    let (s, _) = call(&app, Method::DELETE, "/api/v1/blocklist", &admin, None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, page) = call(&app, Method::GET, "/api/v1/blocklist", &admin, None).await;
    assert_eq!(page["total"], 0);

    // Pending: list, grab now, delete.
    let release = reelix_pipeline::GrabbedRelease {
        guid: "p1".into(),
        title: "The.Matrix.1999.1080p.BluRay.x264-GRP".into(),
        indexer_id: Some(app.indexer_id),
        indexer: "Idx".into(),
        protocol: reelix_core::Protocol::Usenet,
        size: Some(4 << 30),
        publish_date: Some(chrono::Utc::now()),
        download_url: Some(format!("{}/getnzb/p1.nzb", app.indexer.uri())),
        magnet_url: None,
        info_url: None,
        info_hash: None,
        seeders: None,
        quality: reelix_parser::QualityModel::new(reelix_parser::Quality::Bluray1080p),
        languages: vec![],
        release_group: Some("GRP".into()),
        custom_formats: vec![],
        format_score: 0,
        indexer_flags: vec![],
        seed_ratio: None,
        seed_time_minutes: None,
        soulseek: None,
    };
    let far = (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp_millis();
    let p1 = pending::insert(&app.db, Some(movie), None, &[], &release, "delay", far)
        .await
        .unwrap();
    let (s, list) = call(&app, Method::GET, "/api/v1/pending", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list[0]["id"], p1);
    assert_eq!(list[0]["media"]["title"], "The Matrix");
    assert_eq!(list[0]["reason"], "delay");
    let (s, r) = call(
        &app,
        Method::POST,
        &format!("/api/v1/pending/{p1}/grab"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (_, list) = call(&app, Method::GET, "/api/v1/pending", &admin, None).await;
    assert_eq!(list, json!([]));
    let p2 = pending::insert(&app.db, Some(movie), None, &[], &release, "delay", far)
        .await
        .unwrap();
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/pending/{p2}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/pending/{p2}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
