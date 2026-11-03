//! Search → decision → grab against a wiremock Newznab indexer and a wiremock SABnzbd.

use std::sync::Arc;

use chrono::{Duration, Utc};
use reelix_api_types::library::{LibrarySearchKind, SearchRequest};
use reelix_api_types::pipeline::{GrabRequest, ReleaseQuery};
use reelix_api_types::providers::{IndexerSettingsDto, ProviderInput, ProviderKind};
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_library::{DynMetadata, LibraryService};
use reelix_pipeline::search::{SearchOptions, SearchService};
use reelix_pipeline::store::{blocklist, history, pending, tracked};
use reelix_providers::{ProviderOptions, ProviderService};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p@example.invalid" date="1777440000" subject="x [1/1] - &quot;x.rar&quot; yEnc (1/1)">
    <groups><group>alt.binaries.example</group></groups>
    <segments><segment bytes="768000" number="1">a@example.invalid</segment></segments>
  </file>
</nzb>"#;

const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <limits max="100" default="100" />
  <searching>
    <search available="yes" supportedParams="q" />
    <tv-search available="yes" supportedParams="q,tvdbid,season,ep" />
    <movie-search available="yes" supportedParams="q,imdbid" />
  </searching>
  <categories>
    <category id="2000" name="Movies"><subcat id="2040" name="HD" /></category>
    <category id="5000" name="TV"><subcat id="5040" name="HD" /></category>
  </categories>
</caps>"#;

struct Item {
    title: &'static str,
    guid: &'static str,
    size: u64,
    age: Duration,
    imdb: Option<&'static str>,
}

fn item(title: &'static str, guid: &'static str) -> Item {
    Item {
        title,
        guid,
        size: 4 << 30,
        age: Duration::days(3),
        imdb: None,
    }
}

fn rss(base: &str, items: &[Item]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><title>Idx</title>"#,
    );
    for i in items {
        let date = (Utc::now() - i.age).to_rfc2822();
        s.push_str(&format!(
            r#"<item><title>{t}</title><guid isPermaLink="false">{g}</guid><link>{base}/getnzb/{g}.nzb</link>
<pubDate>{date}</pubDate><enclosure url="{base}/getnzb/{g}.nzb" length="{size}" type="application/x-nzb" />
<newznab:attr name="size" value="{size}" />{imdb}</item>"#,
            t = i.title,
            g = i.guid,
            size = i.size,
            imdb = i
                .imdb
                .map(|v| format!(r#"<newznab:attr name="imdb" value="{v}" />"#))
                .unwrap_or_default(),
        ));
    }
    s.push_str("</channel></rss>");
    s
}

fn xml(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/rss+xml")
}

/// SABnzbd `addfile` reply with a fresh `nzo_id` per call (`SABnzbd_nzo_1`, `_2`, …).
#[derive(Default)]
struct NzoIds(std::sync::atomic::AtomicUsize);

impl wiremock::Respond for NzoIds {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        ResponseTemplate::new(200)
            .set_body_json(json!({"status": true, "nzo_ids": [format!("SABnzbd_nzo_{n}")]}))
    }
}

