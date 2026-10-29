//! Integration routes against an in-memory fake backend: permissions, body handling, status codes.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, ApiError, ApiResult, AppState, Integrations, IntegrationsBackend};
use reelix_api_types::integrations::*;
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct Fake(Mutex<Vec<ImportListExclusionDto>>);

impl IntegrationsBackend for Fake {
    fn exclusions(&self) -> BoxFuture<'_, ApiResult<Vec<ImportListExclusionDto>>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().clone()) })
    }
    fn add_exclusion(&self, req: CreateExclusionRequest) -> BoxFuture<'_, ApiResult<ImportListExclusionDto>> {
        Box::pin(async move {
            let mut v = self.0.lock().unwrap();
            if v.iter().any(|e| e.tmdb_id == req.tmdb_id && e.kind == req.kind) {
                return Err(ApiError::Conflict("dup".into()));
            }
            let dto = ImportListExclusionDto {
                id: v.len() as i64 + 1,
                kind: req.kind,
                tmdb_id: req.tmdb_id,
                title: req.title,
                year: req.year,
                created_at: chrono::Utc::now(),
            };
            v.push(dto.clone());
            Ok(dto)
        })
    }
    fn delete_exclusions(&self, ids: Vec<i64>) -> BoxFuture<'_, ApiResult<u64>> {
        Box::pin(async move {
            let mut v = self.0.lock().unwrap();
            let before = v.len();
            v.retain(|e| !ids.is_empty() && !ids.contains(&e.id));
            Ok((before - v.len()) as u64)
        })
    }
    fn list_status(&self) -> BoxFuture<'_, ApiResult<Vec<ImportListStatusDto>>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn availability(&self) -> BoxFuture<'_, ApiResult<MediaServerAvailabilityDto>> {
        Box::pin(async { Ok(MediaServerAvailabilityDto::default()) })
    }
}

async fn call(r: &Router, m: Method, uri: &str, cookie: &str, body: Option<Value>) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(m)
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
    let res = r.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn cookie_from(r: &Router, uri: &str, body: Value) -> String {
    let res = r
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success());
    res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn integration_routes() {
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
    let state = AppState::new(
        db.clone(),
        events,
        system.queue(),
        SecretBox::from_key([7u8; 32]),
        config,
    )
    .with_extension(Arc::new(Integrations(Arc::new(Fake::default()))));
    let r = reelix_api::router(state);
    let admin = cookie_from(
        &r,
        "/api/v1/setup",
        json!({"username": "admin", "password": "correct-horse-battery"}),
    )
    .await;
    db.users()
        .create(NewUser {
            username: "mgr".into(),
            email: None,
            display_name: None,
            password_hash: Some(reelix_api::auth::hash_password("user-password").unwrap()),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_MANAGER,
        })
        .await
        .unwrap();
    let mgr = cookie_from(
        &r,
        "/api/v1/auth/login",
        json!({"username": "mgr", "password": "user-password"}),
    )
    .await;

    let (s, _) = call(&r, Method::GET, "/api/v1/importlists/exclusions", &mgr, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let body = json!({"kind": "movie", "tmdb_id": 1, "title": "A"});
    let (s, v) = call(
        &r,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &admin,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["tmdb_id"], 1);
    let (s, _) = call(
        &r,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &admin,
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &r,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &admin,
        Some(json!({"kind": "movie", "tmdb_id": 2, "title": " "})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &r,
        Method::DELETE,
        "/api/v1/importlists/exclusions",
        &admin,
        Some(json!({"ids": [1]})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &r,
        Method::DELETE,
        "/api/v1/importlists/exclusions/1",
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Static routes win over the generic provider `/importlists/{id}` routes.
    let (s, v) = call(&r, Method::GET, "/api/v1/importlists/status", &admin, None).await;
    assert_eq!((s, v), (StatusCode::OK, json!([])));
    let (s, v) = call(&r, Method::POST, "/api/v1/importlists/sync", &admin, None).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let job = db
        .jobs()
        .get(v["job_id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.kind, reelix_api::IMPORT_LIST_SYNC);
    let (s, v) = call(&r, Method::GET, "/api/v1/mediaservers/availability", &admin, None).await;
    assert_eq!((s, v["servers"].clone()), (StatusCode::OK, json!([])));
    let doc = reelix_api::openapi();
    assert!(doc.paths.paths.contains_key("/api/v1/importlists/exclusions"));
}
