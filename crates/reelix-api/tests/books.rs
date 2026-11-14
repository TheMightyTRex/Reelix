//! API flows for the books module against a fake Open Library (service flows are covered in
//! reelix-books).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_books::testing::{FakeBookMetadata, author, work};
use reelix_books::{BooksDeps, BooksService};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestApp {
    router: Router,
    state: AppState,
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
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    library.set_queue(system.queue());
    let tolkien = author("OL26320A", "J.R.R. Tolkien");
    let meta = FakeBookMetadata::new()
        .with_author(tolkien.clone())
        .with_work(work("OL27448W", "The Hobbit", &tolkien, Some(1937)))
        .with_work(work("OL27479W", "The Silmarillion", &tolkien, Some(1977)));
    let books = Arc::new(BooksService::new(BooksDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(meta),
    }));
    books.set_queue(system.queue());
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(library)
        .with_extension(books);
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        dir,
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

fn session_cookie(res: &axum::response::Response) -> String {
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
    session_cookie(&res)
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
    session_cookie(&res)
}

#[tokio::test]
async fn books_routes_flow_and_permissions() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let plain = user_with(&app, "plain", Permissions::ROLE_USER).await;

    let (s, _) = call(&app, Method::GET, "/api/v1/authors", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, list) = call(&app, Method::GET, "/api/v1/authors", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 0);

    // Root folders (admin only for changes).
    let ebooks = app.dir.path().join("ebooks");
    let audio = app.dir.path().join("audiobooks");
    std::fs::create_dir_all(&ebooks).unwrap();
    std::fs::create_dir_all(&audio).unwrap();
    let body = json!({"path": ebooks.to_string_lossy(), "kind": "ebook"});
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/bookrootfolders",
        &manager,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, root) = call(
        &app,
        Method::POST,
        "/api/v1/bookrootfolders",
        &admin,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{root}");
    assert_eq!(root["kind"], "ebook");
    let (s, _) = call(&app, Method::POST, "/api/v1/bookrootfolders", &admin, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, aroot) = call(
        &app,
        Method::POST,
        "/api/v1/bookrootfolders",
        &admin,
        Some(json!({"path": audio.to_string_lossy(), "kind": "audiobook"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, roots) = call(&app, Method::GET, "/api/v1/bookrootfolders", &manager, None).await;
    assert_eq!(roots.as_array().unwrap().len(), 2);
    // Book roots never show up as movie/series roots.
    let (_, movie_roots) = call(&app, Method::GET, "/api/v1/rootfolders", &manager, None).await;
    assert_eq!(movie_roots, json!([]));

    let (_, profiles) = call(
        &app,
        Method::GET,
        "/api/v1/qualityprofiles?kind=ebook",
        &manager,
        None,
    )
    .await;
    let profile = profiles[0]["id"].as_i64().unwrap();
    let (_, aprofiles) = call(
        &app,
        Method::GET,
        "/api/v1/qualityprofiles?kind=audiobook",
        &manager,
        None,
    )
    .await;
    let aprofile = aprofiles[0]["id"].as_i64().unwrap();

    // Lookup and add.
    let (s, hits) = call(
        &app,
        Method::GET,
        "/api/v1/authors/lookup?term=tolkien",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hits[0]["openlibrary_id"], "OL26320A");
    assert_eq!(hits[0]["author_id"], Value::Null);
    let (s, err) = call(
        &app,
        Method::POST,
        "/api/v1/authors",
        &manager,
        Some(json!({"openlibrary_id": "OL26320A", "quality_profile_id": aprofile, "root_folder_id": root["id"]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");
    let (s, err) = call(
        &app,
        Method::POST,
        "/api/v1/authors",
        &manager,
        Some(
            json!({"openlibrary_id": "OL404A", "quality_profile_id": profile, "root_folder_id": root["id"]}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{err}");
    let (s, a) = call(
        &app,
        Method::POST,
        "/api/v1/authors",
        &manager,
        Some(json!({
            "openlibrary_id": "OL26320A",
            "quality_profile_id": profile,
            "root_folder_id": root["id"],
            "audio_quality_profile_id": aprofile,
            "audio_root_folder_id": aroot["id"],
            "monitor": "selected",
            "selected_work_ids": ["OL27448W"],
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let id = a["id"].as_i64().unwrap();
    assert_eq!(a["statistics"]["book_count"], 2);
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/authors",
        &manager,
        Some(json!({"openlibrary_id": "OL26320A", "quality_profile_id": profile, "root_folder_id": root["id"]})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    let (s, got) = call(
        &app,
        Method::GET,
        &format!("/api/v1/authors/{id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["name"], "J.R.R. Tolkien");
    let (_, books) = call(
        &app,
        Method::GET,
        &format!("/api/v1/books?author_id={id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(books["total"], 2);
    let hobbit = books["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["title"] == "The Hobbit")
        .unwrap()
        .clone();
    assert_eq!(hobbit["monitored_ebook"], true);
    assert_eq!(hobbit["monitored_audiobook"], true);
    let hid = hobbit["id"].as_i64().unwrap();
    let (s, b) = call(&app, Method::GET, &format!("/api/v1/books/{hid}"), &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["openlibrary_work_id"], "OL27448W");
    let (_, bl) = call(
        &app,
        Method::GET,
        "/api/v1/books/lookup?term=hobbit",
        &manager,
        None,
    )
    .await;
    assert_eq!(bl[0]["book_id"], hid);

    let (s, m) = call(
        &app,
        Method::PUT,
        "/api/v1/books/monitor",
        &manager,
        Some(json!({"book_ids": [hid], "format": "audiobook", "monitored": false})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m[0]["monitored_audiobook"], false);
    let (s, e) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/books/{hid}"),
        &manager,
        Some(json!({"monitored_audiobook": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(e["monitored_audiobook"], true);
    let (s, edited) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/authors/{id}"),
        &manager,
        Some(json!({"audio_quality_profile_id": null, "tags": [2]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{edited}");
    assert_eq!(edited["audio_quality_profile_id"], Value::Null);
    assert_eq!(edited["tags"], json!([2]));

    // Jobs.
    for uri in [
        format!("/api/v1/authors/{id}/refresh"),
        format!("/api/v1/authors/{id}/rescan"),
        format!("/api/v1/books/rename?author_id={id}"),
    ] {
        let (s, j) = call(&app, Method::POST, &uri, &manager, None).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{uri}");
        // The rescan is deduplicated against the scan queued when the author was added.
        assert_eq!(j["job_id"].is_i64(), !uri.ends_with("rescan"), "{uri}: {j}");
    }
    let (s, j) = call(
        &app,
        Method::POST,
        "/api/v1/books/search",
        &manager,
        Some(json!({"kind": "item", "ids": [hid], "format": "ebook"})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(j["job_id"].is_i64());
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/books/search",
        &manager,
        Some(json!({"kind": "parent", "ids": []})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&app, Method::POST, "/api/v1/authors/999/refresh", &manager, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Reads.
    let (s, r) = call(
        &app,
        Method::GET,
        &format!("/api/v1/books/rename?author_id={id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!((s, r), (StatusCode::OK, json!([])));
    let (s, f) = call(
        &app,
        Method::GET,
        &format!("/api/v1/bookfiles?book_id={hid}"),
        &manager,
        None,
    )
    .await;
    assert_eq!((s, f), (StatusCode::OK, json!([])));
    let (s, _) = call(&app, Method::DELETE, "/api/v1/bookfiles/999", &manager, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, w) = call(
        &app,
        Method::GET,
        "/api/v1/books/wanted?kind=missing",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(w["items"][0]["id"], hid);
    let (s, ed) = call(
        &app,
        Method::GET,
        &format!("/api/v1/books/{hid}/editions"),
        &manager,
        None,
    )
    .await;
    assert_eq!((s, ed), (StatusCode::OK, json!([])));
    let (s, series) = call(&app, Method::GET, "/api/v1/bookseries", &manager, None).await;
    assert_eq!((s, series), (StatusCode::OK, json!([])));
    std::fs::create_dir_all(ebooks.join("Someone Else")).unwrap();
    let (s, cands) = call(
        &app,
        Method::GET,
        &format!("/api/v1/bookimport/scan?root_folder_id={}", root["id"]),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cands}");
    assert_eq!(cands[0]["folder"], "Someone Else");
    let (s, r) = call(
        &app,
        Method::POST,
        "/api/v1/bookimport",
        &manager,
        Some(json!({"root_folder_id": root["id"], "items": [{"path": "/elsewhere", "openlibrary_id": "OL1A"}]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["errors"].as_array().unwrap().len(), 1);

    // Settings: admin only, validated.
    let (s, _) = call(&app, Method::GET, "/api/v1/settings/books", &manager, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, mut settings) = call(&app, Method::GET, "/api/v1/settings/books", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(settings["write_metadata"], true);
    settings["naming"]["ebook_file"] = json!("{Book Title} {Nope}");
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/books",
        &admin,
        Some(settings.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    settings["naming"]["ebook_file"] = json!("{Author Name} - {Book Title}");
    let (s, saved) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/books",
        &admin,
        Some(settings),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(saved["naming"]["ebook_file"], "{Author Name} - {Book Title}");
    assert_eq!(saved["audiobook_metadata"]["audible"], true);
    assert_eq!(saved["audiobook_metadata"]["librofm"], false);

    // Audiobook sources (RFX-86): the region is checked; with every source deactivated a
    // lookup says so instead of asking anyone.
    let mut sources = saved.clone();
    sources["audiobook_metadata"]["region"] = json!("xx");
    let (s, err) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/books",
        &admin,
        Some(sources.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");
    sources["audiobook_metadata"] =
        json!({"audible": false, "audnexus": false, "librofm": false, "region": " UK "});
    let (s, stored) = call(&app, Method::PUT, "/api/v1/settings/books", &admin, Some(sources)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(stored["audiobook_metadata"]["region"], "uk");
    let (s, _) = call(
        &app,
        Method::GET,
        &format!("/api/v1/books/{hid}/audiobook"),
        &plain,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, err) = call(
        &app,
        Method::GET,
        &format!("/api/v1/books/{hid}/audiobook"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/books/999999/audiobook/refresh",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Deletes.
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/bookrootfolders/{}", root["id"]),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/books/{hid}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/authors/{id}?delete_files=true"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::GET,
        &format!("/api/v1/authors/{id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/bookrootfolders/{}", root["id"]),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn openapi_documents_book_routes() {
    let app = app().await;
    let req = Request::builder()
        .uri("/api/v1/openapi.json")
        .body(Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    for p in [
        "/api/v1/authors",
        "/api/v1/authors/{id}",
        "/api/v1/books/monitor",
        "/api/v1/books/rename",
        "/api/v1/bookfiles",
        "/api/v1/bookrootfolders",
        "/api/v1/bookimport/scan",
        "/api/v1/settings/books",
        "/api/v1/books/{id}/audiobook",
        "/api/v1/books/{id}/audiobook/refresh",
    ] {
        assert!(doc["paths"][p].is_object(), "{p} missing");
    }
    assert!(doc["components"]["schemas"]["AuthorDto"].is_object());
    assert!(doc["components"]["schemas"]["AudiobookDetailsDto"].is_object());
}
