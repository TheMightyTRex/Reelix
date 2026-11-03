//! API flows for requests, discovery, issues, users, external login and the M6 settings.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, MediaKind, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_requests::fakes::{
    self, FakeDirectory, FakeLibrary, FakeMediaServer, FakeMetadata, FakeNotifications,
};
use reelix_requests::{MediaServerKind, RequestDeps, RequestService};
use serde_json::{Value, json};
use tower::ServiceExt;

const PW: &str = "correct-horse-battery";

struct TestApp {
    router: Router,
    lib: FakeLibrary,
}

async fn app_with(dir: FakeDirectory) -> TestApp {
    let dir_tmp = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let system = JobSystem::builder(db.clone(), events.clone()).build();
    let secrets = SecretBox::from_key([7u8; 32]);
    let lib = FakeLibrary::default();
    let svc = RequestService::new(RequestDeps {
        db: db.clone(),
        events: events.clone(),
        secrets: secrets.clone(),
        library: Arc::new(lib.clone()),
        media_servers: Arc::new(dir),
        metadata: Arc::new(
            FakeMetadata::default()
                .with_movie(fakes::movie(603, "The Matrix", 1999))
                .with_series(fakes::series(1399, "Game of Thrones", 2))
                .with_hits(vec![fakes::hit(MediaKind::Movie, 603, "The Matrix")]),
        ),
        notifications: Arc::new(FakeNotifications::default()),
        http: None,
    });
    let hash = reelix_api::auth::hash_password(PW).unwrap();
    for (name, p) in [("admin", Permissions::ADMIN), ("alice", Permissions::ROLE_USER)] {
        db.users()
            .create(NewUser {
                username: name.into(),
                email: None,
                display_name: None,
                password_hash: Some(hash.clone()),
                auth_source: AuthSource::Local,
                permissions: p,
            })
            .await
            .unwrap();
    }
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: dir_tmp.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    std::mem::forget(dir_tmp);
    let state = AppState::new(db, events, system.queue(), secrets, config).with_extension(Arc::new(svc));
    TestApp {
        router: reelix_api::router(state),
        lib,
    }
}

