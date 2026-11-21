//! The subtitle service end to end against recorded sample answers served by wiremock: no live
//! calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::subtitles::{
    OpenSubtitlesSettings, SubtitleLanguage, SubtitleMediaKind, SubtitleSettings,
};
use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_subtitles::matching::MatchLevel;
use reelix_subtitles::naming::Variant;
use reelix_subtitles::{Error, Options, SubtitleService};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEARCH: &str = include_str!("fixtures/opensubtitles-search.json");
const LOGIN: &str = include_str!("fixtures/opensubtitles-login.json");
const DOWNLOAD: &str = include_str!("fixtures/opensubtitles-download.json");
const STREMIO: &str = include_str!("fixtures/stremio-subtitles.json");

/// A 100-minute film's subtitle.
const FILM_SRT: &str =
    "1\n00:00:05,000 --> 00:00:07,000\nHello.\n\n2\n01:39:00,000 --> 01:39:30,000\nGoodbye.\n";
/// A subtitle for something three hours long.
const LONG_SRT: &str =
    "1\n00:00:05,000 --> 00:00:07,000\nHello.\n\n2\n03:00:00,000 --> 03:00:02,000\nToo long.\n";

struct Setup {
    db: Db,
    svc: Arc<SubtitleService>,
    server: MockServer,
    dir: tempfile::TempDir,
    file_id: i64,
}

impl Setup {
    fn video(&self) -> PathBuf {
        self.dir.path().join("Heat (1995)").join("Heat (1995).mkv")
    }

    fn sidecar(&self, suffix: &str) -> PathBuf {
        self.dir
            .path()
            .join("Heat (1995)")
            .join(format!("Heat (1995).{suffix}"))
    }

    async fn settings(&self, s: SubtitleSettings) {
        self.svc.set_settings(s).await.unwrap();
    }
}

async fn setup() -> Setup {
    let db = Db::in_memory().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Heat (1995)");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Heat (1995).mkv"), vec![7u8; 200_000]).unwrap();
    let movie: i64 = sqlx::query_scalar(
        "INSERT INTO movies (tmdb_id, imdb_id, title, sort_title, clean_title, year, quality_profile_id,
            root_folder_id, path, created_at, updated_at)
         VALUES (949, 'tt0113277', 'Heat', 'heat', 'heat', 1995, 1, 1, ?, 0, 0) RETURNING id",
    )
    .bind(folder.to_string_lossy().into_owned())
    .fetch_one(db.pool())
    .await
    .unwrap();
    let file_id: i64 = sqlx::query_scalar(
        "INSERT INTO movie_files (movie_id, relative_path, size, quality, scene_name, media_info,
            date_added, created_at, updated_at)
         VALUES (?, 'Heat (1995).mkv', 200000, '{}', 'Heat.1995.1080p.BluRay.x264-GRP',
            '{\"container\":\"matroska\",\"duration_secs\":6000.0}', 0, 0, 0) RETURNING id",
    )
    .bind(movie)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let server = MockServer::start().await;
    let svc = Arc::new(SubtitleService::new(
        db.clone(),
        EventBus::default(),
        SecretBox::from_key([7; 32]),
        reqwest::Client::new(),
        Options {
            opensubtitles_base: format!("{}/api/v1/", server.uri()).parse().unwrap(),
            gap: Duration::from_millis(1),
            ..Default::default()
        },
    ));
    Setup {
        db,
        svc,
        server,
        dir,
        file_id,
    }
}

fn english(normal: bool, sdh: bool, forced: bool) -> SubtitleLanguage {
    SubtitleLanguage {
        code: "en".into(),
        normal,
        sdh,
        forced,
    }
}

fn with_opensubtitles(languages: Vec<SubtitleLanguage>) -> SubtitleSettings {
    SubtitleSettings {
        enabled: true,
        languages,
        opensubtitles: OpenSubtitlesSettings {
            enabled: true,
            api_key: "test-key".into(),
            username: "someone".into(),
            password: "not-a-real-password".into(),
        },
        ..Default::default()
    }
}

