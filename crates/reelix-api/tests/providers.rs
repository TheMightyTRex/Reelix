//! API flows for providers, tags, remote path mappings and provider settings.

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

#[tokio::test]
async fn provider_crud_masking_and_permissions() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;

    for kind in [
        "indexers",
        "downloadclients",
        "notifications",
        "mediaservers",
        "importlists",
    ] {
        let (s, list) = call(&app, Method::GET, &format!("/api/v1/{kind}"), &admin, None).await;
        assert_eq!(s, StatusCode::OK, "{kind}");
        assert_eq!(list, json!([]));
        let (s, schemas) = call(&app, Method::GET, &format!("/api/v1/{kind}/schema"), &admin, None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!schemas.as_array().unwrap().is_empty());
        let (s, _) = call(&app, Method::GET, &format!("/api/v1/{kind}"), &manager, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    let body = json!({
        "implementation": "sabnzbd", "name": "Sab", "priority": 3,
        "settings": {"host": "sab.lan", "port": 8080, "api_key": "topsecret"}
    });
    let (s, created) = call(
        &app,
        Method::POST,
        "/api/v1/downloadclients",
        &admin,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert_eq!(created["settings"]["api_key"], "********");
    assert_eq!(created["protocol"], "usenet");
    assert_eq!(created["implementation_name"], "SABnzbd");
    let id = created["id"].as_i64().unwrap();

    let (s, _) = call(&app, Method::POST, "/api/v1/downloadclients", &admin, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/downloadclients",
        &admin,
        Some(json!({"implementation": "nope", "name": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let mut edit = created.clone();
    edit["name"] = json!("SAB 2");
    let (s, updated) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/downloadclients/{id}"),
        &admin,
        Some(edit),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], "SAB 2");
    let svc = app.state.ext::<ProviderService>().unwrap();
    let row = svc
        .get(reelix_providers::ProviderKind::DownloadClient, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.setting_str("api_key"), Some("topsecret"));

    let (s, one) = call(
        &app,
        Method::GET,
        &format!("/api/v1/downloadclients/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["name"], "SAB 2");
    let (s, _) = call(&app, Method::GET, "/api/v1/notifications/999", &admin, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Unsaved test with invalid settings → 200 with a failed check.
    let (s, t) = call(
        &app,
        Method::POST,
        "/api/v1/notifications/test",
        &admin,
        Some(json!({"implementation": "webhook", "name": "w", "settings": {}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["ok"], false);

    // Health includes provider checks.
    let (s, h) = call(&app, Method::GET, "/api/v1/system/health", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    let sources: Vec<&str> = h["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["source"].as_str().unwrap())
        .collect();
    assert!(sources.contains(&"database") && sources.contains(&"indexers") && sources.contains(&"metadata"));

    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/downloadclients/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/downloadclients/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tags_mappings_settings_and_search() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let plain = user_with(&app, "plain", Permissions::ROLE_USER).await;

    let (s, tag) = call(
        &app,
        Method::POST,
        "/api/v1/tags",
        &manager,
        Some(json!({"label": "anime"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/tags",
        &manager,
        Some(json!({"label": "Anime"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, tags) = call(&app, Method::GET, "/api/v1/tags", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tags.as_array().unwrap().len(), 1);
    let (s, _) = call(&app, Method::GET, "/api/v1/tags", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let tid = tag["id"].as_i64().unwrap();
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/tags/{tid}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&app, Method::DELETE, &format!("/api/v1/tags/{tid}"), &admin, None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Local paths must be absolute on the server's platform.
    let (mnt, data) = if cfg!(windows) {
        (r"C:\mnt\dl", r"C:\data")
    } else {
        ("/mnt/dl", "/data")
    };
    let (s, m) = call(
        &app,
        Method::POST,
        "/api/v1/remotepathmappings",
        &admin,
        Some(json!({"host": "nas", "remote_path": "/dl", "local_path": mnt})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let mid = m["id"].as_i64().unwrap();
    let (s, m2) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/remotepathmappings/{mid}"),
        &admin,
        Some(json!({"host": "nas", "remote_path": "/dl", "local_path": data})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m2["local_path"], data);
    let (s, list) = call(&app, Method::GET, "/api/v1/remotepathmappings", &admin, None).await;
    assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 1));
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/remotepathmappings/{mid}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, md) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/metadata",
        &admin,
        Some(json!({"tmdb_api_key": "abc", "language": "de-DE"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{md}");
    assert_eq!(md["tmdb_api_key"], "********");
    let (_, md) = call(&app, Method::GET, "/api/v1/settings/metadata", &admin, None).await;
    assert_eq!(md["language"], "de-DE");
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/metadata",
        &admin,
        Some(json!({"region": "USA"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, is) = call(&app, Method::GET, "/api/v1/settings/indexers", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(is["allow_private_addresses"], false);
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/indexers",
        &admin,
        Some(json!({"rss_interval_minutes": 5})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, r) = call(
        &app,
        Method::GET,
        "/api/v1/indexers/search?q=test",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r, json!({"releases": [], "indexers": []}));
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/indexers/search?q=x&cat=abc",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&app, Method::GET, "/api/v1/indexers/search?q=x", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, cats) = call(&app, Method::GET, "/api/v1/indexers/categories", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(cats.as_array().unwrap().iter().any(|c| c["id"] == 2000));
    let (s, st) = call(&app, Method::GET, "/api/v1/indexers/stats", &admin, None).await;
    assert_eq!((s, st), (StatusCode::OK, json!([])));
}

#[tokio::test]
async fn openapi_lists_provider_paths() {
    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/api/v1/{kind}",
        "/api/v1/{kind}/{id}/test",
        "/api/v1/indexers/search",
        "/api/v1/settings/metadata",
    ] {
        assert!(paths.contains_key(p), "{p}");
    }
}
