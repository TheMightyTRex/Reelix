//! API flows for subtitles (fork issue #62); the service is covered in reelix-subtitles.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_subtitles::{Options, SubtitleService};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestApp {
    router: Router,
    state: AppState,
    _dir: tempfile::TempDir,
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
    let secrets = SecretBox::from_key([7u8; 32]);
    let subtitles = Arc::new(SubtitleService::new(
        db.clone(),
        events.clone(),
        secrets.clone(),
        reqwest::Client::new(),
        Options::default(),
    ));
    subtitles.set_queue(system.queue());
    let state = AppState::new(db, events, system.queue(), secrets, config).with_extension(subtitles);
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn cookie_from(app: &TestApp, uri: &str, body: Value, want: StatusCode) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), want);
    let sc = res.headers()[header::SET_COOKIE].to_str().unwrap();
    sc.split(';').next().unwrap().to_owned()
}

async fn admin(app: &TestApp) -> String {
    cookie_from(
        app,
        "/api/v1/setup",
        json!({"username": "admin", "password": "correct-horse-battery"}),
        StatusCode::CREATED,
    )
    .await
}

async fn manager(app: &TestApp) -> String {
    let hash = reelix_api::auth::hash_password("user-password").unwrap();
    app.state
        .db()
        .users()
        .create(NewUser {
            username: "manager".into(),
            email: None,
            display_name: None,
            password_hash: Some(hash),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_MANAGER,
        })
        .await
        .unwrap();
    cookie_from(
        app,
        "/api/v1/auth/login",
        json!({"username": "manager", "password": "user-password"}),
        StatusCode::OK,
    )
    .await
}

async fn add_movie_file(db: &Db) -> i64 {
    let movie: i64 = sqlx::query_scalar(
        "INSERT INTO movies (tmdb_id, imdb_id, title, sort_title, clean_title, year, quality_profile_id,
            root_folder_id, path, created_at, updated_at)
         VALUES (949, 'tt0113277', 'Heat', 'heat', 'heat', 1995, 1, 1, '/films/Heat (1995)', 0, 0) RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO movie_files (movie_id, relative_path, size, quality, date_added, created_at, updated_at)
         VALUES (?, 'Heat (1995).mkv', 1, '{}', 0, 0, 0) RETURNING id",
    )
    .bind(movie)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn settings_are_off_by_default_admin_only_and_masked() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = manager(&app).await;

    let (s, _) = call(&app, Method::GET, "/api/v1/settings/subtitles", &manager, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, body) = call(&app, Method::GET, "/api/v1/settings/subtitles", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["languages"], json!([]));

    // switched on without a source: refused
    let (s, body) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/subtitles",
        &admin,
        Some(json!({"enabled": true, "languages": [{"code": "en"}]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");

    let settings = json!({
        "enabled": true,
        "languages": [{"code": "EN", "normal": true, "sdh": true, "forced": false}],
        "opensubtitles": {"enabled": true, "api_key": "secret-key", "username": "someone", "password": "pw"},
        "stremio_addons": ["https://addon.example/manifest.json"]
    });
    let (s, body) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/subtitles",
        &admin,
        Some(settings),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["languages"][0]["code"], "en");
    assert_eq!(body["opensubtitles"]["api_key"], "********");
    assert_eq!(body["opensubtitles"]["password"], "********");
    assert_eq!(body["stremio_addons"], json!(["https://addon.example/"]));

    // the secrets are stored encrypted
    let raw: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'subtitles'")
        .fetch_one(app.state.db().pool())
        .await
        .unwrap();
    assert!(!raw.contains("secret-key") && !raw.contains("\"pw\""), "{raw}");

    // sending the mask back keeps them
    let mut again = body.clone();
    again["upgrade"] = json!(false);
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/subtitles",
        &admin,
        Some(again),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let svc = app.state.ext::<SubtitleService>().unwrap();
    let stored = svc.settings().await.unwrap();
    assert_eq!(stored.opensubtitles.api_key, "secret-key");
    assert_eq!(stored.opensubtitles.password, "pw");
    assert!(!stored.upgrade);
}

#[tokio::test]
async fn search_now_needs_subtitles_on() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = manager(&app).await;
    let file = add_movie_file(app.state.db()).await;

    let uri = format!("/api/v1/subtitles/movie/{file}/search");
    let (s, body) = call(&app, Method::POST, &uri, &manager, None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");

    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/subtitles",
        &admin,
        Some(json!({"enabled": true, "languages": [{"code": "en"}], "stremio_addons": ["https://addon.example"]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, body) = call(&app, Method::POST, &uri, &manager, None).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{body}");
    assert!(body["job_id"].as_i64().is_some());

    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/subtitles/movie/999/search",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::GET,
        &format!("/api/v1/subtitles/song/{file}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, body) = call(
        &app,
        Method::GET,
        &format!("/api/v1/subtitles/movie/{file}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, json!([]));
}
