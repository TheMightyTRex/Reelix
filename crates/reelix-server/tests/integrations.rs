//! M7 integrations: import list sync, exclusions API, notification bridge, health watch and
//! media server availability (fake lists/servers/metadata; no network).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState, HealthSource};
use reelix_api_types::library::*;
use reelix_api_types::providers::ProviderKind;
use reelix_api_types::{HealthCheck, HealthLevel};
use reelix_core::bus::EventBus;
use reelix_core::{DomainEvent, MediaKind};
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::JobSystem;
use reelix_library::{LibraryService, MetadataSource};
use reelix_lists::{ImportList, ListError, ListItem};
use reelix_mediaserver::{
    ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};
use reelix_metadata::*;
use reelix_notify::NotificationEvent;
use reelix_providers::ProviderRow;
use reelix_server::integrations::bridge::Bridge;
use reelix_server::integrations::health::HealthWatch;
use reelix_server::integrations::{IntegrationsService, Sources, SyncPayload};
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------- fakes

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn hit(kind: MediaKind, id: u32, title: &str, year: i32) -> SearchHit {
    SearchHit {
        kind,
        tmdb_id: Some(id),
        tvdb_id: None,
        imdb_id: None,
        title: title.into(),
        original_title: None,
        year: Some(year),
        release_date: None,
        overview: None,
        poster: None,
        backdrop: None,
        genre_ids: vec![],
        original_language: Some("en".into()),
        vote_average: 7.0,
        vote_count: 10,
        popularity: 1.0,
        adult: false,
    }
}

fn movie(id: u32, title: &str, year: i32) -> MovieMetadata {
    MovieMetadata {
        tmdb_id: id,
        imdb_id: Some(format!("tt{id:07}")),
        title: title.into(),
        original_title: title.into(),
        original_language: Some("en".into()),
        sort_title: title.to_lowercase(),
        overview: Some("Overview".into()),
        tagline: None,
        year: Some(year),
        release_date: Some(d(&format!("{year}-03-01"))),
        release_dates: ReleaseDates {
            cinema: Some(d(&format!("{year}-03-01"))),
            digital: Some(d(&format!("{year}-06-01"))),
            physical: None,
            all: vec![],
        },
        certification: None,
        status: reelix_metadata::MovieStatus::Released,
        runtime_minutes: Some(100),
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

fn show(id: u32, title: &str) -> SeriesMetadata {
    let ep = |n: u32| EpisodeMetadata {
        season_number: 1,
        episode_number: n,
        absolute_number: None,
        title: Some(format!("Episode {n}")),
        overview: None,
        air_date: Some(d("2020-01-01")),
        runtime_minutes: Some(45),
        still: None,
        tmdb_id: None,
        tvdb_id: None,
        finale: None,
        vote_average: None,
    };
    SeriesMetadata {
        tmdb_id: Some(id),
        tvdb_id: Some(id + 1000),
        imdb_id: None,
        title: title.into(),
        original_title: None,
        sort_title: title.to_lowercase(),
        overview: None,
        year: Some(2020),
        first_air_date: Some(d("2020-01-01")),
        last_air_date: None,
        next_air_date: None,
        status: reelix_metadata::SeriesStatus::Continuing,
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
        seasons: vec![SeasonMetadata {
            season_number: 1,
            name: None,
            overview: None,
            air_date: Some(d("2020-01-01")),
            poster: None,
            episode_count: 2,
            episodes: vec![ep(1), ep(2)],
            tmdb_id: None,
            tvdb_id: None,
        }],
        recommendations: vec![],
        similar: vec![],
    }
}

#[derive(Default)]
struct FakeMeta {
    movies: HashMap<u32, MovieMetadata>,
    series: HashMap<u32, SeriesMetadata>,
}

#[async_trait]
impl MetadataSource for FakeMeta {
    async fn movie(&self, id: u32) -> Result<MovieMetadata, MetaError> {
        self.movies.get(&id).cloned().ok_or(MetaError::NotFound)
    }
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        match id {
            SeriesExternalId::Tmdb(t) => self.series.get(&t).cloned().ok_or(MetaError::NotFound),
            _ => Err(MetaError::NotFound),
        }
    }
    async fn collection(&self, _id: u32) -> Result<CollectionMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn search(
        &self,
        q: &str,
        kind: MediaKind,
        _year: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError> {
        Ok(match kind {
            MediaKind::Movie => self
                .movies
                .values()
                .filter(|m| m.title.eq_ignore_ascii_case(q))
                .map(|m| hit(kind, m.tmdb_id, &m.title, m.year.unwrap_or(0)))
                .collect(),
            MediaKind::Series => vec![],
        })
    }
    async fn find(
        &self,
        source: ExternalSource,
        id: &str,
        kind: MediaKind,
    ) -> Result<Vec<SearchHit>, MetaError> {
        Ok(match (source, kind) {
            (ExternalSource::Imdb, MediaKind::Movie) => self
                .movies
                .values()
                .filter(|m| m.imdb_id.as_deref() == Some(id))
                .map(|m| hit(kind, m.tmdb_id, &m.title, 2000))
                .collect(),
            (ExternalSource::Tvdb, MediaKind::Series) => self
                .series
                .values()
                .filter(|s| s.tvdb_id.map(|t| t.to_string()).as_deref() == Some(id))
                .map(|s| hit(kind, s.tmdb_id.unwrap(), &s.title, 2020))
                .collect(),
            _ => vec![],
        })
    }
}

struct FakeList(Vec<ListItem>);

#[async_trait]
impl ImportList for FakeList {
    fn implementation(&self) -> &'static str {
        "fake"
    }
    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        Ok(self.0.clone())
    }
}