struct Env {
    db: Db,
    providers: Arc<ProviderService>,
    svc: SearchService,
    indexer: MockServer,
    sab: MockServer,
    indexer_id: i64,
    client_id: i64,
    _dir: tempfile::TempDir,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let mut opts = ProviderOptions::new(dir.path());
    opts.search_timeout = std::time::Duration::from_secs(5);
    let providers = ProviderService::new(
        db.clone(),
        SecretBox::from_key([3; 32]),
        events.clone(),
        reqwest::Client::new(),
        opts,
    )
    .await
    .unwrap();
    providers
        .set_indexer_settings(IndexerSettingsDto {
            allow_private_addresses: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let indexer = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(xml(CAPS.into()))
        .mount(&indexer)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("^/getnzb/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(NZB, "application/x-nzb"))
        .mount(&indexer)
        .await;
    let sab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("/api$"))
        .and(query_param("mode", "addfile"))
        .respond_with(NzoIds::default())
        .mount(&sab)
        .await;

    let idx = providers
        .create(
            ProviderKind::Indexer,
            &ProviderInput {
                implementation: "newznab".into(),
                name: "Idx".into(),
                enabled: true,
                priority: 25,
                tags: vec![],
                settings: json!({"base_url": indexer.uri(), "api_key": "k"}),
            },
        )
        .await
        .unwrap();
    let port = sab.address().port();
    let client = providers
        .create(
            ProviderKind::DownloadClient,
            &ProviderInput {
                implementation: "sabnzbd".into(),
                name: "SAB".into(),
                enabled: true,
                priority: 1,
                tags: vec![],
                settings: json!({"host": "127.0.0.1", "port": port, "url_base": "", "api_key": "sabkey"}),
            },
        )
        .await
        .unwrap();

    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    let svc = SearchService::new(
        db.clone(),
        events,
        providers.clone(),
        library,
        SearchOptions {
            throttle: std::time::Duration::ZERO,
            ..Default::default()
        },
    );
    Env {
        db,
        providers,
        svc,
        indexer,
        sab,
        indexer_id: idx.id,
        client_id: client.id,
        _dir: dir,
    }
}

async fn profile_id(db: &Db, name: &str) -> i64 {
    db.quality_profiles().list().await.unwrap();
    sqlx::query_scalar("SELECT id FROM quality_profiles WHERE name = ?")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn root(db: &Db, kind: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES (?, ?, 0, 0) RETURNING id",
    )
    .bind(format!("/media/{kind}"))
    .bind(kind)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

async fn add_movie(db: &Db, title: &str, year: i32, imdb: &str) -> i64 {
    let profile = profile_id(db, "HD-1080p").await;
    let root = root(db, "movie").await;
    sqlx::query_scalar(
        "INSERT INTO movies (tmdb_id, imdb_id, title, sort_title, clean_title, year, status, runtime, monitored, \
         minimum_availability, quality_profile_id, root_folder_id, path, digital_release, created_at, updated_at) \
         VALUES (603, ?, ?, ?, ?, ?, 'released', 120, 1, 'released', ?, ?, ?, ?, 0, 0) RETURNING id",
    )
    .bind(imdb)
    .bind(title)
    .bind(title.to_lowercase())
    .bind(title.to_lowercase())
    .bind(year)
    .bind(profile)
    .bind(root)
    .bind(format!("/media/movie/{title} ({year})"))
    .bind((Utc::now() - Duration::days(5)).date_naive().to_string())
    .fetch_one(db.pool())
    .await
    .unwrap()
}

/// A series with S01E01–E03 aired; returns (series id, episode ids).
async fn add_series(db: &Db) -> (i64, Vec<i64>) {
    let profile = profile_id(db, "HD-1080p").await;
    let root = root(db, "series").await;
    let sid: i64 = sqlx::query_scalar(
        "INSERT INTO series (tmdb_id, tvdb_id, title, sort_title, clean_title, year, runtime, quality_profile_id, \
         root_folder_id, path, created_at, updated_at) \
         VALUES (1399, 121361, 'The Show', 'show', 'show', 2020, 45, ?, ?, '/media/series/The Show', 0, 0) RETURNING id",
    )
    .bind(profile)
    .bind(root)
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO seasons (series_id, season_number, monitored) VALUES (?, 1, 1)")
        .bind(sid)
        .execute(db.pool())
        .await
        .unwrap();
    let mut ids = Vec::new();
    for n in 1..=3 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO episodes (series_id, season_number, episode_number, title, air_date, monitored, created_at, \
             updated_at) VALUES (?, 1, ?, ?, ?, 1, 0, 0) RETURNING id",
        )
        .bind(sid)
        .bind(n)
        .bind(format!("Episode {n}"))
        .bind(format!("2020-01-0{n}"))
        .fetch_one(db.pool())
        .await
        .unwrap();
        ids.push(id);
    }
    (sid, ids)
}

async fn mount_search(e: &Env, t: &str, items: &[Item]) {
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", t))
        .respond_with(xml(rss(&e.indexer.uri(), items)))
        .mount(&e.indexer)
        .await;
}

fn never() -> bool {
    false
}

async fn grabbed_titles(db: &Db) -> Vec<String> {
    tracked::active(db)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect()
}

#[tokio::test]
async fn movie_search_grabs_best_accepted_release() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    mount_search(
        &e,
        "movie",
        &[
            item("The.Matrix.1999.720p.BluRay.x264-GRP", "a"),
            item("The.Matrix.1999.1080p.BluRay.x264-GRP", "b"),
            item("Other.Movie.1999.1080p.BluRay.x264-GRP", "c"),
            item("The.Matrix.1999.2160p.UHD.BluRay.x265-GRP", "d"),
        ],
    )
    .await;
    let sum = e
        .svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids: vec![movie],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    assert_eq!(sum.grabbed, 1);
    let rows = tracked::active(&e.db).await.unwrap();
    assert_eq!(rows.len(), 1);
    let t = &rows[0];
    assert_eq!(t.title, "The.Matrix.1999.1080p.BluRay.x264-GRP");
    assert_eq!(t.movie_id, Some(movie));
    assert_eq!(t.download_id, "SABnzbd_nzo_1");
    assert_eq!(t.download_client_id, e.client_id);
    let (h, _) = history::list(&e.db, &history::HistoryFilter::default(), 0, 10)
        .await
        .unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].event, history::HistoryEvent::Grabbed);
    assert_eq!(h[0].movie_id, Some(movie));
    assert_eq!(h[0].download_client.as_deref(), Some("SAB"));
    // The NZB went to SABnzbd as an upload with the movie category.
    let reqs = e.sab.received_requests().await.unwrap();
    let add = reqs
        .iter()
        .find(|r| r.url.query().unwrap_or("").contains("mode=addfile"))
        .unwrap();
    assert!(add.url.query().unwrap().contains("cat=movies"), "{}", add.url);

    // Searching again: the queued release blocks another grab of the same quality.
    let sum = e
        .svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids: vec![movie],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    assert_eq!(sum.grabbed, 0);
}

