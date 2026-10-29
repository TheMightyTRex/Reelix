//! API flows for queue, history and manual import (service behaviour is covered in reelix-pipeline).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions, Protocol};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_parser::{Quality, QualityModel};
use reelix_pipeline::store::history::{self, HistoryEvent, NewHistory};
use reelix_pipeline::store::tracked::{self, NewTracked};
use reelix_pipeline::{ActivityService, GrabbedRelease, StaticProviders};
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
    let activity = Arc::new(ActivityService::new(
        db.clone(),
        events.clone(),
        library.clone(),
        Arc::new(StaticProviders::default()),
    ));
    activity.set_queue(system.queue());
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(library)
        .with_extension(activity);
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
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

async fn plain_user(app: &TestApp) -> String {
    let hash = reelix_api::auth::hash_password("user-password").unwrap();
    app.state
        .db()
        .users()
        .create(NewUser {
            username: "plain".into(),
            email: None,
            display_name: None,
            password_hash: Some(hash),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_USER,
        })
        .await
        .unwrap();
    cookie_from(
        app,
        "/api/v1/auth/login",
        json!({"username": "plain", "password": "user-password"}),
        StatusCode::OK,
    )
    .await
}

fn release(title: &str) -> GrabbedRelease {
    GrabbedRelease {
        guid: "g".into(),
        title: title.into(),
        indexer_id: Some(1),
        indexer: "Idx".into(),
        protocol: Protocol::Usenet,
        size: Some(1000),
        publish_date: None,
        download_url: None,
        magnet_url: None,
        info_url: None,
        info_hash: None,
        seeders: None,
        quality: QualityModel::new(Quality::WebDl1080p),
        languages: vec![],
        release_group: Some("GRP".into()),
        custom_formats: vec![],
        format_score: 0,
        indexer_flags: vec![],
        seed_ratio: None,
        seed_time_minutes: None,
    }
}

async fn track(db: &Db, id: &str, title: &str) -> i64 {
    tracked::insert(
        db,
        &NewTracked {
            download_client_id: 1,
            download_id: id.into(),
            protocol: Protocol::Usenet,
            title: title.into(),
            movie_id: None,
            series_id: None,
            episode_ids: vec![],
            release: release(title),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn queue_and_history_routes() {
    let app = app().await;
    let admin = admin(&app).await;
    let plain = plain_user(&app).await;
    let db = app.state.db().clone();
    let a = track(&db, "nzo_a", "Some.Release.A.1080p").await;
    let b = track(&db, "nzo_b", "Some.Release.B.1080p").await;

    let (s, _) = call(&app, Method::GET, "/api/v1/queue", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, q) = call(
        &app,
        Method::GET,
        "/api/v1/queue?page=1&page_size=1",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(q["total"], 2);
    assert_eq!(q["items"].as_array().unwrap().len(), 1);
    assert_eq!(q["items"][0]["state"], "downloading");
    assert_eq!(q["items"][0]["indexer"], "Idx");

    let (s, job) = call(
        &app,
        Method::POST,
        &format!("/api/v1/queue/{a}/import"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(job["job_id"].is_i64());

    let (s, _) = call(
        &app,
        Method::POST,
        &format!("/api/v1/queue/{b}/ignore"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/queue/{a}?blocklist=true&skip_redownload=true"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(&app, Method::DELETE, "/api/v1/queue/9999", &admin, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, q) = call(&app, Method::GET, "/api/v1/queue", &admin, None).await;
    assert_eq!(q["total"], 0);

    // History: the ignore row plus a grab we mark as failed.
    let mut h = NewHistory::new(HistoryEvent::Grabbed, "Some.Release.C.1080p");
    h.download_id = Some("nzo_c".into());
    let hid = history::insert(&db, &h).await.unwrap();
    let (s, list) = call(&app, Method::GET, "/api/v1/history", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 2);
    let (s, _) = call(&app, Method::GET, "/api/v1/history?event_type=nope", &admin, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        Method::POST,
        &format!("/api/v1/history/{hid}/failed"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = call(
        &app,
        Method::GET,
        "/api/v1/history?event_type=download_failed",
        &admin,
        None,
    )
    .await;
    assert_eq!(list["total"], 1);
    let ignored = call(
        &app,
        Method::GET,
        "/api/v1/history?event_type=ignored",
        &admin,
        None,
    )
    .await
    .1;
    let iid = ignored["items"][0]["id"].as_i64().unwrap();
    let (s, _) = call(
        &app,
        Method::POST,
        &format!("/api/v1/history/{iid}/failed"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, total) = reelix_pipeline::store::blocklist::list(&db, 0, 10).await.unwrap();
    assert_eq!(total, 2);
}

#[tokio::test]
async fn manual_import_routes_validate() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, _) = call(&app, Method::GET, "/api/v1/manualimport", &admin, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/manualimport?folder=/etc",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/manualimport?download_id=nope",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/manualimport",
        &admin,
        Some(json!({"files": [{"path": "/x.mkv"}]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, job) = call(
        &app,
        Method::POST,
        "/api/v1/manualimport",
        &admin,
        Some(json!({"files": [{"path": "/x.mkv", "movie_id": 1}], "import_mode": "copy"})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(job["job_id"].is_i64());
}
