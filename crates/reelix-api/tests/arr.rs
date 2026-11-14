//! The Radarr/Sonarr/Readarr/Lidarr compatible APIs, driven the way *arr clients call them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
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
use reelix_library::{LibraryService, MetadataSource};
use reelix_metadata::*;
use serde_json::{Value, json};
use tower::ServiceExt;

fn movie(id: u32, title: &str, year: i32) -> MovieMetadata {
    MovieMetadata {
        tmdb_id: id,
        imdb_id: Some(format!("tt{id:07}")),
        title: title.into(),
        original_title: title.into(),
        original_language: Some("en".into()),
        sort_title: title.to_lowercase(),
        overview: None,
        tagline: None,
        year: Some(year),
        release_date: None,
        release_dates: ReleaseDates::default(),
        certification: None,
        status: MovieStatus::Released,
        runtime_minutes: Some(120),
        genres: vec![],
        keywords: vec![],
        studios: vec![],
        collection: None,
        alternative_titles: vec![],
        external_ids: ExternalIds::default(),
        credits: Credits::default(),
        images: Images::default(),
        videos: vec![],
        ratings: Ratings::default(),
        adult: false,
        homepage: None,
        recommendations: vec![],
        similar: vec![],
    }
}

fn episode(season: u32, n: u32) -> EpisodeMetadata {
    EpisodeMetadata {
        season_number: season,
        episode_number: n,
        absolute_number: None,
        title: Some(format!("Episode {n}")),
        overview: None,
        air_date: chrono::NaiveDate::from_ymd_opt(2020 + season as i32, 1, n),
        runtime_minutes: Some(45),
        still: None,
        tmdb_id: None,
        tvdb_id: None,
        finale: None,
        vote_average: None,
    }
}

fn season(n: u32) -> SeasonMetadata {
    let eps = vec![episode(n, 1), episode(n, 2)];
    SeasonMetadata {
        season_number: n,
        name: Some(format!("Season {n}")),
        overview: None,
        air_date: eps[0].air_date,
        poster: None,
        episode_count: 2,
        episodes: eps,
        tmdb_id: None,
        tvdb_id: None,
    }
}

fn show(id: u32, tvdb: u32, title: &str) -> SeriesMetadata {
    SeriesMetadata {
        tmdb_id: Some(id),
        tvdb_id: Some(tvdb),
        imdb_id: None,
        title: title.into(),
        original_title: None,
        sort_title: title.to_lowercase(),
        overview: None,
        year: Some(2020),
        first_air_date: chrono::NaiveDate::from_ymd_opt(2020, 1, 1),
        last_air_date: None,
        next_air_date: None,
        status: SeriesStatus::Continuing,
        show_type: None,
        network: None,
        networks: vec![],
        genres: vec![],
        keywords: vec![],
        runtime_minutes: Some(45),
        certification: None,
        original_language: Some("en".into()),
        origin_country: vec![],
        alternative_titles: vec![],
        external_ids: ExternalIds::default(),
        credits: Credits::default(),
        images: Images::default(),
        videos: vec![],
        ratings: Ratings::default(),
        seasons: vec![season(1), season(2)],
        recommendations: vec![],
        similar: vec![],
    }
}

#[derive(Default)]
struct FakeMeta {
    movies: Mutex<HashMap<u32, MovieMetadata>>,
    series: Mutex<HashMap<u32, SeriesMetadata>>,
}

