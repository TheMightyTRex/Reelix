//! API flows for quality profiles, definitions, custom formats, delay/release profiles and `/parse`.

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
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config);
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
async fn quality_profile_crud_and_permissions() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let plain = user_with(&app, "plain", Permissions::ROLE_USER).await;

    let (s, list) = call(&app, Method::GET, "/api/v1/qualityprofiles", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 6);
    let (s, _) = call(&app, Method::GET, "/api/v1/qualityprofiles", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let mut p = list[3].clone();
    p["name"] = json!("My 1080p");
    p["min_format_score"] = json!(5);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/qualityprofiles",
        &manager,
        Some(p.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "writes are admin-only");
    let (s, created) = call(
        &app,
        Method::POST,
        "/api/v1/qualityprofiles",
        &admin,
        Some(p.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["min_format_score"], 5);

    let (s, body) = call(
        &app,
        Method::POST,
        "/api/v1/qualityprofiles",
        &admin,
        Some(p.clone()),
    )
    .await;
    assert_eq!(
        (s, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );

    let mut bad = p.clone();
    bad["name"] = json!("Bad");
    bad["cutoff"] = json!(424242);
    let (s, body) = call(&app, Method::POST, "/api/v1/qualityprofiles", &admin, Some(bad)).await;
    assert_eq!(
        (s, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("validation"))
    );
    let mut bad = p.clone();
    bad["name"] = json!("Bad CF");
    bad["format_items"] = json!([{"format_id": 999, "score": 10}]);
    let (s, _) = call(&app, Method::POST, "/api/v1/qualityprofiles", &admin, Some(bad)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    p["upgrade_allowed"] = json!(false);
    let uri = format!("/api/v1/qualityprofiles/{id}");
    let (s, updated) = call(&app, Method::PUT, &uri, &admin, Some(p.clone())).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(updated["upgrade_allowed"], false);
    let (s, got) = call(&app, Method::GET, &uri, &manager, None).await;
    assert_eq!((s, got["name"].as_str()), (StatusCode::OK, Some("My 1080p")));
    let (s, _) = call(&app, Method::DELETE, &uri, &admin, None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(&app, Method::GET, &uri, &admin, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn quality_definitions_bulk_update() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, mut defs) = call(&app, Method::GET, "/api/v1/qualitydefinitions", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    let row = defs
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["quality"] == "Bluray-1080p")
        .unwrap();
    row["max_size"] = json!(321.5);
    let (s, updated) = call(
        &app,
        Method::PUT,
        "/api/v1/qualitydefinitions",
        &admin,
        Some(defs.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let row = updated
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["quality"] == "Bluray-1080p")
        .unwrap();
    assert_eq!(row["max_size"], 321.5);
    defs[0]["min_size"] = json!(-5);
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/qualitydefinitions",
        &admin,
        Some(defs),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn quality_families() {
    let app = app().await;
    let admin = admin(&app).await;
    let get = |uri: &'static str| call(&app, Method::GET, uri, &admin, None);

    // Video stays the default and keeps its JSON shape.
    let (_, video) = get("/api/v1/qualityprofiles").await;
    assert_eq!(get("/api/v1/qualityprofiles?kind=video").await.1, video);
    assert!(video[0].get("family").is_none());
    let (s, _) = get("/api/v1/qualityprofiles?kind=comics").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Defaults per family.
    let (s, ebook) = get("/api/v1/qualityprofiles?kind=ebook").await;
    assert_eq!(s, StatusCode::OK);
    let names: Vec<&str> = ebook
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Ebook: Any", "Ebook: EPUB"]);
    assert_eq!(ebook[0]["family"], "ebook");
    let (_, music) = get("/api/v1/qualityprofiles?kind=music").await;
    assert_eq!(music.as_array().unwrap().len(), 4);
    let (_, audio) = get("/api/v1/qualityprofiles?kind=audiobook").await;
    assert_eq!(audio.as_array().unwrap().len(), 2);

    // Create with ?kind=: the body may omit family.
    let mut p = ebook[1].clone();
    p.as_object_mut().unwrap().remove("family");
    p["name"] = json!("Kindle");
    let (s, created) = call(
        &app,
        Method::POST,
        "/api/v1/qualityprofiles?kind=ebook",
        &admin,
        Some(p.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert_eq!(created["family"], "ebook");
    // Without kind/family it would be a video profile holding ebook qualities.
    p["name"] = json!("Kindle 2");
    let (s, body) = call(
        &app,
        Method::POST,
        "/api/v1/qualityprofiles",
        &admin,
        Some(p.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    // Mixing families is refused.
    let mut mixed = created.clone();
    mixed["items"]
        .as_array_mut()
        .unwrap()
        .push(json!({"quality": "FLAC", "allowed": true}));
    let uri = format!("/api/v1/qualityprofiles/{}", created["id"]);
    let (s, _) = call(&app, Method::PUT, &uri, &admin, Some(mixed)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Update keeps the stored family even when the body omits it.
    let mut edit = created.clone();
    edit.as_object_mut().unwrap().remove("family");
    edit["upgrade_allowed"] = json!(false);
    let (s, updated) = call(&app, Method::PUT, &uri, &admin, Some(edit)).await;
    assert_eq!((s, &updated["family"]), (StatusCode::OK, &json!("ebook")));
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/qualityprofiles/99999",
        &admin,
        Some(updated),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Definitions per family.
    let (s, mut defs) = get("/api/v1/qualitydefinitions?kind=music").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(defs[0]["quality"], "Unknown Music");
    let flac = defs
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["quality"] == "FLAC")
        .unwrap();
    flac["max_size"] = json!(20.0);
    let (s, updated) = call(
        &app,
        Method::PUT,
        "/api/v1/qualitydefinitions?kind=music",
        &admin,
        Some(defs.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{updated}");
    assert_eq!(updated.as_array().unwrap().len(), defs.as_array().unwrap().len());
    assert!(
        updated
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["quality"] == "FLAC" && d["max_size"] == 20.0)
    );
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/qualitydefinitions?kind=ebook",
        &admin,
        Some(defs),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, video_defs) = get("/api/v1/qualitydefinitions").await;
    assert_eq!(video_defs[0]["quality"], "Unknown");

    // The release tester parses with the profile's family.
    let id = music
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Music: Lossless")
        .unwrap()["id"]
        .clone();
    let (s, res) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({"title": "Artist - Album (2020) [FLAC]", "profile_id": id})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{res}");
    assert_eq!(res["quality"], "FLAC");
    assert_eq!(res["decision"]["accepted"], true, "{res}");
}

const TRASH: &str = r#"[
  { "trash_id": "dv-boost", "trash_scores": { "default": 1500 }, "name": "DV",
    "includeCustomFormatWhenRenaming": false,
    "specifications": [
      { "name": "DV", "implementation": "ReleaseTitleSpecification", "negate": false, "required": true,
        "fields": { "value": "\\b(dv|dovi|dolby[ .]?vision)\\b" } } ] },
  { "name": "x265 (HD)", "includeCustomFormatWhenRenaming": false,
    "specifications": [
      { "name": "x265", "implementation": "ReleaseTitleSpecification", "negate": false, "required": true,
        "fields": [ { "name": "value", "value": "[xh][ .]?265|\\bHEVC\\b" } ] },
      { "name": "Not 2160p", "implementation": "ResolutionSpecification", "negate": true, "required": true,
        "fields": { "value": 2160 } } ] }
]"#;

#[tokio::test]
async fn custom_formats_import_export_and_parse() {
    let app = app().await;
    let admin = admin(&app).await;
    let body: Value = serde_json::from_str(TRASH).unwrap();
    let (s, imported) = call(
        &app,
        Method::POST,
        "/api/v1/customformats/import",
        &admin,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{imported}");
    assert_eq!(imported.as_array().unwrap().len(), 2);
    assert_eq!(imported[0]["default_score"], 1500);
    let dv_id = imported[0]["format"]["id"].as_i64().unwrap();
    let x265_id = imported[1]["format"]["id"].as_i64().unwrap();
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/customformats/import",
        &admin,
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/customformats/import",
        &admin,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, exported) = call(
        &app,
        Method::GET,
        &format!("/api/v1/customformats/{dv_id}/export"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(exported["trash_id"], "dv-boost");
    assert_eq!(
        exported["specifications"][0]["implementation"],
        "ReleaseTitleSpecification"
    );

    // Manual create with an invalid regex is rejected.
    let bad =
        json!({"name": "Bad", "specifications": [{"name": "b", "type": "release_title", "value": "("}]});
    let (s, body) = call(&app, Method::POST, "/api/v1/customformats", &admin, Some(bad)).await;
    assert_eq!(
        (s, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("validation"))
    );
    let ok = json!({"name": "Remux", "specifications": [{"name": "r", "type": "quality_modifier", "value": "remux"}]});
    let (s, remux) = call(&app, Method::POST, "/api/v1/customformats", &admin, Some(ok)).await;
    assert_eq!(s, StatusCode::CREATED);

    // Score the formats in the HD-1080p profile and test a release against it.
    let (_, profiles) = call(&app, Method::GET, "/api/v1/qualityprofiles", &admin, None).await;
    let mut hd = profiles[3].clone();
    let hd_id = hd["id"].as_i64().unwrap();
    hd["format_items"] = json!([{"format_id": x265_id, "score": -10000}, {"format_id": dv_id, "score": 100}]);
    hd["min_format_score"] = json!(0);
    let (s, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/qualityprofiles/{hd_id}"),
        &admin,
        Some(hd),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let (s, r) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({
            "title": "Some.Movie.2021.1080p.BluRay.x265-GRP", "profile_id": hd_id
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["parsed"]["title"], "Some Movie");
    assert_eq!(r["quality"], "Bluray-1080p");
    assert_eq!(r["format_score"], -10000);
    assert_eq!(r["matched_formats"][0]["name"], "x265 (HD)");
    assert_eq!(r["decision"]["accepted"], false);
    assert_eq!(r["decision"]["rejections"][0]["spec"], "custom_format_score");

    let (_, r) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({
            "title": "Some.Movie.2021.1080p.BluRay.x264-GRP", "profile_id": hd_id
        })),
    )
    .await;
    assert_eq!(r["decision"]["accepted"], true);
    let (_, r) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({"title": "Show.S01E02.720p.HDTV.x264-LOL"})),
    )
    .await;
    assert_eq!(r["parsed"]["kind"]["kind"], "episode");
    assert!(r["decision"].is_null());
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({"title": "x", "profile_id": 999})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({"title": "  "})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Deleting a format removes its score from profiles.
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/customformats/{x265_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, hd) = call(
        &app,
        Method::GET,
        &format!("/api/v1/qualityprofiles/{hd_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(hd["format_items"].as_array().unwrap().len(), 1);
    let (_, all) = call(&app, Method::GET, "/api/v1/customformats", &admin, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert!(remux["id"].is_i64());
}

#[tokio::test]
async fn delay_and_release_profiles() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, delays) = call(&app, Method::GET, "/api/v1/delayprofiles", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    let default_id = delays[0]["id"].as_i64().unwrap();
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/delayprofiles/{default_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "default profile is protected");

    let new = json!({"enable_usenet": true, "enable_torrent": true, "preferred_protocol": "torrent",
                     "torrent_delay": 60, "tags": [1], "order": 1});
    let (s, created) = call(
        &app,
        Method::POST,
        "/api/v1/delayprofiles",
        &admin,
        Some(new.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();
    let mut changed = new.clone();
    changed["usenet_delay"] = json!(30);
    let (s, updated) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/delayprofiles/{id}"),
        &admin,
        Some(changed),
    )
    .await;
    assert_eq!((s, updated["usenet_delay"].as_i64()), (StatusCode::OK, Some(30)));
    let mut bad = new;
    bad["enable_usenet"] = json!(false);
    bad["enable_torrent"] = json!(false);
    let (s, _) = call(&app, Method::POST, "/api/v1/delayprofiles", &admin, Some(bad)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/delayprofiles/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let rp = json!({"name": "No CAM", "required": [], "ignored": ["/\\bCAM\\b/i", "HDTS"]});
    let (s, created) = call(&app, Method::POST, "/api/v1/releaseprofiles", &admin, Some(rp)).await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/releaseprofiles",
        &admin,
        Some(json!({"name": "x", "ignored": ["/(bad/"]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // The release profile now rejects CAM releases in /parse.
    let (_, profiles) = call(&app, Method::GET, "/api/v1/qualityprofiles", &admin, None).await;
    let any_id = profiles[0]["id"].as_i64().unwrap();
    let (_, r) = call(
        &app,
        Method::POST,
        "/api/v1/parse",
        &admin,
        Some(json!({
            "title": "Movie.2024.CAM.x264-GRP", "profile_id": any_id
        })),
    )
    .await;
    let specs: Vec<&str> = r["decision"]["rejections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["spec"].as_str().unwrap())
        .collect();
    assert!(
        specs.contains(&"release_profile") && specs.contains(&"quality"),
        "{specs:?}"
    );

    let (s, list) = call(&app, Method::GET, "/api/v1/releaseprofiles", &admin, None).await;
    assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 1));
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/releaseprofiles/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn openapi_lists_profile_routes() {
    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    for path in [
        "/api/v1/qualityprofiles",
        "/api/v1/qualityprofiles/{id}",
        "/api/v1/qualitydefinitions",
        "/api/v1/customformats/import",
        "/api/v1/customformats/{id}/export",
        "/api/v1/delayprofiles",
        "/api/v1/releaseprofiles/{id}",
        "/api/v1/parse",
    ] {
        assert!(doc["paths"].get(path).is_some(), "{path} missing from OpenAPI");
    }
}