async fn mount_opensubtitles(t: &Setup, files: &[(u64, &str)]) {
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .and(header("Api-Key", "test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(LOGIN)
                .insert_header("content-type", "application/json"),
        )
        .mount(&t.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .and(header("Api-Key", "test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SEARCH)
                .insert_header("content-type", "application/json"),
        )
        .mount(&t.server)
        .await;
    for (id, body) in files {
        let link = format!("{}/files/{id}.srt", t.server.uri());
        Mock::given(method("POST"))
            .and(path("/api/v1/download"))
            .and(header("Authorization", "Bearer test-token-not-real"))
            .and(body_partial_json(serde_json::json!({ "file_id": id })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(DOWNLOAD.replace("{{LINK}}", &link))
                    .insert_header("content-type", "application/json"),
            )
            .mount(&t.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/files/{id}.srt")))
            .respond_with(ResponseTemplate::new(200).set_body_string(*body))
            .mount(&t.server)
            .await;
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[tokio::test]
async fn off_until_switched_on() {
    let t = setup().await;
    // nothing saved: off by default
    assert!(!t.svc.settings().await.unwrap().enabled);
    assert_eq!(t.svc.run_due().await.unwrap(), 0);
    assert!(matches!(
        t.svc.search_file(SubtitleMediaKind::Movie, t.file_id).await,
        Err(Error::Off)
    ));
    assert!(matches!(
        t.svc.queue_search(SubtitleMediaKind::Movie, t.file_id).await,
        Err(Error::Off)
    ));
    let created = DomainEvent::EntityChanged {
        entity: "movie_file".into(),
        action: "created".into(),
        id: t.file_id,
        user_id: None,
    };
    assert!(!t.svc.on_event(&created).await.unwrap());
    // set up but switched off: still nothing
    t.settings(SubtitleSettings {
        enabled: false,
        ..with_opensubtitles(vec![english(true, false, false)])
    })
    .await;
    assert_eq!(t.svc.run_due().await.unwrap(), 0);
    assert!(!t.svc.on_event(&created).await.unwrap());
    assert!(t.server.received_requests().await.unwrap().is_empty());
    assert!(!t.sidecar("en.srt").exists());
}

#[tokio::test]
async fn fetches_normal_sdh_and_forced_beside_the_video() {
    let t = setup().await;
    mount_opensubtitles(&t, &[(7001, FILM_SRT), (7002, FILM_SRT), (7003, FILM_SRT)]).await;
    t.settings(with_opensubtitles(vec![english(true, true, true)]))
        .await;
    let r = t
        .svc
        .search_file(SubtitleMediaKind::Movie, t.file_id)
        .await
        .unwrap();
    assert_eq!(
        r.saved,
        vec![
            ("en".into(), Variant::Normal, MatchLevel::Hash),
            ("en".into(), Variant::Sdh, MatchLevel::Id),
            ("en".into(), Variant::Forced, MatchLevel::Id),
        ]
    );
    for suffix in ["en.srt", "en.sdh.srt", "en.forced.srt"] {
        assert_eq!(read(&t.sidecar(suffix)), FILM_SRT, "{suffix}");
    }
    let rec = t.svc.records(SubtitleMediaKind::Movie, t.file_id).await.unwrap();
    assert_eq!(rec.len(), 3);
    let normal = rec.iter().find(|r| r.variant == "normal").unwrap();
    assert_eq!(normal.match_level, "hash");
    assert_eq!(normal.source, "opensubtitles");
    assert_eq!(normal.path, t.sidecar("en.srt").to_string_lossy());
    // the search sent the file's hash and the film's ids
    let reqs = t.server.received_requests().await.unwrap();
    let search = reqs.iter().find(|r| r.url.path() == "/api/v1/subtitles").unwrap();
    let q = search.url.query().unwrap();
    assert!(
        q.contains("imdb_id=113277") && q.contains("tmdb_id=949") && q.contains("moviehash="),
        "{q}"
    );
    // the hash is the OpenSubtitles hash of the file
    let (hash, _) = reelix_subtitles::hash::file_hash(&t.video()).unwrap();
    assert!(q.contains(&format!("moviehash={hash}")), "{q}");
}

#[tokio::test]
async fn timing_check_turns_down_a_subtitle_for_another_cut() {
    let t = setup().await;
    mount_opensubtitles(&t, &[(7002, LONG_SRT)]).await;
    t.settings(with_opensubtitles(vec![english(false, true, false)]))
        .await;
    let r = t
        .svc
        .search_file(SubtitleMediaKind::Movie, t.file_id)
        .await
        .unwrap();
    assert!(r.saved.is_empty());
    assert_eq!(r.rejected.len(), 1, "{:?}", r.rejected);
    assert_eq!(r.missing, vec![("en".to_string(), Variant::Sdh)]);
    assert!(!t.sidecar("en.sdh.srt").exists());
}

#[tokio::test]
async fn a_hash_match_found_later_replaces_an_id_match() {
    let t = setup().await;
    mount_opensubtitles(&t, &[(7001, FILM_SRT)]).await;
    t.settings(with_opensubtitles(vec![english(true, false, false)]))
        .await;
    // an earlier subtitle matched by id only
    std::fs::write(t.sidecar("en.srt"), "old").unwrap();
    sqlx::query(
        "INSERT INTO subtitle_files (media_kind, file_id, language, variant, path, source, source_id,
            match_level, fetched_at) VALUES ('movie', ?, 'en', 'normal', ?, 'stremio', 'x', 'id', 0)",
    )
    .bind(t.file_id)
    .bind(t.sidecar("en.srt").to_string_lossy().into_owned())
    .execute(t.db.pool())
    .await
    .unwrap();
    let r = t
        .svc
        .search_file(SubtitleMediaKind::Movie, t.file_id)
        .await
        .unwrap();
    assert_eq!(r.saved, vec![("en".into(), Variant::Normal, MatchLevel::Hash)]);
    assert_eq!(read(&t.sidecar("en.srt")), FILM_SRT);
    let rec = t.svc.records(SubtitleMediaKind::Movie, t.file_id).await.unwrap();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].match_level, "hash");

    // with upgrades off, nothing is asked again
    let before = t.server.received_requests().await.unwrap().len();
    sqlx::query("UPDATE subtitle_files SET match_level = 'id'")
        .execute(t.db.pool())
        .await
        .unwrap();
    let mut s = with_opensubtitles(vec![english(true, false, false)]);
    s.upgrade = false;
    t.settings(s).await;
    let r = t
        .svc
        .search_file(SubtitleMediaKind::Movie, t.file_id)
        .await
        .unwrap();
    assert!(r.saved.is_empty());
    assert_eq!(t.server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn a_subtitle_someone_else_saved_is_left_alone() {
    let t = setup().await;
    mount_opensubtitles(&t, &[(7001, FILM_SRT)]).await;
    t.settings(with_opensubtitles(vec![english(true, false, false)]))
        .await;
    std::fs::write(t.sidecar("en.srt"), "from Bazarr").unwrap();
    let r = t
        .svc
        .search_file(SubtitleMediaKind::Movie, t.file_id)
        .await
        .unwrap();
    assert!(r.saved.is_empty() && r.missing.is_empty());
    assert_eq!(read(&t.sidecar("en.srt")), "from Bazarr");
    assert!(t.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn stremio_addon_supplies_normal_subtitles() {
    let t = setup().await;
    let base = format!("{}/addon", t.server.uri());
    // the add-on answers any extra arguments for the film
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            r"^/addon/subtitles/movie/tt0113277/.*\.json$",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(STREMIO.replace("{{BASE}}", &t.server.uri()))
                .insert_header("content-type", "application/json"),
        )
        .mount(&t.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/1001.srt"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&t.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/1002.srt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FILM_SRT))
        .mount(&t.server)
        .await;
    t.settings(SubtitleSettings {
        enabled: true,
        languages: vec![english(true, true, false)],
        stremio_addons: vec![format!("{base}/manifest.json")],
        ..Default::default()
    })
    .await;
    let r = t
        .svc
        .search_file(SubtitleMediaKind::Movie, t.file_id)
        .await
        .unwrap();
    // the first add-on subtitle fails to download, the next one is taken; add-ons have no SDH
    assert_eq!(r.saved, vec![("en".into(), Variant::Normal, MatchLevel::Id)]);
    assert_eq!(r.missing, vec![("en".to_string(), Variant::Sdh)]);
    assert_eq!(read(&t.sidecar("en.srt")), FILM_SRT);
    let rec = t.svc.records(SubtitleMediaKind::Movie, t.file_id).await.unwrap();
    assert_eq!(rec[0].source, "stremio");
}

#[tokio::test]
async fn an_import_puts_the_file_first_in_the_schedule() {
    let t = setup().await;
    mount_opensubtitles(&t, &[(7001, FILM_SRT)]).await;
    t.settings(with_opensubtitles(vec![english(true, false, false)]))
        .await;
    let created = DomainEvent::EntityChanged {
        entity: "movie_file".into(),
        action: "created".into(),
        id: t.file_id,
        user_id: None,
    };
    assert!(t.svc.on_event(&created).await.unwrap());
    assert_eq!(t.svc.run_due().await.unwrap(), 1);
    assert!(t.sidecar("en.srt").exists());
    // searched now; not due again until much later
    assert_eq!(t.svc.run_due().await.unwrap(), 0);
}

#[tokio::test]
async fn an_empty_download_allowance_stops_downloads() {
    let t = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(LOGIN)
                .insert_header("content-type", "application/json"),
        )
        .mount(&t.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SEARCH)
                .insert_header("content-type", "application/json"),
        )
        .mount(&t.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/download"))
        .respond_with(
            ResponseTemplate::new(406)
                .set_body_string(r#"{"message":"You have downloaded your allowed 20 subtitles for 24h"}"#),
        )
        .expect(1)
        .mount(&t.server)
        .await;
    t.settings(with_opensubtitles(vec![english(true, true, true)]))
        .await;
    let r = t.svc.search_file(SubtitleMediaKind::Movie, t.file_id).await;
    assert!(matches!(r, Err(Error::QuotaExhausted)), "{r:?}");
    // a second search does not ask for downloads again
    let _ = t.svc.search_file(SubtitleMediaKind::Movie, t.file_id).await;
}
