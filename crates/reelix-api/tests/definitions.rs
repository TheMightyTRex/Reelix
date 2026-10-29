//! API flows for the indexer definition catalog: status, settings, sync and permissions.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState, HealthSource, HealthSources};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_providers::{ProviderOptions, ProviderService};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    let svc = ProviderService::new(
        db.clone(),
        SecretBox::from_key([7u8; 32]),
        events.clone(),
        reqwest::Client::new(),
        ProviderOptions::new(dir.path()),
    )
    .await
    .unwrap();
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(svc.clone())
        .with_extension(Arc::new(HealthSources(vec![svc as Arc<dyn HealthSource>])));
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

/// A fictional definition (original, for tests only).
fn def_yaml(id: &str, name: &str) -> String {
    format!(
        r#"id: {id}
name: {name}
description: Fictional
language: en-US
type: public
links:
  - https://{id}.invalid/
caps:
  categories:
    movies: Movies
  modes:
    search: [q]
search:
  paths:
    - path: api/list
      response:
        type: json
  rows:
    selector: items
  fields:
    title:
      selector: title
    infohash:
      selector: hash
"#
    )
}

#[tokio::test]
async fn definitions_status_settings_and_sync() {
    let app = app().await;
    let admin = admin(&app).await;
    let svc = app.state.ext::<ProviderService>().unwrap();

    let (st, body) = call(
        &app,
        Method::GET,
        "/api/v1/indexers/definitions/status",
        &admin,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["schema_version"], "v11");
    assert_eq!(body["loaded"], 0);
    assert_eq!(body["failed"], json!([]));
    assert!(body["custom_dir"].as_str().unwrap().ends_with("custom"));

    // Custom definitions (one good, one broken) appear after a reload.
    let custom = app._dir.path().join("definitions/custom");
    std::fs::write(custom.join("echo.yml"), def_yaml("echo", "Echo Fictional")).unwrap();
    std::fs::write(custom.join("bad.yml"), "id: bad\n").unwrap();
    svc.reload_definitions().await.unwrap();
    let (_, body) = call(
        &app,
        Method::GET,
        "/api/v1/indexers/definitions/status",
        &admin,
        None,
    )
    .await;
    assert_eq!(body["custom_count"], 2);
    assert_eq!(body["loaded"], 1, "{body}");
    assert_eq!(body["failed"][0]["id"], "bad");
    assert_eq!(body["failed"][0]["file"], "bad.yml");
    assert_eq!(body["failed"][0]["source"], "custom");
    assert!(body["failed"][0]["error"].as_str().is_some());
    let (_, schemas) = call(&app, Method::GET, "/api/v1/indexers/schema", &admin, None).await;
    assert!(
        schemas
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["schema"]["implementation"] == "definition:echo")
    );

    // Settings round-trip and validation.
    let (st, body) = call(&app, Method::GET, "/api/v1/settings/definitions", &admin, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["auto_update_hours"], 24);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let mut s = body.clone();
    s["source_url"] = json!("ftp://example.invalid/x.tgz");
    let (st, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/definitions",
        &admin,
        Some(s.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    s["source_url"] = json!(format!("{}/catalog.tgz", server.uri()));
    s["auto_update_hours"] = json!(12);
    let (st, body) = call(&app, Method::PUT, "/api/v1/settings/definitions", &admin, Some(s)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["auto_update_hours"], 12);

    // Sync now → 202 with a queued job (deduplicated while queued).
    let (st, body) = call(
        &app,
        Method::POST,
        "/api/v1/indexers/definitions/sync",
        &admin,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    assert!(body["job_id"].as_i64().is_some());
    let (st, body) = call(
        &app,
        Method::POST,
        "/api/v1/indexers/definitions/sync",
        &admin,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert!(body["job_id"].is_null());

    // What the worker does: the failing source is reported in the status.
    assert!(svc.sync_definitions(true).await.is_err());
    let (_, body) = call(
        &app,
        Method::GET,
        "/api/v1/indexers/definitions/status",
        &admin,
        None,
    )
    .await;
    assert!(body["last_error"].as_str().unwrap().contains("503"), "{body}");
    assert_eq!(body["loaded"], 1, "custom definitions still loaded");
}

#[tokio::test]
async fn definitions_routes_are_admin_only() {
    let app = app().await;
    let _admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    for (m, uri, body) in [
        (Method::GET, "/api/v1/indexers/definitions/status", None),
        (Method::POST, "/api/v1/indexers/definitions/sync", None),
        (Method::GET, "/api/v1/settings/definitions", None),
        (
            Method::PUT,
            "/api/v1/settings/definitions",
            Some(json!({"enabled": false})),
        ),
    ] {
        let (st, _) = call(&app, m, uri, &manager, body).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{uri}");
    }
    let (st, _) = call(&app, Method::GET, "/api/v1/indexers/definitions/status", "", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn definitions_paths_in_openapi() {
    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    for p in [
        "/api/v1/indexers/definitions/sync",
        "/api/v1/indexers/definitions/status",
        "/api/v1/settings/definitions",
    ] {
        assert!(doc["paths"][p].is_object(), "{p}");
    }
}