#[async_trait]
impl MetadataSource for FakeMeta {
    async fn movie(&self, id: u32) -> Result<MovieMetadata, MetaError> {
        self.movies
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        let map = self.series.lock().unwrap();
        match id {
            SeriesExternalId::Tmdb(t) => map.get(&t).cloned().ok_or(MetaError::NotFound),
            SeriesExternalId::Tvdb(t) => map
                .values()
                .find(|s| s.tvdb_id == Some(t))
                .cloned()
                .ok_or(MetaError::NotFound),
            SeriesExternalId::Imdb(_) => Err(MetaError::NotFound),
        }
    }
    async fn collection(&self, _id: u32) -> Result<CollectionMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn search(
        &self,
        _q: &str,
        _k: reelix_core::MediaKind,
        _y: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn find(
        &self,
        _s: ExternalSource,
        _id: &str,
        _k: reelix_core::MediaKind,
    ) -> Result<Vec<SearchHit>, MetaError> {
        Ok(vec![])
    }
}

/// A health source whose findings a test sets.
#[derive(Default)]
struct FakeHealth(Mutex<Vec<reelix_api_types::HealthCheck>>);

impl reelix_api::HealthSource for FakeHealth {
    fn checks(&self) -> Vec<reelix_api_types::HealthCheck> {
        self.0.lock().unwrap().clone()
    }
}

struct TestApp {
    router: Router,
    health: Arc<FakeHealth>,
    key: String,
    admin_key: String,
    viewer_key: String,
    movies_dir: tempfile::TempDir,
    tv_dir: tempfile::TempDir,
    _data: tempfile::TempDir,
}

async fn key_for(db: &Db, name: &str, permissions: Permissions) -> String {
    let user = db
        .users()
        .create(NewUser {
            username: name.into(),
            email: None,
            display_name: None,
            password_hash: None,
            auth_source: AuthSource::Local,
            permissions,
        })
        .await
        .unwrap();
    let key = reelix_api::auth::random_token();
    db.api_keys()
        .create(user.id, "test", &key[..8], &reelix_api::auth::sha256_hex(&key))
        .await
        .unwrap();
    key
}

async fn app() -> TestApp {
    let data = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let system = JobSystem::builder(db.clone(), events.clone()).build();
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: data.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    let meta = Arc::new(FakeMeta::default());
    meta.movies
        .lock()
        .unwrap()
        .insert(603, movie(603, "The Matrix", 1999));
    meta.series
        .lock()
        .unwrap()
        .insert(1399, show(1399, 2399, "The Show"));
    let library = Arc::new(LibraryService::new(db.clone(), events.clone(), meta));
    library.set_queue(system.queue());
    let mut mm = library.media_management().await.unwrap();
    mm.analyze_video_files = false;
    library.set_media_management(mm).await.unwrap();
    let movies_dir = tempfile::tempdir().unwrap();
    let tv_dir = tempfile::tempdir().unwrap();
    library
        .add_root_folder(movies_dir.path().to_str().unwrap(), reelix_core::MediaKind::Movie)
        .await
        .unwrap();
    library
        .add_root_folder(tv_dir.path().to_str().unwrap(), reelix_core::MediaKind::Series)
        .await
        .unwrap();
    let providers = reelix_providers::ProviderService::new(
        db.clone(),
        SecretBox::from_key([3u8; 32]),
        events.clone(),
        reqwest::Client::new(),
        reelix_providers::ProviderOptions::new(data.path()),
    )
    .await
    .unwrap();
    let key = key_for(&db, "homepage", Permissions::ROLE_MANAGER).await;
    let viewer_key = key_for(&db, "viewer", Permissions::ROLE_USER).await;
    let admin_key = key_for(&db, "recyclarr", Permissions::ADMIN).await;
    let health = Arc::new(FakeHealth::default());
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([3u8; 32]), config)
        .with_extension(library)
        .with_extension(providers)
        .with_extension(Arc::new(reelix_api::HealthSources(vec![
            health.clone() as Arc<dyn reelix_api::HealthSource>
        ])));
    TestApp {
        router: reelix_api::router(state),
        health,
        key,
        admin_key,
        viewer_key,
        movies_dir,
        tv_dir,
        _data: data,
    }
}

async fn call(app: &TestApp, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_as(app, &app.key, method, uri, body).await
}