#[tokio::test]
async fn blocklisted_release_is_skipped() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    blocklist::insert(
        &e.db,
        &blocklist::NewBlocklist {
            movie_id: Some(movie),
            source_title: "The.Matrix.1999.1080p.BluRay.x264-GRP".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    mount_search(
        &e,
        "movie",
        &[
            item("The.Matrix.1999.1080p.BluRay.x264-GRP", "b"),
            item("The.Matrix.1999.1080p.WEB-DL.H264-OTHER", "w"),
        ],
    )
    .await;
    e.svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids: vec![movie],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    assert_eq!(
        grabbed_titles(&e.db).await,
        vec!["The.Matrix.1999.1080p.WEB-DL.H264-OTHER"]
    );

    // Interactive search shows the blocklisted one as rejected.
    let rows = e
        .svc
        .interactive(&ReleaseQuery {
            movie_id: Some(movie),
            ..Default::default()
        })
        .await
        .unwrap();
    let b = rows.iter().find(|r| r.guid == "b").unwrap();
    assert!(!b.accepted);
    assert!(
        b.rejections.iter().any(|r| r.spec == "blocklist"),
        "{:?}",
        b.rejections
    );
}

#[tokio::test]
async fn interactive_search_ranks_and_grabs_from_cache() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    mount_search(
        &e,
        "movie",
        &[
            item("The.Matrix.1999.720p.BluRay.x264-GRP", "a"),
            item("The.Matrix.1999.1080p.BluRay.x264-GRP", "b"),
            item("Other.Movie.2001.1080p.BluRay.x264-GRP", "c"),
        ],
    )
    .await;
    let rows = e
        .svc
        .interactive(&ReleaseQuery {
            movie_id: Some(movie),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].guid, "b");
    assert!(rows[0].accepted);
    assert_eq!(rows[0].rank, 3);
    let other = rows.iter().find(|r| r.guid == "c").unwrap();
    assert!(
        other.rejections.iter().any(|r| r.spec == "mapping"),
        "{:?}",
        other.rejections
    );

    // A rejected release needs force.
    let req = GrabRequest {
        guid: "c".into(),
        indexer_id: e.indexer_id,
        force: false,
        movie_id: None,
        episode_ids: None,
        download_client_id: None,
        download_url: None,
        title: None,
    };
    assert!(e.svc.grab_request(&req).await.is_err());
    let r = e
        .svc
        .grab_request(&GrabRequest {
            guid: "b".into(),
            ..req.clone()
        })
        .await
        .unwrap();
    assert_eq!(r.download_client, "SAB");
    let t = tracked::get(&e.db, r.queue_id).await.unwrap().unwrap();
    assert_eq!(t.movie_id, Some(movie));

    // Raw grab (indexer search page): no media.
    let r = e
        .svc
        .grab_request(&GrabRequest {
            guid: "zzz".into(),
            download_url: Some(format!("{}/getnzb/zzz.nzb", e.indexer.uri())),
            title: Some("Some.Release.2020.1080p.WEB-DL-X".into()),
            ..req.clone()
        })
        .await
        .unwrap();
    let t = tracked::get(&e.db, r.queue_id).await.unwrap().unwrap();
    assert_eq!(t.movie_id, None);
    assert_eq!(t.title, "Some.Release.2020.1080p.WEB-DL-X");

    // A raw link to another host is refused (the indexer's credentials would go along).
    let err = e
        .svc
        .grab_request(&GrabRequest {
            guid: "elsewhere".into(),
            download_url: Some("http://elsewhere.invalid/getnzb/x.nzb".into()),
            title: Some("Some.Release.2020.1080p.WEB-DL-X".into()),
            ..req
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("must point at indexer"), "{err}");
}

#[tokio::test]
async fn raw_grab_of_private_url_is_rejected() {
    let e = env().await;
    // Guard on, indexer on a public host name (never contacted).
    e.providers
        .set_indexer_settings(IndexerSettingsDto::default())
        .await
        .unwrap();
    let idx = e
        .providers
        .create(
            ProviderKind::Indexer,
            &ProviderInput {
                implementation: "newznab".into(),
                name: "Public".into(),
                enabled: true,
                priority: 25,
                tags: vec![],
                settings: json!({"base_url": "https://indexer.example.com", "api_key": "k"}),
            },
        )
        .await
        .unwrap();
    for url in ["http://127.0.0.1/getnzb/x.nzb", "http://[::ffff:10.0.0.1]/x.nzb"] {
        let err = e
            .svc
            .grab_request(&GrabRequest {
                guid: "raw".into(),
                indexer_id: idx.id,
                force: false,
                movie_id: None,
                episode_ids: None,
                download_client_id: None,
                download_url: Some(url.into()),
                title: Some("Some.Release.2020.1080p.WEB-DL-X".into()),
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("blocked"), "{url}: {err}");
    }
    assert!(e.sab.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn season_search_prefers_full_season_pack() {
    let e = env().await;
    let (sid, eps) = add_series(&e.db).await;
    mount_search(
        &e,
        "tvsearch",
        &[
            item("The.Show.S01E01.1080p.WEB-DL.H264-GRP", "e1"),
            Item {
                size: 10 << 30,
                ..item("The.Show.S01.1080p.WEB-DL.H264-GRP", "pack")
            },
        ],
    )
    .await;
    let sum = e
        .svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Season,
                ids: vec![sid],
                season: Some(1),
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    assert_eq!(sum.grabbed, 1, "{sum:?}");
    let rows = tracked::active(&e.db).await.unwrap();
    assert_eq!(rows[0].title, "The.Show.S01.1080p.WEB-DL.H264-GRP");
    assert_eq!(rows[0].episode_ids, eps);
    // One grabbed history row per episode.
    let (h, total) = history::list(&e.db, &history::HistoryFilter::default(), 0, 10)
        .await
        .unwrap();
    assert_eq!(total, 3);
    assert!(
        h.iter()
            .all(|r| r.series_id == Some(sid) && r.episode_id.is_some())
    );
}

#[tokio::test]
async fn episode_search_maps_by_numbering() {
    let e = env().await;
    let (_sid, eps) = add_series(&e.db).await;
    mount_search(
        &e,
        "tvsearch",
        &[
            item("The.Show.S01E01.1080p.WEB-DL.H264-GRP", "e1"),
            item("The.Show.S01E02.1080p.WEB-DL.H264-GRP", "e2"),
            item("Another.Show.S01E02.1080p.WEB-DL.H264-GRP", "x"),
        ],
    )
    .await;
    e.svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Episode,
                ids: vec![eps[1]],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    let rows = tracked::active(&e.db).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "The.Show.S01E02.1080p.WEB-DL.H264-GRP");
    assert_eq!(rows[0].episode_ids, vec![eps[1]]);
}

#[tokio::test]
async fn delay_profile_holds_release_then_pending_grabs_it() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    let mut dp = e.db.delay_profiles().list().await.unwrap().remove(0);
    dp.usenet_delay = 60;
    dp.bypass_if_highest_quality = false;
    e.db.delay_profiles().update(dp.id, &dp).await.unwrap();
    mount_search(
        &e,
        "movie",
        &[Item {
            age: Duration::minutes(5),
            ..item("The.Matrix.1999.1080p.BluRay.x264-GRP", "b")
        }],
    )
    .await;
    let sum = e
        .svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids: vec![movie],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    assert_eq!((sum.grabbed, sum.pending), (0, 1));
    let p = pending::all(&e.db).await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].movie_id, Some(movie));
    assert!(p[0].release_at > Utc::now() + Duration::minutes(50));
    let listed = e.svc.list_pending().await.unwrap();
    assert_eq!(listed[0].media.as_ref().unwrap().title, "The Matrix");

    // Not due yet: nothing happens.
    assert_eq!(e.svc.process_pending().await.unwrap().grabbed, 0);
    assert_eq!(pending::all(&e.db).await.unwrap().len(), 1);

    // Make it due (publish date in the past) → grabbed.
    let mut rel = p[0].release.clone();
    rel.publish_date = Some(Utc::now() - Duration::hours(3));
    pending::delete(&e.db, p[0].id).await.unwrap();
    pending::insert(&e.db, Some(movie), None, &[], &rel, "delay", 0)
        .await
        .unwrap();
    assert_eq!(e.svc.process_pending().await.unwrap().grabbed, 1);
    assert!(pending::all(&e.db).await.unwrap().is_empty());
    assert_eq!(
        grabbed_titles(&e.db).await,
        vec!["The.Matrix.1999.1080p.BluRay.x264-GRP"]
    );
}

