//! `POST /customitems`, `PUT /customitems/{kind}/{id}` and `GET /customitems/{kind}/{id}/image`
//! (RFX-90): films, series, authors, artists and radio shows with the owner's own details and
//! no outside match.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_books::testing::FakeBookMetadata;
use reelix_books::{BooksDeps, BooksService};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_music::provider::*;
use reelix_music::{MusicDeps, MusicService};
use reelix_radio::{NewEpisodeFile, ProviderRegistry, RadioDeps, RadioService};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The start of a PNG file: enough for Reelix to recognise it.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

fn png_data() -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(PNG)
    )
}

struct NoMusicBrainz;

#[async_trait]
impl MusicMetadataProvider for NoMusicBrainz {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn search_artists(&self, _q: &str) -> Result<Vec<ArtistSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn search_release_groups(
        &self,
        _q: &str,
        _artist: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn artist(&self, _mbid: &str) -> Result<ArtistMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn release_group(&self, _mbid: &str) -> Result<ReleaseGroupMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn releases(&self, _group: &str) -> Result<Vec<ReleaseMetadata>, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn release(&self, _mbid: &str) -> Result<ReleaseMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn cover_art(&self, _group: &str) -> Result<Option<CoverArt>, MetaError> {
        Ok(None)
    }
}

/// A minimal FLAC stream (STREAMINFO, 44.1 kHz stereo, `secs` long).
fn flac(path: &Path, secs: u64) {
    let mut b = b"fLaC".to_vec();
    b.push(0x00);
    b.extend_from_slice(&[0, 0, 34]);
    b.extend_from_slice(&4096u16.to_be_bytes());
    b.extend_from_slice(&4096u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    let v: u64 = (44_100u64 << 44) | (1u64 << 41) | (15u64 << 36) | (secs * 44_100);
    b.extend_from_slice(&v.to_be_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[0x81, 0, 0, 16]);
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[0xAB; 256]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b).unwrap();
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![0u8; 4096]).unwrap();
}

struct TestApp {
    router: Router,
    state: AppState,
    library: Arc<LibraryService>,
    books: Arc<BooksService>,
    music: Arc<MusicService>,
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
        data_dir: dir.path().join("data"),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    library.set_queue(system.queue());
    let books = Arc::new(BooksService::new(BooksDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(FakeBookMetadata::new()),
    }));
    books.set_queue(system.queue());
    let music = Arc::new(MusicService::new(MusicDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(NoMusicBrainz),
    }));
    music.set_queue(system.queue());
    let radio = Arc::new(RadioService::new(RadioDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default(),
        library_sync: None,
    }));
    radio.set_queue(system.queue());
    radio.set_data_dir(dir.path().join("data"));
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(library.clone())
        .with_extension(books.clone())
        .with_extension(music.clone())
        .with_extension(radio.clone());
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        library,
        books,
        music,
        radio,
        dir,
    }
}

/// Calls the API with a cookie (CSRF header included); returns status, headers and body.
async fn raw(
    app: &TestApp,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
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
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

async fn call(
    app: &TestApp,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, _, bytes) = raw(app, method, uri, cookie, body).await;
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (s, v)
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
    let sc = res.headers()[header::SET_COOKIE].to_str().unwrap();
    sc.split(';').next().unwrap().to_owned()
}