async fn call_as(
    app: &TestApp,
    key: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", key);
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn status_and_auth_per_app() {
    let app = app().await;
    for (prefix, name) in [
        ("/radarr/api/v3", "Radarr"),
        ("/sonarr/api/v3", "Sonarr"),
        ("/readarr/api/v1", "Readarr"),
        ("/lidarr/api/v1", "Lidarr"),
    ] {
        let (s, v) = call(&app, Method::GET, &format!("{prefix}/system/status"), None).await;
        assert_eq!(s, StatusCode::OK, "{prefix}: {v}");
        assert_eq!(v["appName"], name);
        assert!(v["version"].as_str().unwrap().contains('.'));
    }
    // The apikey query works like the header.
    let req = Request::builder()
        .uri(format!("/radarr/api/v3/system/status?apikey={}", app.key))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let (s, _) = call_as(&app, "nope", Method::GET, "/radarr/api/v3/movie", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call_as(
        &app,
        &app.viewer_key.clone(),
        Method::GET,
        "/radarr/api/v3/movie",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "needs Manage library");
}

#[tokio::test]
async fn health_lists_native_problems_in_arr_shape() {
    use reelix_api_types::{HealthCheck, HealthLevel};
    let app = app().await;
    // Healthy: the database and data folder checks pass and *arr lists only problems.
    for prefix in [
        "/radarr/api/v3",
        "/sonarr/api/v3",
        "/readarr/api/v1",
        "/lidarr/api/v1",
    ] {
        let (s, v) = call(&app, Method::GET, &format!("{prefix}/health"), None).await;
        assert_eq!(s, StatusCode::OK, "{prefix}: {v}");
        assert_eq!(v, json!([]), "{prefix}");
    }
    *app.health.0.lock().unwrap() = vec![
        HealthCheck {
            source: "indexers".into(),
            level: HealthLevel::Warning,
            message: "Indexer Example is failing".into(),
        },
        HealthCheck {
            source: "downloadclients".into(),
            level: HealthLevel::Ok,
            message: "All download clients are reachable".into(),
        },
        HealthCheck {
            source: "disk".into(),
            level: HealthLevel::Error,
            message: "Root folder /movies is missing".into(),
        },
    ];
    for prefix in ["/radarr/api/v3", "/sonarr/api/v3"] {
        let (s, v) = call(&app, Method::GET, &format!("{prefix}/health"), None).await;
        assert_eq!(s, StatusCode::OK, "{prefix}: {v}");
        assert_eq!(
            v,
            json!([
                {"id": 1, "source": "indexers", "type": "warning",
                 "message": "Indexer Example is failing", "wikiUrl": ""},
                {"id": 2, "source": "disk", "type": "error",
                 "message": "Root folder /movies is missing", "wikiUrl": ""},
            ]),
            "{prefix}"
        );
    }
    // Messages name indexers and paths, so the Manage library right is still needed.
    let (s, _) = call_as(
        &app,
        &app.viewer_key.clone(),
        Method::GET,
        "/sonarr/api/v3/health",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn radarr_movies_files_and_commands() {
    let app = app().await;
    let (s, roots) = call(&app, Method::GET, "/radarr/api/v3/rootfolder", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        roots.as_array().unwrap().len(),
        1,
        "only movie root folders: {roots}"
    );
    let root = roots[0]["path"].as_str().unwrap().to_owned();
    let (_, profiles) = call(&app, Method::GET, "/radarr/api/v3/qualityprofile", None).await;
    let profile = profiles[0]["id"].as_i64().unwrap();
    assert!(profiles[0]["items"].as_array().unwrap().len() > 3);

    // Add like Overseerr/LunaSea do.
    let (s, m) = call(
        &app,
        Method::POST,
        "/radarr/api/v3/movie",
        Some(json!({
            "tmdbId": 603, "title": "The Matrix", "qualityProfileId": profile, "rootFolderPath": root,
            "monitored": true, "minimumAvailability": "released", "tags": [],
            "addOptions": {"searchForMovie": false, "monitor": "movieOnly"}
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let id = m["id"].as_i64().unwrap();
    assert_eq!(m["tmdbId"], 603);
    assert_eq!(m["hasFile"], false);
    assert_eq!(m["monitored"], true);
    assert_eq!(m["titleSlug"], "the-matrix-603");
    assert_eq!(
        m["rootFolderPath"].as_str().unwrap().trim_end_matches('/'),
        root.trim_end_matches('/')
    );

    let (_, list) = call(&app, Method::GET, "/radarr/api/v3/movie", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, by_tmdb) = call(&app, Method::GET, "/radarr/api/v3/movie?tmdbId=603", None).await;
    assert_eq!(by_tmdb[0]["id"], id);
    let (s, _) = call(&app, Method::GET, "/radarr/api/v3/movie/9999", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Edit by PUTting the movie back (Radarr clients send the whole object).
    let mut body = m.clone();
    body["monitored"] = json!(false);
    let (s, edited) = call(
        &app,
        Method::PUT,
        &format!("/radarr/api/v3/movie/{id}"),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{edited}");
    assert_eq!(edited["monitored"], false);

    let (_, files) = call(
        &app,
        Method::GET,
        &format!("/radarr/api/v3/moviefile?movieId={id}"),
        None,
    )
    .await;
    assert_eq!(files, json!([]));

    // Wanted: unmonitored now, so nothing missing unless asked.
    let (_, wanted) = call(
        &app,
        Method::GET,
        "/radarr/api/v3/wanted/missing?page=1&pageSize=10",
        None,
    )
    .await;
    assert_eq!(wanted["totalRecords"], 0, "{wanted}");
    assert_eq!(wanted["records"], json!([]));

    // Commands are queued as jobs and can be polled.
    let (s, cmd) = call(
        &app,
        Method::POST,
        "/radarr/api/v3/command",
        Some(json!({"name": "MoviesSearch", "movieIds": [id]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{cmd}");
    assert_eq!(cmd["name"], "MoviesSearch");
    let job = cmd["id"].as_i64().unwrap();
    assert!(job > 0);
    let (s, polled) = call(&app, Method::GET, &format!("/radarr/api/v3/command/{job}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(polled["status"], "queued");
    let (s, err) = call(
        &app,
        Method::POST,
        "/radarr/api/v3/command",
        Some(json!({"name": "Backup"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");

    // Tags: creating an existing label returns it.
    let (s, t) = call(
        &app,
        Method::POST,
        "/radarr/api/v3/tag",
        Some(json!({"label": "kids"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, again) = call(
        &app,
        Method::POST,
        "/radarr/api/v3/tag",
        Some(json!({"label": "Kids"})),
    )
    .await;
    assert_eq!(t["id"], again["id"]);

    let (s, _) = call(
        &app,
        Method::DELETE,
        &format!("/radarr/api/v3/movie/{id}?deleteFiles=false"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, list) = call(&app, Method::GET, "/radarr/api/v3/movie", None).await;
    assert_eq!(list, json!([]));
    let _ = &app.movies_dir;
}

#[tokio::test]
async fn sonarr_series_episodes_and_seasons() {
    let app = app().await;
    let root = app.tv_dir.path().to_str().unwrap().to_owned();
    let (_, profiles) = call(&app, Method::GET, "/sonarr/api/v3/qualityprofile", None).await;
    let profile = profiles[0]["id"].as_i64().unwrap();
    let (_, langs) = call(&app, Method::GET, "/sonarr/api/v3/languageprofile", None).await;
    assert_eq!(langs[0]["id"], 1);

    let (s, series) = call(
        &app,
        Method::POST,
        "/sonarr/api/v3/series",
        Some(json!({
            "tvdbId": 2399, "title": "The Show", "qualityProfileId": profile, "languageProfileId": 1,
            "rootFolderPath": root, "seasonFolder": true, "monitored": true, "seriesType": "standard",
            "seasons": [{"seasonNumber": 1, "monitored": true}, {"seasonNumber": 2, "monitored": false}],
            "addOptions": {"searchForMissingEpisodes": false}
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{series}");
    let id = series["id"].as_i64().unwrap();
    assert_eq!(series["tvdbId"], 2399);
    let seasons = series["seasons"].as_array().unwrap();
    let mon = |n: u64| seasons.iter().find(|s| s["seasonNumber"] == n).unwrap()["monitored"].clone();
    assert_eq!(mon(1), true);
    assert_eq!(mon(2), false);

    let (_, eps) = call(
        &app,
        Method::GET,
        &format!("/sonarr/api/v3/episode?seriesId={id}"),
        None,
    )
    .await;
    let eps = eps.as_array().unwrap();
    assert_eq!(eps.len(), 4);
    let s2e1 = eps
        .iter()
        .find(|e| e["seasonNumber"] == 2 && e["episodeNumber"] == 1)
        .unwrap();
    assert_eq!(s2e1["monitored"], false);
    assert_eq!(s2e1["hasFile"], false);
    let ep_id = s2e1["id"].as_i64().unwrap();

    // Toggle one episode, then a season through a series PUT.
    let (s, e) = call(
        &app,
        Method::PUT,
        &format!("/sonarr/api/v3/episode/{ep_id}"),
        Some(json!({"monitored": true})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{e}");
    assert_eq!(e["monitored"], true);
    assert_eq!(e["series"]["id"], id);
    let mut body = series.clone();
    body["seasons"] =
        json!([{"seasonNumber": 1, "monitored": false}, {"seasonNumber": 2, "monitored": true}]);
    let (s, updated) = call(
        &app,
        Method::PUT,
        &format!("/sonarr/api/v3/series/{id}"),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{updated}");
    let (_, eps) = call(
        &app,
        Method::GET,
        &format!("/sonarr/api/v3/episode?seriesId={id}&seasonNumber=1"),
        None,
    )
    .await;
    assert!(
        eps.as_array().unwrap().iter().all(|e| e["monitored"] == false),
        "{eps}"
    );

    // Wanted/missing returns episodes, with the series when asked.
    let (_, wanted) = call(
        &app,
        Method::GET,
        "/sonarr/api/v3/wanted/missing?page=1&pageSize=50&includeSeries=true",
        None,
    )
    .await;
    assert_eq!(wanted["totalRecords"], 2, "{wanted}");
    assert_eq!(wanted["records"][0]["series"]["title"], "The Show");

    let (s, cmd) = call(
        &app,
        Method::POST,
        "/sonarr/api/v3/command",
        Some(json!({"name": "SeasonSearch", "seriesId": id, "seasonNumber": 2})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{cmd}");
    let (s, _) = call(
        &app,
        Method::POST,
        "/sonarr/api/v3/command",
        Some(json!({"name": "SeasonSearch", "seriesId": id})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "season number is required");
}

#[tokio::test]
async fn recyclarr_style_profile_format_and_definition_sync() {
    let app = app().await;
    let admin = app.admin_key.clone();
    // Settings writes need an admin key, as in the native API.
    let (s, _) = call(
        &app,
        Method::POST,
        "/radarr/api/v3/customformat",
        Some(json!({"name": "x", "specifications": []})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Custom format in the Radarr/TRaSH shape.
    let (s, cf) = call_as(
        &app,
        &admin,
        Method::POST,
        "/radarr/api/v3/customformat",
        Some(json!({
            "name": "x265 (HD)", "includeCustomFormatWhenRenaming": false,
            "specifications": [{
                "name": "x265", "implementation": "ReleaseTitleSpecification", "negate": false, "required": true,
                "fields": [{"name": "value", "value": "[xh][ .]?265|\\bHEVC(\\b|\\d)"}]
            }]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{cf}");
    let cf_id = cf["id"].as_i64().unwrap();
    assert_eq!(cf["name"], "x265 (HD)");
    let (_, list) = call(&app, Method::GET, "/radarr/api/v3/customformat", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        list[0]["specifications"][0]["implementation"],
        "ReleaseTitleSpecification"
    );

    // Score it in a profile by PUTting the profile back with formatItems.
    let (_, profiles) = call(&app, Method::GET, "/radarr/api/v3/qualityprofile", None).await;
    let mut p = profiles[0].clone();
    let pid = p["id"].as_i64().unwrap();
    assert_eq!(p["formatItems"][0]["format"], cf_id);
    p["formatItems"] = json!([{"format": cf_id, "name": "x265 (HD)", "score": -10000}]);
    p["minFormatScore"] = json!(0);
    let (s, saved) = call_as(
        &app,
        &admin,
        Method::PUT,
        &format!("/radarr/api/v3/qualityprofile/{pid}"),
        Some(p),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{saved}");
    assert_eq!(saved["formatItems"][0]["score"], -10000);

    // Quality definition sizes (MB/min) update in bulk.
    let (_, defs) = call(&app, Method::GET, "/radarr/api/v3/qualitydefinition", None).await;
    let mut d = defs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["quality"]["name"] == "Bluray-1080p")
        .unwrap()
        .clone();
    d["minSize"] = json!(25.0);
    d["maxSize"] = json!(400.0);
    let (s, _) = call_as(
        &app,
        &admin,
        Method::PUT,
        "/radarr/api/v3/qualitydefinition/update",
        Some(json!([d])),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (_, defs) = call(&app, Method::GET, "/radarr/api/v3/qualitydefinition", None).await;
    let d = defs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["quality"]["name"] == "Bluray-1080p")
        .unwrap();
    assert_eq!(d["minSize"], 25.0);
    assert_eq!(d["maxSize"], 400.0);

    // Sonarr names remuxes its own way.
    let (_, defs) = call(&app, Method::GET, "/sonarr/api/v3/qualitydefinition", None).await;
    assert!(
        defs.as_array()
            .unwrap()
            .iter()
            .any(|d| d["quality"]["name"] == "Bluray-1080p Remux")
    );
}
