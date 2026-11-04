#![cfg(feature = "direct")]
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::{AuthSource, Permissions, bus::EventBus};
use reelix_db::{Db, NewUser, secrets::SecretBox};
use reelix_jobs::JobSystem;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

async fn call(
    r: &Router,
    method: &str,
    path: &str,
    cookie: &str,
    body: Value,
    csrf: bool,
) -> (StatusCode, Value, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/v1{path}"))
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json");
    if csrf {
        req = req.header("x-reelix-request", "1");
    }
    let res = r
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}

#[tokio::test]
async fn downloads_require_admin_and_csrf_and_roundtrip_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let jobs = JobSystem::builder(db.clone(), events.clone()).build();
    let service = Arc::new(reelix_direct::DirectService::new(
        db.clone(),
        dir.path().to_path_buf(),
    ));
    let state = AppState::new(
        db.clone(),
        events,
        jobs.queue(),
        SecretBox::from_key([7; 32]),
        ApiConfig {
            url_base: String::new(),
            data_dir: dir.path().to_path_buf(),
            database: "sqlite".into(),
            start_time: chrono::Utc::now(),
        },
    )
    .with_extension(service);
    let r = reelix_api::router(state);
    let (_, _, admin) = call(
        &r,
        "POST",
        "/setup",
        "",
        json!({"username":"admin","password":"correct-horse-battery"}),
        true,
    )
    .await;
    db.users()
        .create(NewUser {
            username: "manager".into(),
            email: None,
            display_name: None,
            password_hash: Some(reelix_api::auth::hash_password("user-password").unwrap()),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_MANAGER,
        })
        .await
        .unwrap();
    let (_, _, manager) = call(
        &r,
        "POST",
        "/auth/login",
        "",
        json!({"username":"manager","password":"user-password"}),
        true,
    )
    .await;
    for path in [
        "/downloads",
        "/downloads/settings",
        "/downloads/health",
        "/downloads/spotify",
    ] {
        assert_eq!(
            call(&r, "GET", path, "", Value::Null, true).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&r, "GET", path, &manager, Value::Null, true).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for path in ["/downloads/spotify/login", "/downloads/spotify/disconnect"] {
        assert_eq!(
            call(&r, "POST", path, &manager, Value::Null, true).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, spotify, _) = call(&r, "GET", "/downloads/spotify", &admin, Value::Null, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(spotify["connected"], json!(false));
    let request = json!({"url":"https://youtu.be/abcdefghijk", "source":"youtube", "mode":"audio", "format":"mp3", "max_height":null,"playlist":false});
    assert_eq!(
        call(&r, "POST", "/downloads", &admin, request.clone(), false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "POST", "/downloads", &admin, request.clone(), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, mut settings, _) = call(&r, "GET", "/downloads/settings", &admin, Value::Null, true).await;
    settings["enabled"] = json!(true);
    assert_eq!(
        call(&r, "PUT", "/downloads/settings", &admin, settings, true)
            .await
            .0,
        StatusCode::OK
    );
    let (status, job, _) = call(&r, "POST", "/downloads", &admin, request.clone(), true).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["state"], "queued");
    assert_eq!(
        call(&r, "POST", "/downloads", &admin, request, true).await.0,
        StatusCode::BAD_REQUEST
    );
    let id = job["id"].as_i64().unwrap();
    assert_eq!(
        call(
            &r,
            "POST",
            &format!("/downloads/{id}/cancel"),
            &manager,
            Value::Null,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "POST",
            &format!("/downloads/{id}/cancel"),
            &admin,
            Value::Null,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &r,
            "POST",
            &format!("/downloads/{id}/retry"),
            &admin,
            Value::Null,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, list, _) = call(&r, "GET", "/downloads", &admin, Value::Null, true).await;
    assert_eq!(list[0]["id"], id);
    assert_eq!(list[0]["state"], "queued");
}