/// Makes `name` under the test folder and adds it as a root folder through `route`.
async fn root(app: &TestApp, cookie: &str, route: &str, name: &str, kind: &str) -> (i64, std::path::PathBuf) {
    let p = app.dir.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    let body = if route == "/api/v1/rootfolders" {
        json!({"path": p.to_str().unwrap(), "media_kind": kind})
    } else {
        json!({"path": p.to_str().unwrap(), "kind": kind})
    };
    let (s, r) = call(app, Method::POST, route, cookie, Some(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    (r["id"].as_i64().unwrap(), p)
}

#[tokio::test]
async fn listed_in_openapi_and_needs_manage_library() {
    let app = app().await;
    let admin = admin(&app).await;
    let (s, doc) = call(&app, Method::GET, "/api/v1/openapi.json", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(doc["paths"]["/api/v1/customitems"]["post"].is_object());
    assert!(doc["paths"]["/api/v1/customitems/{kind}/{id}"]["put"].is_object());
    assert!(doc["paths"]["/api/v1/customitems/{kind}/{id}/image"]["get"].is_object());
    assert!(doc["components"]["schemas"]["AddCustomItemRequest"].is_object());

    let plain = user_with(&app, "plain", Permissions::ROLE_USER).await;
    let body = json!({"kind": "movie", "root_folder_id": 1, "path": "/x", "title": "A"});
    let (s, _) = call(&app, Method::POST, "/api/v1/customitems", &plain, Some(body)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/customitems/movie/1/image",
        &plain,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn film_with_own_details_and_picture() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let (root_id, root_path) = root(&app, &admin, "/api/v1/rootfolders", "films", "movie").await;
    let folder = root_path.join("Wedding 2004");
    touch(&folder.join("wedding.mkv"));

    let body = json!({
        "kind": "movie", "root_folder_id": root_id, "path": format!("{}/", folder.display()),
        "title": "  Our   wedding ", "year": 2004, "overview": "The day itself.",
        "image_data": png_data(), "monitored": false,
    });
    let (s, item) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &manager,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    assert_eq!(item["kind"], "movie");
    assert_eq!(item["title"], "Our wedding");
    assert_eq!(item["path"], folder.to_str().unwrap());
    let id = item["id"].as_i64().unwrap();

    let (s, m) = call(&app, Method::GET, &format!("/api/v1/movies/{id}"), &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["custom"], true);
    assert_eq!(m["tmdb_id"], 0);
    assert_eq!(m["year"], 2004);
    assert_eq!(m["overview"], "The day itself.");
    assert_eq!(m["monitored"], false);
    let poster = m["images"]["poster"].as_str().unwrap().to_owned();
    assert!(
        poster.starts_with(&format!("/api/v1/customitems/movie/{id}/image?v=")),
        "{poster}"
    );
    let (s, list) = call(&app, Method::GET, "/api/v1/movies", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"][0]["custom"], true);

    // The picture is Reelix's own copy.
    let (s, h, bytes) = raw(&app, Method::GET, &poster, &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert_eq!(bytes, PNG);
    assert!(
        app.dir
            .path()
            .join(format!("data/custom/movie/{id}.png"))
            .is_file()
    );

    // The scan finds the film's file; a refresh keeps the typed details (no TMDB key here,
    // so a matched film would fail).
    app.library.scan_movie(id).await.unwrap();
    app.library.refresh_movie(id).await.unwrap();
    let m = app.library.get_movie(id).await.unwrap();
    assert!(m.has_file);
    assert_eq!(m.title, "Our wedding");

    // The same folder, a folder inside it, and a second custom film elsewhere.
    let (s, e) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &manager,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    std::fs::create_dir_all(folder.join("extras")).unwrap();
    let mut inside = body.clone();
    inside["path"] = json!(folder.join("extras").to_str().unwrap());
    let (s, _) = call(&app, Method::POST, "/api/v1/customitems", &manager, Some(inside)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    std::fs::create_dir_all(root_path.join("Holiday")).unwrap();
    let mut other = body.clone();
    other["path"] = json!(root_path.join("Holiday").to_str().unwrap());
    other["title"] = json!("Holiday");
    other.as_object_mut().unwrap().remove("image_data");
    let (s, second) = call(&app, Method::POST, "/api/v1/customitems", &manager, Some(other)).await;
    assert_eq!(s, StatusCode::CREATED, "{second}");
    let second = app
        .library
        .get_movie(second["id"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(second.tmdb_id, 0);
    assert_eq!(second.images.poster, None);

    // Changing the details later.
    let (s, changed) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/customitems/movie/{id}"),
        &manager,
        Some(json!({"title": "Our wedding day", "year": null, "remove_image": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{changed}");
    assert_eq!(changed["title"], "Our wedding day");
    let m = app.library.get_movie(id).await.unwrap();
    assert_eq!(m.year, None);
    assert_eq!(m.overview.as_deref(), Some("The day itself."));
    assert_eq!(m.images.poster, None);
    let (s, _) = call(
        &app,
        Method::GET,
        &format!("/api/v1/customitems/movie/{id}/image"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/customitems/movie/999",
        &manager,
        Some(json!({"title": "Nobody"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/customitems/film/{id}"),
        &manager,
        Some(json!({"title": "Nobody"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bad_requests_add_nothing() {
    let app = app().await;
    let admin = admin(&app).await;
    let (root_id, root_path) = root(&app, &admin, "/api/v1/rootfolders", "films", "movie").await;
    std::fs::create_dir_all(root_path.join("Clip")).unwrap();
    let elsewhere = app.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let ok = json!({"kind": "movie", "root_folder_id": root_id, "path": root_path.join("Clip").to_str().unwrap(), "title": "Clip"});
    let cases = [
        json!({"path": elsewhere.to_str().unwrap()}),
        json!({"path": root_path.join("Missing").to_str().unwrap()}),
        json!({"title": "   "}),
        json!({"title": "x".repeat(201)}),
        json!({"year": 1799}),
        json!({"overview": "o".repeat(4001)}),
        json!({"image_data": "data:image/gif;base64,R0lGODlh"}),
        json!({"image_data": format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(b"<html></html>"))}),
        json!({"image_url": "ftp://example.com/a.png"}),
        json!({"image_url": "https://example.com/a.png", "image_data": png_data()}),
        json!({"root_folder_id": 999}),
        json!({"root_folder_id": null}),
        json!({"quality_profile_id": 999}),
    ];
    for c in cases {
        let mut body = ok.clone();
        for (k, v) in c.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (s, e) = call(
            &app,
            Method::POST,
            "/api/v1/customitems",
            &admin,
            Some(body.clone()),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body} -> {e}");
        assert!(e["message"].is_string());
    }
    let (s, list) = call(&app, Method::GET, "/api/v1/movies", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 0);

    // A series root folder is not a film root folder; a film with a TMDB match cannot be
    // changed here.
    let (series_root, _) = root(&app, &admin, "/api/v1/rootfolders", "tv", "series").await;
    let mut body = ok.clone();
    body["root_folder_id"] = json!(series_root);
    let (s, _) = call(&app, Method::POST, "/api/v1/customitems", &admin, Some(body)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn series_episodes_come_from_file_names() {
    let app = app().await;
    let admin = admin(&app).await;
    let (root_id, root_path) = root(&app, &admin, "/api/v1/rootfolders", "tv", "series").await;
    let folder = root_path.join("Garden lectures");
    touch(&folder.join("Season 1/Garden lectures S01E01.mkv"));
    touch(&folder.join("Season 1/Garden lectures S01E02.mkv"));
    touch(&folder.join("Season 2/Garden lectures S02E01.mkv"));
    touch(&folder.join("notes.mkv"));
    let (s, item) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(
            json!({"kind": "series", "root_folder_id": root_id, "path": folder.to_str().unwrap(),
            "title": "Garden lectures", "year": 2019}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    let id = item["id"].as_i64().unwrap();
    let report = app.library.scan_series(id).await.unwrap();
    assert_eq!(report.added, 3);
    assert_eq!(report.unmatched.len(), 1);
    let eps = app.library.episodes(id, None).await.unwrap();
    let numbers: Vec<(u32, u32, bool)> = eps
        .iter()
        .map(|e| (e.season_number, e.episode_number, e.has_file))
        .collect();
    assert_eq!(numbers, [(1, 1, true), (1, 2, true), (2, 1, true)]);
    app.library.refresh_series(id).await.unwrap();
    let s = app.library.get_series(id).await.unwrap();
    assert!(s.custom);
    assert_eq!(s.tmdb_id, None);
    assert_eq!(s.title, "Garden lectures");
    assert_eq!(s.seasons.len(), 2);
    // A second scan adds nothing twice.
    let again = app.library.scan_series(id).await.unwrap();
    assert_eq!(again.added, 0);
    assert_eq!(app.library.episodes(id, None).await.unwrap().len(), 3);
}

#[tokio::test]
async fn author_books_come_from_folders_and_files() {
    let app = app().await;
    let admin = admin(&app).await;
    let (root_id, root_path) = root(&app, &admin, "/api/v1/bookrootfolders", "audiobooks", "audiobook").await;
    let folder = root_path.join("Jo Bloggs");
    touch(&folder.join("Garden Diary.m4b"));
    touch(&folder.join("Lectures/01.mp3"));
    touch(&folder.join("Lectures/02.mp3"));
    let (s, item) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(
            json!({"kind": "author", "root_folder_id": root_id, "path": folder.to_str().unwrap(),
            "title": "Jo Bloggs", "overview": "Gardener.", "image_data": png_data()}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    let id = item["id"].as_i64().unwrap();
    assert_eq!(item["path"], folder.to_str().unwrap());
    app.books.scan_authors(&[id]).await.unwrap();
    app.books.refresh_authors(Some(vec![id]), true).await.unwrap();
    let (s, a) = call(&app, Method::GET, &format!("/api/v1/authors/{id}"), &admin, None).await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(a["custom"], true);
    assert_eq!(a["openlibrary_id"], format!("custom:{id}"));
    assert_eq!(a["name"], "Jo Bloggs");
    assert_eq!(a["overview"], "Gardener.");
    assert_eq!(a["audio_path"], folder.to_str().unwrap());
    assert!(
        a["images"]["poster"]
            .as_str()
            .unwrap()
            .contains("/customitems/author/")
    );
    let (s, books) = call(
        &app,
        Method::GET,
        &format!("/api/v1/books?author_id={id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{books}");
    let items = books["items"].as_array().or(books.as_array()).unwrap().clone();
    let mut titles: Vec<String> = items
        .iter()
        .map(|b| b["title"].as_str().unwrap().to_owned())
        .collect();
    titles.sort();
    assert_eq!(titles, ["Garden Diary", "Lectures"]);
    assert!(items.iter().all(|b| !b["audiobook_file"].is_null()), "{books}");

    // The same folder again, and a change of name.
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(json!({"kind": "author", "root_folder_id": root_id, "path": folder.to_str().unwrap(), "title": "Jo"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, c) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/customitems/author/{id}"),
        &admin,
        Some(json!({"title": "Jo A. Bloggs"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(app.books.get_author(id).await.unwrap().name, "Jo A. Bloggs");
}

#[tokio::test]
async fn artist_albums_come_from_folders() {
    let app = app().await;
    let admin = admin(&app).await;
    let (root_id, root_path) = root(&app, &admin, "/api/v1/musicrootfolders", "music", "music").await;
    let folder = root_path.join("The Allotment Band");
    flac(&folder.join("Live at the Hall (2010)/01 Intro.flac"), 30);
    flac(&folder.join("Live at the Hall (2010)/02 Song.flac"), 40);
    flac(&folder.join("Demos/01 Demo.flac"), 20);
    let (s, item) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(
            json!({"kind": "artist", "root_folder_id": root_id, "path": folder.to_str().unwrap(),
            "title": "The Allotment Band"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    let id = item["id"].as_i64().unwrap();
    // A refresh leaves the artist alone (MusicBrainz knows nothing of it).
    assert_eq!(app.music.refresh_artists(Some(vec![id]), true).await.unwrap(), 0);
    let a = app.music.get_artist(id).await.unwrap();
    assert!(a.custom);
    assert_eq!(a.musicbrainz_id, format!("custom:{id}"));
    assert_eq!(a.name, "The Allotment Band");
    assert_eq!(a.statistics.album_count, 2);
    assert_eq!(a.statistics.track_file_count, 3);
    let (s, albums) = call(
        &app,
        Method::GET,
        &format!("/api/v1/albums?artist_id={id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{albums}");
    let items = albums["items"].as_array().or(albums.as_array()).unwrap().clone();
    let mut titles: Vec<String> = items
        .iter()
        .map(|b| b["title"].as_str().unwrap().to_owned())
        .collect();
    titles.sort();
    assert_eq!(titles, ["Demos", "Live at the Hall"]);
    // Scanning again makes no second copy.
    app.music.scan_artists(&[id]).await.unwrap();
    assert_eq!(app.music.get_artist(id).await.unwrap().statistics.album_count, 2);
}

#[tokio::test]
async fn radio_show_episodes_come_from_library_files() {
    let app = app().await;
    let admin = admin(&app).await;
    let files: Vec<NewEpisodeFile> = (1..=2)
        .map(|n| NewEpisodeFile {
            abs_item_id: "li_garden".into(),
            abs_episode_id: format!("ep_{n}"),
            item_title: Some("Garden Hour (local)".into()),
            title: Some(format!("Week {n}")),
            episode: Some(n),
            duration_secs: Some(1800),
            path: Some(format!("/podcasts/Garden Hour/week {n}.mp3")),
            ..NewEpisodeFile::default()
        })
        .collect();
    app.radio
        .upsert_episode_files(&files, chrono::Utc::now())
        .await
        .unwrap();

    let (s, e) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(json!({"kind": "radio_show", "abs_item_id": "li_unknown", "title": "Nothing"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    let (s, item) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(
            json!({"kind": "radio_show", "abs_item_id": "li_garden", "title": "Garden Hour",
            "year": 1998, "overview": "Weekly gardening.", "station": "Radio Leeds",
            "image_data": png_data(), "monitored": false}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    assert_eq!(item["path"], Value::Null);
    let id = item["id"].as_i64().unwrap();
    let (s, show) = call(
        &app,
        Method::GET,
        &format!("/api/v1/radio/shows/{id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{show}");
    assert_eq!(show["custom"], true);
    assert_eq!(show["provider"], "custom");
    assert_eq!(show["abs_item_id"], "li_garden");
    assert_eq!(show["station_name"], "Radio Leeds");
    assert_eq!(show["synopsis"], "Weekly gardening.");
    assert_eq!(show["has_artwork"], true);
    assert_eq!(show["statistics"]["episode_count"], 2);
    assert_eq!(show["statistics"]["in_library_count"], 2);
    let (s, h, _) = raw(
        &app,
        Method::GET,
        &format!("/api/v1/radio/shows/{id}/artwork"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");

    // A new file arrives; a refresh adds it and keeps the typed details.
    app.radio
        .upsert_episode_files(
            &[NewEpisodeFile {
                abs_item_id: "li_garden".into(),
                abs_episode_id: "ep_3".into(),
                title: Some("Week 3".into()),
                ..NewEpisodeFile::default()
            }],
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    app.radio.refresh_show(id).await.unwrap();
    let s = app.radio.get_show(id).await.unwrap();
    assert_eq!(s.title, "Garden Hour");
    assert_eq!(s.statistics.in_library_count, 3);

    // The podcast already belongs to the show; the station can be removed later.
    let (st, _) = call(
        &app,
        Method::POST,
        "/api/v1/customitems",
        &admin,
        Some(json!({"kind": "radio_show", "abs_item_id": "li_garden", "title": "Again"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, c) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/customitems/radio_show/{id}"),
        &admin,
        Some(json!({"station": null, "title": "The Garden Hour"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let s = app.radio.get_show(id).await.unwrap();
    assert_eq!(s.title, "The Garden Hour");
    assert_eq!(s.station_id, None);
}
