//! API flows for the library module (metadata-free paths; service flows are covered in reelix-library).

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
use reelix_library::{DynMetadata, LibraryService};
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
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    library.set_queue(system.queue());
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(library);
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        _dir: dir,
    }
}

/// Calls the API with a cookie (CSRF header included) and returns status + JSON body.
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

async fn login_cookie(app: &TestApp, username: &str, password: &str) -> String {
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
    let sc = res.headers()[header::SET_COOKIE].to_str().unwrap();
    sc.split(';').next().unwrap().to_owned()
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
    let sc = res.headers()[header::SET_COOKIE].to_str().unwrap();
    sc.split(';').next().unwrap().to_owned()
}

async fn user_with(app: &TestApp, name: &str, permissions: Permissions) -> String {
    let hash = reelix_api::auth::hash_password("user-password").unwrap();
    app.state
        .db()
        .users()
        .create(NewUser {
            username: name.into(),
            email: None,
            display_name: None,
            password_hash: Some(hash),
            auth_source: AuthSource::Local,
            permissions,
        })
        .await
        .unwrap();
    login_cookie(app, name, "user-password").await
}

#[tokio::test]
async fn library_routes_permissions_and_settings() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let plain = user_with(&app, "plain", Permissions::ROLE_USER).await;

    let (s, list) = call(&app, Method::GET, "/api/v1/movies", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 0);
    assert_eq!(list["items"], json!([]));
    let (s, _) = call(&app, Method::GET, "/api/v1/movies", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/series?page=1&page_size=10&sort=title&dir=desc",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Root folders: admin only for writes; must exist.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("Some Movie (2001)")).unwrap();
    let body = json!({"path": dir.path().to_str().unwrap(), "media_kind": "movie"});
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/rootfolders",
        &manager,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, rf) = call(
        &app,
        Method::POST,
        "/api/v1/rootfolders",
        &admin,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rf}");
    assert_eq!(rf["unmapped_folders"][0]["name"], "Some Movie (2001)");
    let (s, _) = call(&app, Method::POST, "/api/v1/rootfolders", &admin, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/rootfolders",
        &admin,
        Some(json!({"path": "/definitely/not/here", "media_kind": "series"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Metadata-dependent endpoints report the missing TMDB key.
    let (s, e) = call(
        &app,
        Method::GET,
        "/api/v1/movies/lookup?term=matrix",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{e}");
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/movies",
        &manager,
        Some(json!({
            "tmdb_id": 603, "quality_profile_id": 1, "root_folder_id": rf["id"]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let (s, cands) = call(
        &app,
        Method::GET,
        &format!("/api/v1/libraryimport/scan?root_folder_id={}", rf["id"]),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cands[0]["parsed_title"], "Some Movie");
    assert_eq!(cands[0]["parsed_year"], 2001);

    let (s, _) = call(&app, Method::GET, "/api/v1/movies/42", &manager, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&app, Method::POST, "/api/v1/movies/42/refresh", &manager, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Search hand-off.
    let (s, job) = call(
        &app,
        Method::POST,
        "/api/v1/search",
        &manager,
        Some(json!({"kind": "missing"})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(job["job_id"].is_i64());
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/search",
        &manager,
        Some(json!({"kind": "season", "ids": [1]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Naming settings.
    let (s, mut naming) = call(&app, Method::GET, "/api/v1/settings/naming", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    naming["movie_folder_format"] = json!("{Movie Title} ({Release Year})");
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/naming",
        &manager,
        Some(naming.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, saved) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/naming",
        &admin,
        Some(naming.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(saved["movie_folder_format"], "{Movie Title} ({Release Year})");
    naming["movie_file_format"] = json!("{Movie Title");
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/naming",
        &admin,
        Some(naming.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, p) = call(
        &app,
        Method::POST,
        "/api/v1/naming/preview",
        &manager,
        Some(naming),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(p["errors"][0]["field"], "movie_file_format");
    let (s, p) = call(&app, Method::GET, "/api/v1/naming/preview", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        p["movie_folder"]
            .as_str()
            .unwrap()
            .starts_with("The Movie Title (2010)")
    );

    // Media management.
    let (s, mut mm) = call(
        &app,
        Method::GET,
        "/api/v1/settings/mediamanagement",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mm["import_mode"], "hardlink_or_copy");
    mm["recycle_bin_path"] = json!("relative/bin");
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/mediamanagement",
        &admin,
        Some(mm.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    mm["recycle_bin_path"] = json!(null);
    mm["extra_file_extensions"] = json!([".SRT", "nfo"]);
    let (s, mm) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/mediamanagement",
        &admin,
        Some(mm),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mm["extra_file_extensions"], json!(["srt", "nfo"]));

    // Calendar / wanted / stats.
    let (s, cal) = call(
        &app,
        Method::GET,
        "/api/v1/calendar?start=2024-01-01&end=2024-01-31",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cal, json!([]));
    let (s, w) = call(
        &app,
        Method::GET,
        "/api/v1/wanted/missing?kind=episode&page=1&page_size=5",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(w["page_size"], 5);
    let (s, _) = call(&app, Method::GET, "/api/v1/wanted/cutoff", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, st) = call(&app, Method::GET, "/api/v1/library/stats", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["root_folders"].as_array().unwrap().len(), 1);
    let (s, _) = call(&app, Method::GET, "/api/v1/rename/preview", &manager, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Root folder delete.
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/rootfolders/{}", rf["id"]),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn ical_feed_with_api_key_and_openapi() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, created) = call(
        &app,
        Method::POST,
        "/api/v1/auth/apikeys",
        &admin,
        Some(json!({"name": "cal"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let key = created["key"].as_str().unwrap();
    let req = Request::builder()
        .uri(format!("/api/v1/calendar.ics?apikey={key}"))
        .body(Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/calendar")
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).starts_with("BEGIN:VCALENDAR"));
    let req = Request::builder()
        .uri("/api/v1/calendar.ics")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    for p in [
        "/api/v1/movies",
        "/api/v1/series/{id}/episodes",
        "/api/v1/calendar.ics",
        "/api/v1/settings/naming",
        "/api/v1/search",
    ] {
        assert!(doc["paths"][p].is_object(), "missing {p}");
    }
}