#[tokio::test]
async fn pending_grab_now() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    let mut dp = e.db.delay_profiles().list().await.unwrap().remove(0);
    dp.usenet_delay = 60;
    dp.bypass_if_highest_quality = false;
    e.db.delay_profiles().update(dp.id, &dp).await.unwrap();
    mount_search(
        &e,
        "movie",
        &[Item {
            age: Duration::minutes(1),
            ..item("The.Matrix.1999.1080p.BluRay.x264-GRP", "b")
        }],
    )
    .await;
    e.svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids: vec![movie],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    let id = pending::all(&e.db).await.unwrap()[0].id;
    let r = e.svc.grab_pending(id).await.unwrap();
    assert_eq!(r.download_id, "SABnzbd_nzo_1");
    assert!(pending::all(&e.db).await.unwrap().is_empty());
}

#[tokio::test]
async fn rss_sync_grabs_wanted_media_only() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    let (_sid, eps) = add_series(&e.db).await;
    mount_search(
        &e,
        "search",
        &[
            Item {
                imdb: Some("0133093"),
                ..item("The.Matrix.1999.1080p.BluRay.x264-GRP", "m")
            },
            item("The.Show.S01E03.1080p.WEB-DL.H264-GRP", "s3"),
            item("Unrelated.Show.S02E01.1080p.WEB-DL.H264-GRP", "u"),
            item("Some.Other.Film.2010.1080p.BluRay.x264-GRP", "o"),
        ],
    )
    .await;
    let sum = e.svc.rss_sync(true).await.unwrap().unwrap();
    assert_eq!(sum.grabbed, 2, "{sum:?}");
    let rows = tracked::active(&e.db).await.unwrap();
    assert!(rows.iter().any(|t| t.movie_id == Some(movie)));
    assert!(rows.iter().any(|t| t.episode_ids == vec![eps[2]]));

    // Interval not elapsed → skipped.
    assert!(e.svc.rss_sync(false).await.unwrap().is_none());
    let _ = &e.providers;
}

