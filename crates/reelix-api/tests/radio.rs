//! API flows and permissions for the radio module against a fake provider (service flows are
//! covered in reelix-radio).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_radio::provider::async_trait;
use reelix_radio::{
    EpisodeData, NewEpisodeFile, ProviderError, ProviderRegistry, RadioDeps, RadioProvider, RadioService,
    SearchPrefs, SeriesData, ShowData, ShowFetch, ShowHit,
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestApp {
    router: Router,
    state: AppState,
    radio: Arc<RadioService>,
    _dir: tempfile::TempDir,
}

struct FakeBroadcaster;

fn show() -> ShowData {
    ShowData {
        external_id: "p001".into(),
        title: "The Archers".into(),
        series: vec![SeriesData {
            external_id: "s1".into(),
            number: Some(1),
            ..SeriesData::default()
        }],
        episodes: (1..=3)
            .map(|n| EpisodeData {
                external_id: format!("e{n}"),
                series_external_id: "s1".into(),
                number: Some(n),
                title: format!("Episode {n}"),
                first_broadcast: Some(Utc.with_ymd_and_hms(2024, 1, n, 19, 0, 0).unwrap()),
                ..EpisodeData::default()
            })
            .collect(),
        complete: true,
        ..ShowData::default()
    }
}

#[async_trait]
impl RadioProvider for FakeBroadcaster {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    fn supports_search(&self) -> bool {
        true
    }
    async fn search(&self, _q: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Ok(vec![show().hit("fake")])
    }
    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(link.starts_with("https://fake.example/").then(show))
    }
    async fn show(&self, _id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        Ok(show())
    }
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
    let radio = Arc::new(RadioService::new(RadioDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(Arc::new(FakeBroadcaster)),
        library_sync: None,
    }));
    radio.set_queue(system.queue());
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(radio.clone());
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        radio,
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
async fn radio_permissions() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, show) = call(
        &app,
        Method::POST,
        "/api/v1/radio/shows",
        &admin,
        Some(json!({"provider": "fake", "external_id": "p001"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{show}");
    let id = show["id"].as_i64().unwrap();
    let eps = app.radio.show_episodes(id).await.unwrap().episodes;
    let ep = eps[0].id;

    // A plain user (requests only) can read nothing.
    let user = user_with(&app, "listener", Permissions::ROLE_USER).await;
    for uri in [
        "/api/v1/radio/shows".to_owned(),
        format!("/api/v1/radio/shows/{id}"),
        format!("/api/v1/radio/shows/{id}/episodes"),
        format!("/api/v1/radio/episodes/{ep}"),
        "/api/v1/radio/search?q=archers".to_owned(),
        "/api/v1/radio/preview?link=https://fake.example/p001".to_owned(),
        "/api/v1/radio/stations".to_owned(),
        "/api/v1/radio/providers".to_owned(),
        "/api/v1/radio/library/review".to_owned(),
        "/api/v1/radio/rootfolders".to_owned(),
        "/api/v1/radio/settings".to_owned(),
    ] {
        let (s, _) = call(&app, Method::GET, &uri, &user, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
    }
    for (m, uri, body) in [
        (
            Method::POST,
            "/api/v1/radio/shows".to_owned(),
            Some(json!({"link": "https://fake.example/x"})),
        ),
        (
            Method::PUT,
            format!("/api/v1/radio/shows/{id}"),
            Some(json!({"monitored": false})),
        ),
        (Method::DELETE, format!("/api/v1/radio/shows/{id}"), None),
        (Method::POST, format!("/api/v1/radio/shows/{id}/refresh"), None),
        (
            Method::PUT,
            format!("/api/v1/radio/episodes/{ep}/file"),
            Some(json!({"file_id": 1})),
        ),
        (Method::DELETE, format!("/api/v1/radio/episodes/{ep}/file"), None),
        (
            Method::POST,
            "/api/v1/command".to_owned(),
            Some(json!({"name": "RescanRadioLibrary"})),
        ),
    ] {
        let (s, _) = call(&app, m.clone(), &uri, &user, body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {uri}");
    }

    // A library manager reads and writes, but not the settings or root folders.
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let (s, list) = call(&app, Method::GET, "/api/v1/radio/shows", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 1);
    let (s, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/radio/shows/{id}"),
        &manager,
        Some(json!({"follow": "none"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(&app, Method::GET, "/api/v1/radio/settings", &manager, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/radio/settings",
        &manager,
        Some(json!({"default_follow": "none"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/radio/rootfolders",
        &manager,
        Some(json!({"path": "/tmp", "kind": "radio"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // The admin manages the settings.
    let (s, body) = call(&app, Method::GET, "/api/v1/radio/settings", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["default_follow"], "all");
    let (s, body) = call(
        &app,
        Method::PUT,
        "/api/v1/radio/settings",
        &admin,
        Some(json!({"library_folder": "/definitely/not/mounted"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(body["message"].as_str().unwrap().contains("mounted"), "{body}");
    let (s, body) = call(
        &app,
        Method::PUT,
        "/api/v1/radio/settings",
        &admin,
        Some(json!({"default_follow": "from_now_on", "country": "gb"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["country"], "GB");
}

#[tokio::test]
async fn radio_shows_episodes_matches_and_commands() {
    let app = app().await;
    let admin = admin(&app).await;

    // Preview and search save nothing.
    let (s, p) = call(
        &app,
        Method::GET,
        "/api/v1/radio/preview?link=https://fake.example/p001",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["episode_count"], 3);
    assert_eq!(p["show"]["show_id"], Value::Null);
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/radio/preview?link=https://elsewhere/",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, hits) = call(&app, Method::GET, "/api/v1/radio/search?q=archers", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hits[0]["title"], "The Archers");

    let (s, show) = call(
        &app,
        Method::POST,
        "/api/v1/radio/shows",
        &admin,
        Some(json!({"link": "https://fake.example/p001", "follow": "all"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{show}");
    let id = show["id"].as_i64().unwrap();
    assert_eq!(show["statistics"]["wanted_count"], 3);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/radio/shows",
        &admin,
        Some(json!({"provider": "fake", "external_id": "p001"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(&app, Method::GET, "/api/v1/radio/shows/999", &admin, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, eps) = call(
        &app,
        Method::GET,
        &format!("/api/v1/radio/shows/{id}/episodes"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(eps["series"].as_array().unwrap().len(), 1);
    assert_eq!(eps["episodes"][0]["status"], "wanted");
    let ep = eps["episodes"][0]["id"].as_i64().unwrap();

    // Manual matches.
    let (s, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/radio/episodes/{ep}/file"),
        &admin,
        Some(json!({"file_id": 42})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let file = app
        .radio
        .upsert_episode_files(
            &[NewEpisodeFile {
                abs_item_id: "li_1".into(),
                abs_episode_id: "ep_1".into(),
                ..NewEpisodeFile::default()
            }],
            Utc::now(),
        )
        .await
        .unwrap()[0];
    let (s, e) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/radio/episodes/{ep}/file"),
        &admin,
        Some(json!({"file_id": file})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{e}");
    assert_eq!(
        (e["status"].as_str(), e["manual_match"].as_bool()),
        (Some("in_library"), Some(true))
    );
    let (s, review) = call(&app, Method::GET, "/api/v1/radio/library/review", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(review["unmatched_files"].as_array().unwrap().len(), 0);
    let (s, e) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/radio/episodes/{ep}/file?auto=true"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (e["status"].as_str(), e["manual_match"].as_bool()),
        (Some("wanted"), Some(false))
    );

    // Edits, commands, stations.
    let (s, edited) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/radio/shows/{id}"),
        &admin,
        Some(json!({"follow": "none", "root_folder_id": 77})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{edited}");
    let (s, edited) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/radio/shows/{id}"),
        &admin,
        Some(json!({"follow": "none", "abs_item_id": "li_1"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(edited["statistics"]["wanted_count"], 0);
    assert_eq!(edited["abs_item_id"], "li_1");
    let (s, job) = call(
        &app,
        Method::POST,
        &format!("/api/v1/radio/shows/{id}/refresh"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(job["job_id"].as_i64().is_some());
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/command",
        &admin,
        Some(json!({"name": "RefreshRadioShow", "show_ids": [id]})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/command",
        &admin,
        Some(json!({"name": "RefreshRadioShow", "show_ids": [999]})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, job) = call(
        &app,
        Method::POST,
        "/api/v1/command",
        &admin,
        Some(json!({"name": "RescanRadioLibrary"})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(job["job_id"].as_i64().is_some());
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/command",
        &admin,
        Some(json!({"name": "Nope"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, st) = call(&app, Method::GET, "/api/v1/radio/stations", &admin, None).await;
    assert_eq!((s, st.as_array().unwrap().len()), (StatusCode::OK, 0));
    let (s, p) = call(&app, Method::GET, "/api/v1/radio/providers", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(p[0]["supports_search"], true);

    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/radio/shows/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/radio/shows/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_documents_radio_routes() {
    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    for p in [
        "/api/v1/radio/shows",
        "/api/v1/radio/shows/{id}",
        "/api/v1/radio/shows/{id}/episodes",
        "/api/v1/radio/episodes/{id}/file",
        "/api/v1/radio/search",
        "/api/v1/radio/preview",
        "/api/v1/radio/library/review",
        "/api/v1/radio/settings",
        "/api/v1/command",
    ] {
        assert!(doc["paths"].get(p).is_some(), "{p} missing from OpenAPI");
    }
}
