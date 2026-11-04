//! Serving radio show artwork and station logos: permissions, content types, ETag/304 and the
//! SVG security headers; and the masked Podcast Index secret in the radio settings.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_api_types::radio::AddRadioShowRequest;
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_radio::provider::async_trait;
use reelix_radio::{
    ProviderError, ProviderRegistry, RadioDeps, RadioProvider, RadioService, ShowData, ShowFetch, StationData,
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fake;

#[async_trait]
impl RadioProvider for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    async fn preview(&self, _link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(None)
    }
    async fn show(&self, _id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        Ok(ShowData {
            external_id: "p001".into(),
            title: "The Archers".into(),
            station: Some(StationData {
                external_id: "radio4".into(),
                name: "Radio 4".into(),
                logo_url: None,
            }),
            complete: true,
            ..ShowData::default()
        })
    }
}

struct TestApp {
    router: Router,
    state: AppState,
    radio: Arc<RadioService>,
    dir: tempfile::TempDir,
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
        providers: ProviderRegistry::default().with(Arc::new(Fake)),
        library_sync: None,
    }));
    radio.set_data_dir(dir.path().to_path_buf());
    radio.set_secrets(SecretBox::from_key([5u8; 32]));
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(radio.clone());
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        radio,
        dir,
    }
}

async fn get(
    app: &TestApp,
    uri: &str,
    cookie: &str,
    if_none_match: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut b = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::COOKIE, cookie);
    if let Some(t) = if_none_match {
        b = b.header(header::IF_NONE_MATCH, t);
    }
    let res = app
        .router
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

async fn put_json(app: &TestApp, uri: &str, cookie: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header("x-reelix-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn cookie_of(res: &axum::response::Response) -> String {
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
    cookie_of(&res)
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
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": name, "password": "user-password"}).to_string(),
        ))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    cookie_of(&res)
}

const LOGO: &str = r#"<svg xmlns="http://www.w3.org/2000/svg"><circle r="4"/></svg>"#;

#[tokio::test]
async fn artwork_and_logos_are_served_with_etags() {
    let app = app().await;
    let admin = admin(&app).await;
    let show = app
        .radio
        .add_show(AddRadioShowRequest {
            provider: Some("fake".into()),
            external_id: Some("p001".into()),
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    let (id, station) = (show.id, show.station_id.unwrap());
    let art = format!("/api/v1/radio/shows/{id}/artwork");
    let logo = format!("/api/v1/radio/stations/{station}/logo");

    // Nothing downloaded yet; unknown ids.
    assert_eq!(get(&app, &art, &admin, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, &logo, &admin, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&app, "/api/v1/radio/shows/999/artwork", &admin, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&app, "/api/v1/radio/stations/999/logo", &admin, None).await.0,
        StatusCode::NOT_FOUND
    );

    // As a refresh would leave them.
    let root = app.dir.path().join("radio");
    std::fs::create_dir_all(root.join("artwork")).unwrap();
    std::fs::create_dir_all(root.join("logos")).unwrap();
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
    jpeg.resize(600, 1);
    std::fs::write(root.join(format!("artwork/{id}.jpg")), &jpeg).unwrap();
    std::fs::write(root.join(format!("logos/{station}.svg")), LOGO).unwrap();
    app.radio
        .set_show_artwork_path(id, Some(&format!("radio/artwork/{id}.jpg")))
        .await
        .unwrap();
    app.radio
        .set_station_logo_path(station, Some(&format!("radio/logos/{station}.svg")))
        .await
        .unwrap();

    let (s, h, body) = get(&app, &art, &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, jpeg);
    assert_eq!(h[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(
        h[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .starts_with("private, max-age=")
    );
    assert!(h.get(header::CONTENT_SECURITY_POLICY).is_none());
    let etag = h[header::ETAG].to_str().unwrap().to_owned();
    assert!(etag.starts_with('"'));

    let (s, h, body) = get(&app, &art, &admin, Some(&etag)).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    assert_eq!(h[header::ETAG], etag.as_str());
    let (s, _, _) = get(&app, &art, &admin, Some(&format!("\"other\", W/{etag}"))).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED, "lists and weak tags match");
    let (s, _, _) = get(&app, &art, &admin, Some("\"stale\"")).await;
    assert_eq!(s, StatusCode::OK);

    let (s, h, body) = get(&app, &logo, &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, LOGO.as_bytes());
    assert_eq!(h[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(
        h[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; style-src 'unsafe-inline'; sandbox"
    );
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    // A changed file gets a new ETag.
    std::fs::write(root.join(format!("artwork/{id}.jpg")), [&jpeg[..], b"x"].concat()).unwrap();
    let (s, h, _) = get(&app, &art, &admin, Some(&etag)).await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(h[header::ETAG], etag.as_str());

    // Same permission as the other radio reads.
    let listener = user_with(&app, "listener", Permissions::ROLE_USER).await;
    assert_eq!(get(&app, &art, &listener, None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, &logo, &listener, None).await.0, StatusCode::FORBIDDEN);
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    assert_eq!(get(&app, &art, &manager, None).await.0, StatusCode::OK);
    assert_eq!(
        get(&app, &art, "nobody=1", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn podcastindex_secret_is_masked() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, saved) = put_json(
        &app,
        "/api/v1/radio/settings",
        &admin,
        json!({"podcastindex_api_key": "KEY", "podcastindex_api_secret": "SECRET"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["podcastindex_api_secret"], "********");
    let (s, _, body) = get(&app, "/api/v1/radio/settings", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["podcastindex_api_key"], "KEY");
    assert_eq!(v["podcastindex_api_secret"], "********");
    assert!(!String::from_utf8_lossy(&body).contains("SECRET"));
    assert_eq!(
        app.radio
            .settings()
            .await
            .unwrap()
            .podcastindex_api_secret
            .as_deref(),
        Some("SECRET")
    );
}