#[tokio::test]
async fn blocklist_api_helpers() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    for t in ["A", "B", "C"] {
        blocklist::insert(
            &e.db,
            &blocklist::NewBlocklist {
                movie_id: Some(movie),
                source_title: t.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let page = e.svc.list_blocklist(Some(1), Some(2)).await.unwrap();
    assert_eq!((page.items.len(), page.total), (2, 3));
    assert_eq!(page.items[0].media.as_ref().unwrap().title, "The Matrix");
    e.svc.delete_blocklist(page.items[0].id).await.unwrap();
    assert!(e.svc.delete_blocklist(page.items[0].id).await.is_err());
    assert_eq!(e.svc.clear_blocklist(&[]).await.unwrap(), 2);
}

#[tokio::test]
async fn decision_context_uses_existing_file_ids_and_alt_titles() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    // Existing Bluray-1080p file: meets the HD-1080p cutoff → nothing is an upgrade.
    sqlx::query(
        "INSERT INTO movie_files (movie_id, relative_path, size, quality, date_added, created_at, updated_at) \
         VALUES (?, 'The Matrix (1999).mkv', 1000, ?, 0, 0, 0)",
    )
    .bind(movie)
    .bind(serde_json::to_string(&reelix_parser::QualityModel::new(reelix_parser::Quality::Bluray1080p)).unwrap())
    .execute(e.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO alternative_titles (media_kind, media_id, title, clean_title) VALUES ('movie', ?, 'Matrix Alt', 'matrixalt')",
    )
    .bind(movie)
    .execute(e.db.pool())
    .await
    .unwrap();
    mount_search(
        &e,
        "movie",
        &[
            item("The.Matrix.1999.1080p.WEB-DL.H264-GRP", "web"),
            item("Matrix.Alt.1999.720p.BluRay.x264-GRP", "alt"),
            Item {
                imdb: Some("0000001"),
                ..item("The.Matrix.1999.1080p.BluRay.x264-WRONG", "wrong")
            },
        ],
    )
    .await;
    let rows = e
        .svc
        .interactive(&ReleaseQuery {
            movie_id: Some(movie),
            ..Default::default()
        })
        .await
        .unwrap();
    let by = |g: &str| rows.iter().find(|r| r.guid == g).unwrap();
    assert!(
        by("web").rejections.iter().any(|r| r.spec == "upgrade"),
        "{:?}",
        by("web").rejections
    );
    // Alternative title maps (only the upgrade check rejects it).
    assert!(
        by("alt").rejections.iter().all(|r| r.spec != "mapping"),
        "{:?}",
        by("alt").rejections
    );
    // Indexer id wins over a matching title.
    assert!(
        by("wrong").rejections.iter().any(|r| r.spec == "mapping"),
        "{:?}",
        by("wrong").rejections
    );
}

#[tokio::test]
async fn unaired_episode_is_temporarily_rejected() {
    let e = env().await;
    let (sid, _) = add_series(&e.db).await;
    let future = (Utc::now() + Duration::days(10)).date_naive().to_string();
    let ep: i64 = sqlx::query_scalar(
        "INSERT INTO episodes (series_id, season_number, episode_number, air_date, monitored, created_at, updated_at) \
         VALUES (?, 1, 4, ?, 1, 0, 0) RETURNING id",
    )
    .bind(sid)
    .bind(future)
    .fetch_one(e.db.pool())
    .await
    .unwrap();
    mount_search(
        &e,
        "tvsearch",
        &[item("The.Show.S01E04.1080p.WEB-DL.H264-GRP", "e4")],
    )
    .await;
    let rows = e
        .svc
        .interactive(&ReleaseQuery {
            episode_id: Some(ep),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].episodes[0].id, ep);
    let r = rows[0]
        .rejections
        .iter()
        .find(|r| r.spec == "availability")
        .unwrap();
    assert_eq!(r.kind, "temporary");
}

#[tokio::test]
async fn missing_search_walks_wanted_list() {
    let e = env().await;
    let movie = add_movie(&e.db, "The Matrix", 1999, "tt0133093").await;
    mount_search(&e, "movie", &[item("The.Matrix.1999.1080p.BluRay.x264-GRP", "b")]).await;
    let sum = e
        .svc
        .run_search(
            &SearchRequest {
                kind: LibrarySearchKind::Missing,
                ids: vec![],
                season: None,
                media: None,
            },
            &never,
        )
        .await
        .unwrap();
    assert_eq!((sum.searched, sum.grabbed), (1, 1));
    assert_eq!(tracked::active(&e.db).await.unwrap()[0].movie_id, Some(movie));
}