struct BrokenList;

#[async_trait]
impl ImportList for BrokenList {
    fn implementation(&self) -> &'static str {
        "broken"
    }
    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        Err(ListError::Unauthorized)
    }
}

struct FakeServer(Vec<MsItem>);

#[async_trait]
impl MediaServer for FakeServer {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Jellyfin
    }
    async fn test(&self) -> Result<ServerInfo, MsError> {
        Err(MsError::Unsupported("fake"))
    }
    async fn authenticate(&self, _u: &str, _p: &str) -> Result<ExternalUser, MsError> {
        Err(MsError::Unsupported("fake"))
    }
    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        Ok(vec![])
    }
    async fn refresh_library(&self, _paths: &[PathBuf]) -> Result<(), MsError> {
        Ok(())
    }
    async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
        Ok(self.0.clone())
    }
    async fn watch_history(&self, _u: &str) -> Result<Vec<WatchedItem>, MsError> {
        Ok(vec![])
    }
}

fn row(id: i64, kind: ProviderKind, name: &str, settings: Value, tags: Vec<i64>) -> ProviderRow {
    ProviderRow {
        id,
        kind,
        implementation: "fake".into(),
        name: name.into(),
        enabled: true,
        priority: 25,
        tags,
        settings: settings.as_object().cloned().unwrap_or_default(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

type Lists = Arc<Mutex<Vec<(ProviderRow, Arc<dyn ImportList>)>>>;
type Servers = Arc<Mutex<Vec<(ProviderRow, Arc<dyn MediaServer>)>>>;

struct Env {
    db: Db,
    events: EventBus,
    library: Arc<LibraryService>,
    svc: Arc<IntegrationsService>,
    lists: Lists,
    servers: Servers,
    sent: Arc<Mutex<Vec<NotificationEvent>>>,
    movie_root: i64,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

async fn env() -> Env {
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let mut meta = FakeMeta::default();
    for (id, t, y) in [
        (603, "The Matrix", 1999),
        (604, "Heat", 1995),
        (605, "Alien", 1979),
        (606, "Up", 2009),
    ] {
        meta.movies.insert(id, movie(id, t, y));
    }
    meta.series.insert(1399, show(1399, "The Show"));
    meta.series.insert(1400, show(1400, "Other Show"));
    let meta: Arc<dyn MetadataSource> = Arc::new(meta);
    let library = Arc::new(LibraryService::new(db.clone(), events.clone(), meta.clone()));
    let mut mm = library.media_management().await.unwrap();
    mm.analyze_video_files = false;
    library.set_media_management(mm).await.unwrap();
    let movies = tempfile::tempdir().unwrap();
    let tv = tempfile::tempdir().unwrap();
    let movie_root = library
        .add_root_folder(movies.path().to_str().unwrap(), MediaKind::Movie)
        .await
        .unwrap()
        .id;
    library
        .add_root_folder(tv.path().to_str().unwrap(), MediaKind::Series)
        .await
        .unwrap();
    let lists: Lists = Arc::default();
    let servers: Servers = Arc::default();
    let sent: Arc<Mutex<Vec<NotificationEvent>>> = Arc::default();
    let (l, s, n) = (lists.clone(), servers.clone(), sent.clone());
    let sources = Sources {
        import_lists: Arc::new(move || l.lock().unwrap().clone()),
        media_servers: Arc::new(move || s.lock().unwrap().clone()),
        notify: Arc::new(move |ev| {
            n.lock().unwrap().push(ev);
            Box::pin(async {})
        }),
    };
    let svc = IntegrationsService::new(db.clone(), events.clone(), library.clone(), meta, sources);
    Env {
        db,
        events,
        library,
        svc,
        lists,
        servers,
        sent,
        movie_root,
        _dirs: (movies, tv),
    }
}

fn item(kind: MediaKind, title: &str) -> ListItem {
    ListItem::new(kind, title)
}

// ---------------------------------------------------------------- import lists

#[tokio::test]
async fn import_list_sync_adds_with_options_and_skips_exclusions() {
    let e = env().await;
    // 604 is excluded; 603 is already in the library.
    e.library
        .add_movie(AddMovieRequest {
            tmdb_id: 603,
            quality_profile_id: 1,
            root_folder_id: e.movie_root,
            monitor: MovieMonitor::MovieOnly,
            minimum_availability: MinimumAvailability::Released,
            tags: vec![],
            search_on_add: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();
    reelix_server::integrations::lists::add_exclusion(
        &e.db,
        &reelix_api_types::integrations::CreateExclusionRequest {
            kind: MediaKind::Movie,
            tmdb_id: 604,
            title: "Heat".into(),
            year: Some(1995),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let items = vec![
        ListItem {
            tmdb_id: Some(603),
            ..item(MediaKind::Movie, "The Matrix")
        },
        ListItem {
            tmdb_id: Some(604),
            ..item(MediaKind::Movie, "Heat")
        },
        // Resolved through IMDb.
        ListItem {
            imdb_id: Some("tt0000605".into()),
            ..item(MediaKind::Movie, "Alien")
        },
        // Resolved through a title + year search.
        ListItem {
            year: Some(2009),
            ..item(MediaKind::Movie, "Up")
        },
        // Unknown to TMDB.
        item(MediaKind::Movie, "Nope"),
        // Series by TVDB id.
        ListItem {
            tvdb_id: Some(2399),
            ..item(MediaKind::Series, "The Show")
        },
    ];
    let settings = json!({
        "monitor": "none",
        "series_monitor": "pilot",
        "minimum_availability": "announced",
        "series_type": "anime",
        "season_folder": false,
        "search_on_add": false,
        "sync_interval_hours": 6,
    });
    e.lists.lock().unwrap().push((
        row(10, ProviderKind::ImportList, "My list", settings, vec![7]),
        Arc::new(FakeList(items)),
    ));
    e.lists.lock().unwrap().push((
        row(11, ProviderKind::ImportList, "Broken", json!({}), vec![]),
        Arc::new(BrokenList),
    ));

    let out = e.svc.sync_import_lists(&SyncPayload::default()).await.unwrap();
    assert_eq!(out.len(), 2);
    let o = out.iter().find(|o| o.list_id == 10).unwrap();
    assert_eq!(
        (o.found, o.added, o.excluded, o.existing, o.unresolved),
        (6, 3, 1, 1, 1)
    );
    assert!(o.error.is_none());
    assert!(out.iter().find(|o| o.list_id == 11).unwrap().error.is_some());

    let alien = e.library.movie_by_tmdb(605).await.unwrap().unwrap();
    assert!(!alien.monitored, "monitor: none");
    assert_eq!(alien.minimum_availability, MinimumAvailability::Announced);
    assert_eq!(alien.tags, vec![7]);
    assert!(e.library.movie_by_tmdb(606).await.unwrap().is_some());
    assert!(e.library.movie_by_tmdb(604).await.unwrap().is_none());
    let show = e.library.series_by_tmdb(1399).await.unwrap().unwrap();
    assert_eq!(show.series_type, SeriesType::Anime);
    assert!(!show.season_folder);
    let eps = e.library.episodes(show.id, None).await.unwrap();
    assert_eq!(eps.iter().filter(|e| e.monitored).count(), 1, "pilot only");

    // Status recorded; not due again until the interval passes, unless forced.
    let status = reelix_server::integrations::lists::list_status(&e.db)
        .await
        .unwrap();
    assert_eq!(status.len(), 2);
    assert_eq!(status.iter().find(|s| s.list_id == 10).unwrap().items_added, 3);
    assert!(
        e.svc
            .sync_import_lists(&SyncPayload::default())
            .await
            .unwrap()
            .is_empty()
    );
    let forced = e
        .svc
        .sync_import_lists(&SyncPayload {
            list_ids: vec![10],
            force: true,
        })
        .await
        .unwrap();
    assert_eq!(forced.len(), 1);
    assert_eq!(
        (forced[0].added, forced[0].existing, forced[0].excluded),
        (0, 4, 1)
    );
}

#[tokio::test]
async fn auto_add_disabled_only_fetches() {
    let e = env().await;
    e.lists.lock().unwrap().push((
        row(
            1,
            ProviderKind::ImportList,
            "L",
            json!({"enabled_auto_add": false}),
            vec![],
        ),
        Arc::new(FakeList(vec![ListItem {
            tmdb_id: Some(605),
            ..item(MediaKind::Movie, "Alien")
        }])),
    ));
    let out = e.svc.sync_import_lists(&SyncPayload::default()).await.unwrap();
    assert_eq!((out[0].found, out[0].added), (1, 0));
    assert!(e.library.movie_by_tmdb(605).await.unwrap().is_none());
}

// ---------------------------------------------------------------- API

async fn call(
    router: &axum::Router,
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
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn exclusions_sync_and_availability_api() {
    let e = env().await;
    let dir = tempfile::tempdir().unwrap();
    let system = JobSystem::builder(e.db.clone(), e.events.clone()).build();
    let state = AppState::new(
        e.db.clone(),
        e.events.clone(),
        system.queue(),
        SecretBox::from_key([7u8; 32]),
        ApiConfig {
            url_base: String::new(),
            data_dir: dir.path().to_path_buf(),
            database: "sqlite".into(),
            start_time: Utc::now(),
        },
    )
    .with_extension(Arc::new(reelix_api::Integrations(e.svc.clone())));
    let router = reelix_api::router(state);
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/setup")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": "correct-horse-battery"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let body = json!({"kind": "movie", "tmdb_id": 604, "title": "Heat", "year": 1995});
    let (s, created) = call(
        &router,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &cookie,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(created["kind"], "movie");
    let (s, _) = call(
        &router,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &cookie,
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &router,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &cookie,
        Some(json!({"kind": "series", "tmdb_id": 0, "title": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    call(
        &router,
        Method::POST,
        "/api/v1/importlists/exclusions",
        &cookie,
        Some(json!({"kind": "series", "tmdb_id": 1399, "title": "The Show"})),
    )
    .await;
    let (s, list) = call(
        &router,
        Method::GET,
        "/api/v1/importlists/exclusions",
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2);

    let id = created["id"].as_i64().unwrap();
    let (s, _) = call(
        &router,
        Method::DELETE,
        &format!("/api/v1/importlists/exclusions/{id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &router,
        Method::DELETE,
        &format!("/api/v1/importlists/exclusions/{id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &router,
        Method::DELETE,
        "/api/v1/importlists/exclusions",
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = call(
        &router,
        Method::GET,
        "/api/v1/importlists/exclusions",
        &cookie,
        None,
    )
    .await;
    assert_eq!(list, json!([]));

    let (s, job) = call(
        &router,
        Method::POST,
        "/api/v1/importlists/sync",
        &cookie,
        Some(json!({"list_ids": [3]})),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let job_id = job["job_id"].as_i64().unwrap();
    let stored = e.db.jobs().get(job_id).await.unwrap().unwrap();
    assert_eq!(stored.kind, "import_list_sync");

    let (s, status) = call(&router, Method::GET, "/api/v1/importlists/status", &cookie, None).await;
    assert_eq!((s, status), (StatusCode::OK, json!([])));
    let (s, avail) = call(
        &router,
        Method::GET,
        "/api/v1/mediaservers/availability",
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(avail["movies_total"], 0);

    // Unauthenticated.
    let (s, _) = call(
        &router,
        Method::GET,
        "/api/v1/importlists/exclusions",
        "x=y",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------- media server availability

#[tokio::test]
async fn media_server_availability_matches_library_items() {
    let e = env().await;
    let m = e
        .library
        .add_movie(AddMovieRequest {
            tmdb_id: 603,
            quality_profile_id: 1,
            root_folder_id: e.movie_root,
            monitor: MovieMonitor::MovieOnly,
            minimum_availability: MinimumAvailability::Released,
            tags: vec![],
            search_on_add: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();
    let ms = |kind, id: &str, tmdb: Option<i64>, tvdb: Option<i64>, imdb: Option<&str>| MsItem {
        kind,
        id: id.into(),
        title: "x".into(),
        year: None,
        tmdb_id: tmdb,
        tvdb_id: tvdb,
        imdb_id: imdb.map(Into::into),
        path: None,
        added_at: None,
    };
    let lib = e.library.list_movies(&MovieListQuery::default()).await.unwrap();
    assert_eq!(lib.total, 1);
    e.servers.lock().unwrap().push((
        row(5, ProviderKind::MediaServer, "Jelly", json!({}), vec![]),
        Arc::new(FakeServer(vec![
            ms(MediaKind::Movie, "a", None, None, Some("tt0000603")),
            ms(MediaKind::Movie, "b", Some(999), None, None),
            ms(MediaKind::Series, "c", None, Some(1), None),
        ])),
    ));
    e.svc.sync_media_servers().await.unwrap();
    assert!(
        reelix_server::integrations::availability::is_available(&e.db, MediaKind::Movie, m.id)
            .await
            .unwrap()
    );
    let names = vec![(5, "Jelly".to_owned())];
    let sum = reelix_server::integrations::availability::summary(&e.db, &names)
        .await
        .unwrap();
    assert_eq!((sum.movies_total, sum.movies_available), (1, 1));
    assert_eq!(sum.servers[0].items_total, 3);
    assert_eq!(sum.servers[0].movies_matched, 1);

    // Server removed → its rows are forgotten.
    e.servers.lock().unwrap().clear();
    e.svc.sync_media_servers().await.unwrap();
    let sum = reelix_server::integrations::availability::summary(&e.db, &[])
        .await
        .unwrap();
    assert_eq!(sum.movies_available, 0);
}

// ---------------------------------------------------------------- notification bridge

fn changed(entity: &str, action: &str, id: i64) -> DomainEvent {
    DomainEvent::EntityChanged {
        entity: entity.into(),
        action: action.into(),
        id,
        user_id: None,
    }
}

async fn insert_movie_file(db: &Db, movie_id: i64, rel: &str) -> i64 {
    let q = serde_json::to_string(&reelix_parser::parse("Movie.2000.1080p.BluRay.x264-GRP").quality).unwrap();
    sqlx::query_scalar(
        "INSERT INTO movie_files (movie_id, relative_path, size, quality, release_group, date_added, created_at, updated_at)
         VALUES (?, ?, 1000, ?, 'GRP', 0, 0, 0) RETURNING id",
    )
    .bind(movie_id)
    .bind(rel)
    .bind(q)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn bridge_maps_library_events_to_notifications() {
    let e = env().await;
    let mut bridge = Bridge::new(e.db.clone()).with_grace(Duration::ZERO);
    bridge.reload_all().await.unwrap();
    assert!(bridge.cache().is_empty());

    let m = e
        .library
        .add_movie(AddMovieRequest {
            tmdb_id: 603,
            quality_profile_id: 1,
            root_folder_id: e.movie_root,
            monitor: MovieMonitor::MovieOnly,
            minimum_availability: MinimumAvailability::Released,
            tags: vec![],
            search_on_add: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();
    let out = bridge.handle(&changed("movie", "created", m.id)).await;
    match &out[..] {
        [NotificationEvent::MediaAdded { media }] => {
            assert_eq!(media.title, "The Matrix");
            assert_eq!(media.tmdb_id, Some(603));
            assert_eq!(media.id, Some(m.id));
        }
        other => panic!("{other:?}"),
    }

    // A file appears (scan), then is deleted manually → FileDeleted.
    let f1 = insert_movie_file(&e.db, m.id, "The Matrix (1999).mkv").await;
    assert!(
        bridge
            .handle(&changed("movie_file", "created", f1))
            .await
            .is_empty()
    );
    sqlx::query("DELETE FROM movie_files WHERE id = ?")
        .bind(f1)
        .execute(e.db.pool())
        .await
        .unwrap();
    assert!(
        bridge
            .handle(&changed("movie_file", "deleted", f1))
            .await
            .is_empty()
    );
    match &bridge.flush(tokio::time::Instant::now())[..] {
        [NotificationEvent::FileDeleted { media, file, .. }] => {
            assert_eq!(media.title, "The Matrix");
            assert!(file.path.ends_with("The Matrix (1999).mkv"));
            assert_eq!(file.quality.as_deref(), Some("Bluray-1080p"));
            assert_eq!(file.release_group.as_deref(), Some("GRP"));
        }
        other => panic!("{other:?}"),
    }

    // Upgrade: delete + new file for the same movie → no FileDeleted (pipeline sends Import).
    let f2 = insert_movie_file(&e.db, m.id, "old.mkv").await;
    bridge.handle(&changed("movie", "updated", m.id)).await;
    sqlx::query("DELETE FROM movie_files WHERE id = ?")
        .bind(f2)
        .execute(e.db.pool())
        .await
        .unwrap();
    bridge.handle(&changed("movie_file", "deleted", f2)).await;
    let f3 = insert_movie_file(&e.db, m.id, "new.mkv").await;
    bridge.handle(&changed("movie_file", "created", f3)).await;
    assert!(bridge.flush(tokio::time::Instant::now()).is_empty());

    // Rename.
    sqlx::query("UPDATE movie_files SET relative_path = 'renamed.mkv' WHERE id = ?")
        .bind(f3)
        .execute(e.db.pool())
        .await
        .unwrap();
    match &bridge.handle(&changed("movie", "renamed", m.id)).await[..] {
        [NotificationEvent::Rename { files, .. }] => {
            assert!(files[0].previous_path.ends_with("new.mkv"));
            assert!(files[0].new_path.ends_with("renamed.mkv"));
        }
        other => panic!("{other:?}"),
    }
    // Renamed with nothing changed → silent.
    assert!(bridge.handle(&changed("movie", "renamed", m.id)).await.is_empty());

    // Delete the movie (files kept on disk: folder missing here, but no files left either way).
    e.library.delete_movie(m.id, false).await.unwrap();
    match &bridge.handle(&changed("movie", "deleted", m.id)).await[..] {
        [NotificationEvent::MediaDeleted { media, .. }] => assert_eq!(media.title, "The Matrix"),
        other => panic!("{other:?}"),
    }
    // Unrelated entities are ignored.
    assert!(bridge.handle(&changed("queue", "updated", 1)).await.is_empty());
    assert!(
        bridge
            .handle(&changed("movie", "deleted", 12345))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn bridge_runs_on_the_event_bus() {
    let e = env().await;
    let bridge = Bridge::new(e.db.clone());
    let rx = e.events.subscribe();
    let sent = e.sent.clone();
    let task = tokio::spawn(bridge.run(rx, move |ev| {
        sent.lock().unwrap().push(ev);
        async {}
    }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    e.library
        .add_series(AddSeriesRequest {
            tmdb_id: Some(1400),
            tvdb_id: None,
            quality_profile_id: 1,
            root_folder_id: 2,
            monitor: SeriesMonitor::All,
            seasons: None,
            series_type: SeriesType::Standard,
            season_folder: true,
            monitor_new_items: NewItemMonitor::All,
            tags: vec![],
            search_on_add: false,
            search_cutoff_unmet: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if !e.sent.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sent = e.sent.lock().unwrap().clone();
    assert!(
        matches!(&sent[..], [NotificationEvent::MediaAdded { media }] if media.title == "Other Show" && media.tvdb_id == Some(2400)),
        "{sent:?}"
    );
    task.abort();
}

// ---------------------------------------------------------------- health watch

fn hc(source: &str, level: HealthLevel, msg: &str) -> HealthCheck {
    HealthCheck {
        source: source.into(),
        level,
        message: msg.into(),
    }
}

#[test]
fn health_watch_transitions() {
    let w = HealthWatch::default();
    let out = w.update(vec![
        hc("indexers", HealthLevel::Warning, "No indexers"),
        hc("database", HealthLevel::Ok, "fine"),
    ]);
    assert!(matches!(&out[..], [NotificationEvent::HealthIssue(s)] if s.message == "No indexers"));
    // Unchanged → nothing.
    assert!(
        w.update(vec![hc("indexers", HealthLevel::Warning, "No indexers")])
            .is_empty()
    );
    // Escalated → issue again; a new one appears.
    let out = w.update(vec![
        hc("indexers", HealthLevel::Error, "No indexers"),
        hc("clients", HealthLevel::Warning, "Client down"),
    ]);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|e| matches!(e, NotificationEvent::HealthIssue(_))));
    // Cleared → restored.
    let out = w.update(vec![hc("clients", HealthLevel::Warning, "Client down")]);
    assert!(matches!(&out[..], [NotificationEvent::HealthRestored(s)] if s.source == "indexers"));
}

struct StaticHealth(Mutex<Vec<HealthCheck>>);

impl HealthSource for StaticHealth {
    fn checks(&self) -> Vec<HealthCheck> {
        self.0.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn health_watch_job_notifies_through_sources() {
    let e = env().await;
    let src = Arc::new(StaticHealth(Mutex::new(vec![hc(
        "x",
        HealthLevel::Error,
        "broken",
    )])));
    e.svc.set_health_sources(vec![src.clone()]);
    assert_eq!(e.svc.watch_health().await, 1);
    src.0.lock().unwrap().clear();
    assert_eq!(e.svc.watch_health().await, 1);
    let sent = e.sent.lock().unwrap().clone();
    assert!(matches!(sent[0], NotificationEvent::HealthIssue(_)));
    assert!(matches!(sent[1], NotificationEvent::HealthRestored(_)));
}
