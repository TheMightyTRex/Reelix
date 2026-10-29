//! API flows for the music module against a fake MusicBrainz provider (service flows are
//! covered in reelix-music).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_decision::{ProfileItem, QualityProfile};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_music::provider::*;
use reelix_music::{MusicDeps, MusicService};
use reelix_parser::{Quality, QualityFamily};
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
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    library.set_queue(system.queue());
    let music = Arc::new(MusicService::new(MusicDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(FakeMb),
    }));
    music.set_queue(system.queue());
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config)
        .with_extension(library)
        .with_extension(music);
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

const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OKC: &str = "b1392450-e666-3926-a536-22c65f834433";

struct FakeMb;

fn group() -> ReleaseGroupMetadata {
    ReleaseGroupMetadata {
        id: OKC.into(),
        title: "OK Computer".into(),
        primary_type: Some("Album".into()),
        first_release_date: Some("1997-05-21".into()),
        artist_credit: vec![ArtistCredit {
            artist_id: ARTIST.into(),
            name: "Radiohead".into(),
            join_phrase: String::new(),
        }],
        ..Default::default()
    }
}

fn rel(id: &str, country: &str) -> ReleaseMetadata {
    ReleaseMetadata {
        id: id.into(),
        release_group_id: OKC.into(),
        title: "OK Computer".into(),
        status: Some("Official".into()),
        country: Some(country.into()),
        date: Some("1997-05-21".into()),
        media: vec![MediumMetadata {
            position: 1,
            format: Some("CD".into()),
            title: None,
            tracks: ["Airbag", "Paranoid Android"]
                .iter()
                .enumerate()
                .map(|(i, t)| TrackMetadata {
                    id: format!("{id}-{i}"),
                    recording_id: format!("rec-{i}"),
                    position: i as u32 + 1,
                    number: (i + 1).to_string(),
                    title: (*t).into(),
                    length_ms: Some(200_000),
                    artist_credit: vec![],
                })
                .collect(),
        }],
        ..Default::default()
    }
}