async fn app() -> TestApp {
    app_with(FakeDirectory::new("http://127.0.0.1:9")).await
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

async fn call(app: &TestApp, m: Method, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Resp {
    let mut b = Request::builder()
        .method(m)
        .uri(uri)
        .header("x-reelix-request", "1")
        .header(header::HOST, "reelix.local");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Resp {
        status,
        headers,
        body,
    }
}

fn cookie_of(r: &Resp) -> String {
    r.headers
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn login(app: &TestApp, user: &str) -> String {
    let r = call(
        app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"username": user, "password": PW})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    cookie_of(&r)
}

#[tokio::test]
async fn request_routes() {
    let app = app().await;
    let admin = login(&app, "admin").await;
    let alice = login(&app, "alice").await;

    let r = call(&app, Method::GET, "/api/v1/requests", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests",
        Some(&alice),
        Some(json!({"kind": "movie", "tmdb_id": 603})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["status"], "pending");
    let id = r.body["id"].as_i64().unwrap();
    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests",
        Some(&alice),
        Some(json!({"kind": "movie", "tmdb_id": 603})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/requests/{id}/approve"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/requests?status=pending",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body["total"], 1);
    assert_eq!(r.body["items"][0]["requested_by"]["username"], "alice");
    let r = call(
        &app,
        Method::GET,
        "/api/v1/requests?status=nope",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/requests/{id}/approve"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "approved");
    assert_eq!(app.lib.state().added_movies.len(), 1);

    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests",
        Some(&alice),
        Some(json!({"kind": "series", "tmdb_id": 1399, "seasons": [2]})),
    )
    .await;
    let tv = r.body["id"].as_i64().unwrap();
    // Decline without a body, then with a reason.
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/requests/{tv}/decline"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!(r.body["decline_reason"].is_null());
    let r = call(&app, Method::GET, "/api/v1/requests/count", Some(&alice), None).await;
    assert_eq!(r.body["total"], 2);
    assert_eq!(r.body["declined"], 1);
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/requests/{tv}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/requests/{tv}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let me = call(&app, Method::GET, "/api/v1/auth/me", Some(&alice), None).await;
    let uid = me.body["id"].as_i64().unwrap();
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/users/{uid}/quota"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["movie"]["used"], 1);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/requests/options?kind=movie",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/requests/options?kind=series",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    // Settings: admin only, validated.
    let r = call(&app, Method::GET, "/api/v1/settings/requests", Some(&alice), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let mut s = call(&app, Method::GET, "/api/v1/settings/requests", Some(&admin), None)
        .await
        .body;
    s["series_type"] = json!("weird");
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/settings/requests",
        Some(&admin),
        Some(s.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    s["series_type"] = json!("anime");
    s["movie_quota_limit"] = json!(5);
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/settings/requests",
        Some(&admin),
        Some(s),
    )
    .await;
    assert_eq!(r.body["movie_quota_limit"], 5);

    // Profile notification prefs.
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/profile/notifications",
        Some(&alice),
        Some(json!({"events": ["request_available"]})),
    )
    .await;
    assert_eq!(r.body["events"], json!(["request_available"]));
    let r = call(
        &app,
        Method::GET,
        "/api/v1/profile/notifications",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.body["events"], json!(["request_available"]));
}

#[tokio::test]
async fn discover_and_media_routes() {
    let app = app().await;
    let alice = login(&app, "alice").await;
    for uri in [
        "/api/v1/discover/trending",
        "/api/v1/discover/popular-series?page=2",
        "/api/v1/discover/genre/movie/28",
        "/api/v1/discover/network/213",
        "/api/v1/media/movie/603/recommendations",
        "/api/v1/media/tv/1399/similar",
    ] {
        let r = call(&app, Method::GET, uri, Some(&alice), None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {:?}", r.body);
        assert_eq!(r.body["results"][0]["tmdb_id"], 603, "{uri}");
    }
    let r = call(
        &app,
        Method::GET,
        "/api/v1/discover/unknown-feed",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/discover/genres/movie",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.body[0]["name"], "Action");
    let r = call(&app, Method::GET, "/api/v1/search?q=matrix", Some(&alice), None).await;
    assert_eq!(r.body["results"][0]["availability"], "unknown");
    let r = call(&app, Method::GET, "/api/v1/media/movie/603", Some(&alice), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["title"], "The Matrix");
    assert_eq!(r.body["request_ability"]["can_request"], true);
    let r = call(&app, Method::GET, "/api/v1/media/tv/1399", Some(&alice), None).await;
    assert_eq!(r.body["seasons"].as_array().unwrap().len(), 2);
    let r = call(&app, Method::GET, "/api/v1/media/movie/1", Some(&alice), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn issue_routes() {
    let app = app().await;
    let admin = login(&app, "admin").await;
    let alice = login(&app, "alice").await;
    let r = call(
        &app,
        Method::POST,
        "/api/v1/issues",
        Some(&alice),
        Some(json!({"kind": "movie", "tmdb_id": 603, "issue_type": "subtitles", "message": "missing subs"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let id = r.body["id"].as_i64().unwrap();
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/issues/{id}/comments"),
        Some(&admin),
        Some(json!({"message": "on it"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["comments"][0]["message"], "on it");
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/issues/{id}/resolve"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body["status"], "resolved");
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/issues/{id}/reopen"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.body["status"], "open");
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/issues/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.body["comments"].as_array().unwrap().len(), 1);
    let r = call(&app, Method::GET, "/api/v1/issues/count", Some(&admin), None).await;
    assert_eq!(r.body["open"], 1);
    let r = call(
        &app,
        Method::PUT,
        &format!("/api/v1/issues/{id}"),
        Some(&alice),
        Some(json!({"issue_type": "video"})),
    )
    .await;
    assert_eq!(r.body["issue_type"], "video");
    let r = call(
        &app,
        Method::GET,
        "/api/v1/issues?status=open",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body["total"], 1);
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/issues/{id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn user_routes() {
    let app = app().await;
    let admin = login(&app, "admin").await;
    let alice = login(&app, "alice").await;
    assert_eq!(
        call(&app, Method::GET, "/api/v1/users", Some(&alice), None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let r = call(&app, Method::GET, "/api/v1/users/roles", Some(&alice), None).await;
    assert_eq!(r.body["roles"].as_array().unwrap().len(), 4);
    let r = call(
        &app,
        Method::POST,
        "/api/v1/users",
        Some(&admin),
        Some(json!({"username": "bob", "password": "short"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &app,
        Method::POST,
        "/api/v1/users",
        Some(&admin),
        Some(json!({"username": "bob", "password": PW, "permissions": 96})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let bob_id = r.body["id"].as_i64().unwrap();
    assert_eq!(r.body["has_password"], true);
    let bob = login(&app, "bob").await;

    // Password reset by admin logs bob out.
    let mut u = call(
        &app,
        Method::GET,
        &format!("/api/v1/users/{bob_id}"),
        Some(&admin),
        None,
    )
    .await
    .body;
    u["password"] = json!("another-long-password");
    u["display_name"] = json!("Bobby");
    let r = call(
        &app,
        Method::PUT,
        &format!("/api/v1/users/{bob_id}"),
        Some(&admin),
        Some(u),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["display_name"], "Bobby");
    assert_eq!(
        call(&app, Method::GET, "/api/v1/auth/me", Some(&bob), None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let r = call(&app, Method::GET, "/api/v1/users", Some(&admin), None).await;
    assert_eq!(r.body.as_array().unwrap().len(), 3);
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/users/{bob_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/users/import/servers",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body, json!([]));
    let r = call(&app, Method::GET, "/api/v1/users/import/9", Some(&admin), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn external_login_routes() {
    let dir = FakeDirectory::new("http://127.0.0.1:9").with(
        3,
        "Emby",
        FakeMediaServer {
            kind: MediaServerKind::Emby,
            users: vec![(FakeMediaServer::user("e1", "erin"), Some("emby-pw".into()))],
        },
    );
    let app = app_with(dir).await;
    let r = call(&app, Method::GET, "/api/v1/auth/methods", None, None).await;
    assert_eq!(
        r.body,
        json!({"local": true, "jellyfin": false, "emby": false, "plex": false, "oidc": false, "oidc_label": null})
    );
    let login_body = json!({"username": "erin", "password": "emby-pw"});
    let r = call(
        &app,
        Method::POST,
        "/api/v1/auth/emby",
        None,
        Some(login_body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let admin = login(&app, "admin").await;
    let alice = login(&app, "alice").await;
    assert_eq!(
        call(&app, Method::GET, "/api/v1/settings/security", Some(&alice), None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let mut s = call(&app, Method::GET, "/api/v1/settings/security", Some(&admin), None)
        .await
        .body;
    s["emby_login"] = json!(true);
    s["oidc"]["enabled"] = json!(true);
    s["oidc"]["client_id"] = json!("reelix");
    s["oidc"]["client_secret"] = json!("shh");
    s["oidc"]["issuer_url"] = json!("not a url");
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/settings/security",
        Some(&admin),
        Some(s.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let idp = wiremock::MockServer::start().await;
    s["oidc"]["issuer_url"] = json!(idp.uri());
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/settings/security",
        Some(&admin),
        Some(s),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["oidc"]["client_secret"], "********");

    let r = call(
        &app,
        Method::POST,
        "/api/v1/auth/emby",
        None,
        Some(json!({"username": "erin", "password": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&app, Method::POST, "/api/v1/auth/emby", None, Some(login_body)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["auth_source"], "emby");
    let erin = cookie_of(&r);
    let me = call(&app, Method::GET, "/api/v1/auth/me", Some(&erin), None).await;
    assert_eq!(me.body["username"], "erin");

    // OIDC: login redirects to the provider's authorize endpoint with our derived callback.
    let base = idp.uri();
    wiremock::Mock::given(wiremock::matchers::path("/.well-known/openid-configuration"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base, "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"), "jwks_uri": format!("{base}/jwks"),
            "response_types_supported": ["code"], "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        })))
        .mount(&idp)
        .await;
    wiremock::Mock::given(wiremock::matchers::path("/jwks"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"keys": []})))
        .mount(&idp)
        .await;
    let r = call(&app, Method::GET, "/api/v1/auth/methods", None, None).await;
    assert_eq!(r.body["oidc"], true);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/auth/oidc/login?redirect=/discover",
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FOUND);
    let loc = r.headers[header::LOCATION].to_str().unwrap();
    assert!(loc.starts_with(&format!("{base}/authorize?")), "{loc}");
    assert!(
        loc.contains("redirect_uri=http%3A%2F%2Freelix.local%2Fapi%2Fv1%2Fauth%2Foidc%2Fcallback"),
        "{loc}"
    );
    let r = call(
        &app,
        Method::GET,
        "/api/v1/auth/oidc/callback?code=x&state=bogus",
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert!(
        r.headers[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/login?error=")
    );
    assert!(r.headers.get(header::SET_COOKIE).is_none());
    // Provider errors are only echoed for a sign-in this browser started.
    let r = call(
        &app,
        Method::GET,
        "/api/v1/auth/oidc/callback?error=access_denied&error_description=Call+0800-SCAM",
        None,
        None,
    )
    .await;
    assert_eq!(
        r.headers[header::LOCATION],
        "/login?error=sign-in%20failed%3B%20try%20again"
    );
    let started = call(&app, Method::GET, "/api/v1/auth/oidc/login", None, None).await;
    let st = url::Url::parse(started.headers[header::LOCATION].to_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/auth/oidc/callback?error=access_denied&state={st}"),
        Some(&cookie_of(&started)),
        None,
    )
    .await;
    assert_eq!(r.headers[header::LOCATION], "/login?error=access_denied");

    // Plex PIN: disabled.
    let r = call(&app, Method::POST, "/api/v1/auth/plex/pin", None, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

/// `call` from a given TCP peer.
async fn call_from(app: &TestApp, peer: &str, m: Method, uri: &str, body: Value) -> Resp {
    let req = Request::builder()
        .method(m)
        .uri(uri)
        .header("x-reelix-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(axum::extract::ConnectInfo(
            peer.parse::<std::net::SocketAddr>().unwrap(),
        ))
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Resp {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

#[tokio::test]
async fn disabled_local_login_is_enforced_with_local_admin_break_glass() {
    let app = app().await;
    let admin = login(&app, "admin").await;
    let mut s = call(&app, Method::GET, "/api/v1/settings/security", Some(&admin), None)
        .await
        .body;
    s["local_login"] = json!(false);
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/settings/security",
        Some(&admin),
        Some(s),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);

    for user in ["admin", "alice"] {
        let r = call(
            &app,
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({"username": user, "password": PW})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{user}");
    }
    let body = |user: &str| json!({"username": user, "password": PW});
    let r = call_from(
        &app,
        "127.0.0.1:40000",
        Method::POST,
        "/api/v1/auth/login",
        body("alice"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call_from(
        &app,
        "127.0.0.1:40000",
        Method::POST,
        "/api/v1/auth/login",
        body("admin"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = call_from(
        &app,
        "192.0.2.1:40000",
        Method::POST,
        "/api/v1/auth/login",
        body("admin"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn password_changes_need_the_current_password_and_end_other_sessions() {
    let app = app().await;
    let admin = login(&app, "admin").await;
    let alice = login(&app, "alice").await;
    let alice_other = login(&app, "alice").await;
    let key = |cookie: &str| {
        let cookie = cookie.to_owned();
        let app = &app;
        async move {
            call(
                app,
                Method::POST,
                "/api/v1/auth/apikeys",
                Some(&cookie),
                Some(json!({"name": "k"})),
            )
            .await
            .body["key"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let alice_key = key(&alice).await;
    let me = |cred: &str| {
        let uri = if cred.starts_with("rlx_") {
            format!("/api/v1/auth/me?apikey={cred}")
        } else {
            "/api/v1/auth/me".to_owned()
        };
        let cookie = (!cred.starts_with("rlx_")).then(|| cred.to_owned());
        let app = &app;
        async move { call(app, Method::GET, &uri, cookie.as_deref(), None).await.status }
    };
    let alice_me = call(&app, Method::GET, "/api/v1/auth/me", Some(&alice), None)
        .await
        .body;
    let alice_id = alice_me["id"].as_i64().unwrap();
    let mut u = call(
        &app,
        Method::GET,
        &format!("/api/v1/users/{alice_id}"),
        Some(&alice),
        None,
    )
    .await
    .body;
    u["password"] = json!("brand-new-password");

    // Missing or wrong current password.
    for current in [None, Some("not-my-password")] {
        let mut body = u.clone();
        if let Some(c) = current {
            body["current_password"] = json!(c);
        }
        let r = call(
            &app,
            Method::PUT,
            &format!("/api/v1/users/{alice_id}"),
            Some(&alice),
            Some(body),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    }
    u["current_password"] = json!(PW);
    let r = call(
        &app,
        Method::PUT,
        &format!("/api/v1/users/{alice_id}"),
        Some(&alice),
        Some(u),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    // This session and the API key survive; the other session is gone.
    assert_eq!(me(&alice).await, StatusCode::OK);
    assert_eq!(me(&alice_key).await, StatusCode::OK);
    assert_eq!(me(&alice_other).await, StatusCode::UNAUTHORIZED);

    // An admin reset needs no current password and revokes sessions and API keys.
    let mut u = call(
        &app,
        Method::GET,
        &format!("/api/v1/users/{alice_id}"),
        Some(&admin),
        None,
    )
    .await
    .body;
    u["password"] = json!("reset-by-admin-pw");
    let r = call(
        &app,
        Method::PUT,
        &format!("/api/v1/users/{alice_id}"),
        Some(&admin),
        Some(u),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(me(&alice).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&alice_key).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&admin).await, StatusCode::OK);
}

#[test]
fn openapi_has_m6_tags() {
    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    for p in [
        "/api/v1/requests",
        "/api/v1/discover/{feed}",
        "/api/v1/discover/content/{kind}/{row}",
        "/api/v1/users",
        "/api/v1/auth/oidc/login",
    ] {
        assert!(doc["paths"].get(p).is_some(), "{p}");
    }
}

#[tokio::test]
async fn book_and_album_requests_and_discovery() {
    let app = app().await;
    app.lib
        .state()
        .book_hits
        .push(fakes::book("OL893415W", "Dune", "Frank Herbert"));
    app.lib.state().album_hits.push(fakes::album(
        "b1392450-e666-3926-a536-22c65f834433",
        "OK Computer",
        "Radiohead",
    ));
    let admin = login(&app, "admin").await;
    let alice = login(&app, "alice").await;

    // ROLE_USER has no REQUEST_BOOK.
    let body = json!({"kind": "book", "foreign_id": "OL893415W", "formats": ["audiobook"]});
    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests/media",
        Some(&alice),
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Search shows availability; the detail view lists requestable formats.
    let r = call(
        &app,
        Method::GET,
        "/api/v1/search/books?q=dune",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["results"][0]["foreign_id"], "OL893415W");
    assert_eq!(r.body["results"][0]["availability"], "unknown");
    let r = call(
        &app,
        Method::GET,
        "/api/v1/media/book/OL893415W",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["title"], "Dune");
    assert_eq!(
        r.body["request_ability"]["formats"],
        json!(["ebook", "audiobook"])
    );
    let r = call(&app, Method::GET, "/api/v1/media/book/OL404W", Some(&admin), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Admin requests are auto-approved and added to the library.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests/media",
        Some(&admin),
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["status"], "approved");
    assert_eq!(r.body["formats"], json!(["audiobook"]));
    assert_eq!(r.body["subtitle"], "Frank Herbert");
    assert!(r.body["book_id"].is_i64());
    assert_eq!(app.lib.state().added_books.len(), 1);

    let album = json!({"kind": "music", "foreign_id": "b1392450-e666-3926-a536-22c65f834433"});
    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests/media",
        Some(&admin),
        Some(album.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let album_id = r.body["id"].as_i64().unwrap();
    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests/media",
        Some(&admin),
        Some(album),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    let r = call(
        &app,
        Method::GET,
        "/api/v1/requests/media?kind=music",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body["total"], 1);
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/requests/media/{album_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body["kind"], "music");
    // Movie/TV endpoints don't see book/album requests.
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/requests/{album_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&app, Method::GET, "/api/v1/requests", Some(&admin), None).await;
    assert_eq!(r.body["total"], 0);
    let r = call(&app, Method::GET, "/api/v1/requests/count", Some(&admin), None).await;
    assert_eq!(r.body["total"], 2);
    assert_eq!(r.body["book"], 1);
    assert_eq!(r.body["music"], 1);

    let r = call(&app, Method::GET, "/api/v1/search/music?q=ok", Some(&alice), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["albums"][0]["request"]["status"], Value::Null);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/requests/media/options?kind=audiobook",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["profiles"][0]["id"], 100);

    let me = call(&app, Method::GET, "/api/v1/auth/me", Some(&admin), None).await;
    let uid = me.body["id"].as_i64().unwrap();
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/users/{uid}/quota"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.body["book"]["used"], 1);
    assert_eq!(r.body["music"]["used"], 1);
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/requests/media/{album_id}/decline"),
        Some(&admin),
        None,
    )
    .await;
    // Approved requests cannot be declined.
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/requests/media/{album_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn movie_request_json_shape_is_unchanged() {
    let app = app().await;
    let admin = login(&app, "admin").await;
    let r = call(
        &app,
        Method::POST,
        "/api/v1/requests",
        Some(&admin),
        Some(json!({"kind": "movie", "tmdb_id": 603})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let mut keys: Vec<&str> = r.body.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "approved_at",
            "available_at",
            "backdrop",
            "created_at",
            "decline_reason",
            "failure_reason",
            "id",
            "imdb_id",
            "is_4k",
            "kind",
            "modified_by",
            "movie_id",
            "poster",
            "profile_id",
            "requested_by",
            "root_folder_id",
            "seasons",
            "series_id",
            "status",
            "tags",
            "title",
            "tmdb_id",
            "tvdb_id",
            "updated_at",
            "year",
        ]
    );
    let r = call(&app, Method::GET, "/api/v1/requests/count", Some(&admin), None).await;
    assert!(r.body.get("book").is_none() && r.body.get("music").is_none());
}

#[tokio::test]
async fn book_and_music_discover_rows() {
    let app = app().await;
    app.lib.state().content_rows.insert(
        (
            reelix_api_types::requests::ContentRequestKind::Book,
            "subject-fantasy".into(),
        ),
        vec![fakes::book("OL27479W", "The Two Towers", "J.R.R. Tolkien")],
    );
    app.lib.state().content_rows.insert(
        (
            reelix_api_types::requests::ContentRequestKind::Music,
            "new-releases".into(),
        ),
        vec![fakes::album("rg-1", "Perfecth", "Queens of the Stone Age")],
    );
    let alice = login(&app, "alice").await;

    // Not taken by the `/discover/{feed}` catch-all.
    let r = call(
        &app,
        Method::GET,
        "/api/v1/discover/content/book/subject-fantasy",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["results"][0]["foreign_id"], "OL27479W");
    assert_eq!(r.body["results"][0]["subtitle"], "J.R.R. Tolkien");
    assert_eq!(r.body["results"][0]["kind"], "book");

    let r = call(
        &app,
        Method::GET,
        "/api/v1/discover/content/music/new-releases",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["results"][0]["title"], "Perfecth");

    for uri in [
        "/api/v1/discover/content/movies/trending",
        "/api/v1/discover/content/book/nope",
    ] {
        let r = call(&app, Method::GET, uri, Some(&alice), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let r = call(
        &app,
        Method::GET,
        "/api/v1/discover/content/book/trending",
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}