#[async_trait]
impl MusicMetadataProvider for FakeMb {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn search_artists(&self, _q: &str) -> Result<Vec<ArtistSearchHit>, MetaError> {
        Ok(vec![ArtistSearchHit {
            id: ARTIST.into(),
            name: "Radiohead".into(),
            sort_name: "Radiohead".into(),
            score: 100,
            ..Default::default()
        }])
    }
    async fn search_release_groups(
        &self,
        _q: &str,
        _artist: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn artist(&self, mbid: &str) -> Result<ArtistMetadata, MetaError> {
        if mbid != ARTIST {
            return Err(MetaError::NotFound);
        }
        Ok(ArtistMetadata {
            id: ARTIST.into(),
            name: "Radiohead".into(),
            sort_name: "Radiohead".into(),
            release_groups: vec![group()],
            ..Default::default()
        })
    }
    async fn release_group(&self, _mbid: &str) -> Result<ReleaseGroupMetadata, MetaError> {
        Ok(group())
    }
    async fn releases(&self, _group: &str) -> Result<Vec<ReleaseMetadata>, MetaError> {
        Ok(vec![rel("okc-gb", "GB"), rel("okc-us", "US")])
    }
    async fn release(&self, _mbid: &str) -> Result<ReleaseMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn cover_art(&self, _group: &str) -> Result<Option<CoverArt>, MetaError> {
        Ok(None)
    }
}

#[tokio::test]
async fn music_routes_flow_and_permissions() {
    let app = app().await;
    let admin = admin(&app).await;
    let manager = user_with(&app, "manager", Permissions::ROLE_MANAGER).await;
    let plain = user_with(&app, "plain", Permissions::ROLE_USER).await;
    let profile = app
        .state
        .db()
        .quality_profiles()
        .create(&QualityProfile {
            id: 0,
            name: "Music Test".into(),
            items: Quality::MUSIC
                .iter()
                .map(|q| ProfileItem::quality(*q, true))
                .collect(),
            cutoff: u32::from(Quality::Flac.id()),
            upgrade_allowed: true,
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
            format_items: vec![],
            language: None,
            family: QualityFamily::Music,
        })
        .await
        .unwrap();

    let (s, list) = call(&app, Method::GET, "/api/v1/artists", &manager, None).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 0);
    let (s, _) = call(&app, Method::GET, "/api/v1/artists", &plain, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Root folders: admin-only writes, kind must be music.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("music");
    std::fs::create_dir_all(&root).unwrap();
    let body = json!({"path": root.to_str().unwrap(), "kind": "music"});
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/musicrootfolders",
        &manager,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, e) = call(
        &app,
        Method::POST,
        "/api/v1/musicrootfolders",
        &admin,
        Some(json!({"path": root.to_str().unwrap(), "kind": "ebook"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    let (s, rf) = call(&app, Method::POST, "/api/v1/musicrootfolders", &admin, Some(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{rf}");
    let root_id = rf["id"].as_i64().unwrap();
    let (s, rfs) = call(&app, Method::GET, "/api/v1/musicrootfolders", &manager, None).await;
    assert_eq!((s, rfs.as_array().unwrap().len()), (StatusCode::OK, 1));
    // The movie API never sees music roots.
    let (_, movie_roots) = call(&app, Method::GET, "/api/v1/rootfolders", &manager, None).await;
    assert_eq!(movie_roots, json!([]));

    // Metadata profiles.
    let (s, mps) = call(&app, Method::GET, "/api/v1/metadataprofiles", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mps[0]["name"], "Standard");
    let mp = json!({"name": "Singles", "primary_types": ["single"], "secondary_types": [], "release_statuses": ["official"]});
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/metadataprofiles",
        &manager,
        Some(mp.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, created) = call(
        &app,
        Method::POST,
        "/api/v1/metadataprofiles",
        &admin,
        Some(mp.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let (s, _) = call(&app, Method::POST, "/api/v1/metadataprofiles", &admin, Some(mp)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let mp_id = created["id"].as_i64().unwrap();
    let (s, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/metadataprofiles/{mp_id}"),
        &admin,
        Some(json!({"name": "Bad", "primary_types": [], "secondary_types": [], "release_statuses": ["official"]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/metadataprofiles/{mp_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Lookup + add an artist.
    let (s, hits) = call(
        &app,
        Method::GET,
        "/api/v1/artists/lookup?term=radiohead",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hits[0]["musicbrainz_id"], ARTIST);
    assert_eq!(hits[0]["artist_id"], Value::Null);
    let add = json!({
        "musicbrainz_id": ARTIST,
        "quality_profile_id": profile.id,
        "metadata_profile_id": 1,
        "root_folder_id": root_id,
        "monitor": "all",
    });
    let (s, artist) = call(&app, Method::POST, "/api/v1/artists", &manager, Some(add.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{artist}");
    let artist_id = artist["id"].as_i64().unwrap();
    assert_eq!(artist["statistics"]["album_count"], 1);
    let (s, _) = call(&app, Method::POST, "/api/v1/artists", &manager, Some(add)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, bad) = call(
        &app,
        Method::POST,
        "/api/v1/artists",
        &manager,
        Some(json!({"musicbrainz_id": "nope", "quality_profile_id": profile.id, "metadata_profile_id": 1, "root_folder_id": root_id})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{bad}");

    // Albums, releases, tracks.
    let (s, albums) = call(
        &app,
        Method::GET,
        &format!("/api/v1/albums?artist_id={artist_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let album_id = albums["items"][0]["id"].as_i64().unwrap();
    let (s, album) = call(
        &app,
        Method::GET,
        &format!("/api/v1/albums/{album_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(album["releases"].as_array().unwrap().len(), 2);
    let (s, releases) = call(
        &app,
        Method::GET,
        &format!("/api/v1/albums/{album_id}/releases"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let unselected = releases
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["selected"] == false)
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (s, a) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/albums/{album_id}/release"),
        &manager,
        Some(json!({"release_id": unselected})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(a["selected_release_id"], unselected);
    let (s, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/albums/{album_id}/release"),
        &manager,
        Some(json!({"release_id": 999_999})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, tracks) = call(
        &app,
        Method::GET,
        &format!("/api/v1/tracks?album_id={album_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tracks.as_array().unwrap().len(), 2);
    assert_eq!(tracks[0]["release_id"], unselected);
    let (s, files) = call(
        &app,
        Method::GET,
        &format!("/api/v1/trackfiles?album_id={album_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!((s, files), (StatusCode::OK, json!([])));
    let (s, _) = call(&app, Method::DELETE, "/api/v1/trackfiles/12345", &manager, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, a) = call(
        &app,
        Method::PUT,
        "/api/v1/albums/monitor",
        &manager,
        Some(json!({"album_ids": [album_id], "monitored": false})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a[0]["monitored"], false);
    let (s, a) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/albums/{album_id}"),
        &manager,
        Some(json!({"monitored": true})),
    )
    .await;
    assert_eq!((s, a["monitored"].clone()), (StatusCode::OK, json!(true)));
    let (s, wanted) = call(&app, Method::GET, "/api/v1/albums/wanted", &manager, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(wanted["total"], 1);
    let (s, _) = call(
        &app,
        Method::GET,
        "/api/v1/albums/wanted?kind=bogus",
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Jobs → 202.
    for (uri, body) in [
        (format!("/api/v1/albums/{album_id}/search"), None),
        (
            "/api/v1/music/search".to_string(),
            Some(json!({"kind": "missing"})),
        ),
        (format!("/api/v1/artists/{artist_id}/refresh"), None),
        (format!("/api/v1/artists/{artist_id}/rescan"), None),
        (format!("/api/v1/music/rename?artist_id={artist_id}"), None),
    ] {
        let (s, j) = call(&app, Method::POST, &uri, &manager, body).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{uri}: {j}");
    }
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/music/search",
        &manager,
        Some(json!({"kind": "item"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, preview) = call(
        &app,
        Method::GET,
        &format!("/api/v1/music/rename?artist_id={artist_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!((s, preview), (StatusCode::OK, json!([])));

    // Settings: admin only, templates validated.
    let (s, _) = call(&app, Method::GET, "/api/v1/settings/music", &manager, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, mut settings) = call(&app, Method::GET, "/api/v1/settings/music", &admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(settings["naming"]["track_file"], "{track:00} - {Track Title}");
    settings["naming"]["track_file"] = json!("{Movie Title}");
    let (s, _) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/music",
        &admin,
        Some(settings.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    settings["naming"]["track_file"] = json!("{track:00}. {Track Title}");
    settings["preferred_release_countries"] = json!(["us", " gb "]);
    let (s, saved) = call(
        &app,
        Method::PUT,
        "/api/v1/settings/music",
        &admin,
        Some(settings),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(saved["preferred_release_countries"], json!(["US", "GB"]));

    // Library import of an unmapped folder; root folder in use.
    std::fs::create_dir_all(root.join("Some Band")).unwrap();
    let (s, cands) = call(
        &app,
        Method::GET,
        &format!("/api/v1/musicimport/scan?root_folder_id={root_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cands}");
    assert_eq!(cands[0]["folder"], "Some Band");
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/musicrootfolders/{root_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Artist edit + delete.
    let (s, a) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/artists/{artist_id}"),
        &manager,
        Some(json!({"monitored": false, "tags": [3]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (a["monitored"].clone(), a["tags"].clone()),
        (json!(false), json!([3]))
    );
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/artists/{artist_id}?delete_files=true"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        Method::GET,
        &format!("/api/v1/artists/{artist_id}"),
        &manager,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/musicrootfolders/{root_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn music_openapi_lists_the_routes() {
    let doc = serde_json::to_value(reelix_api::openapi()).unwrap();
    for p in [
        "/api/v1/artists",
        "/api/v1/albums/{id}/release",
        "/api/v1/trackfiles",
        "/api/v1/settings/music",
    ] {
        assert!(
            doc["paths"].get(p).is_some(),
            "{p} missing from the OpenAPI document"
        );
    }
}
