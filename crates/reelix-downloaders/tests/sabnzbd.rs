//! SABnzbd adapter against recorded-style API responses.

mod common;

use common::{build, fixture, json_fixture, settings_for};
use reelix_core::{MediaKind, Protocol};
use reelix_downloaders::*;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/sabnzbd/api";

async fn sab(server: &MockServer, extra: serde_json::Value) -> Box<dyn DownloadClient> {
    let mut s = json!({ "api_key": "0123456789abcdef0123456789abcdef" });
    s.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    build("sabnzbd", settings_for(server, s))
}

fn mode(m: &str) -> wiremock::MockBuilder {
    Mock::given(path(API))
        .and(query_param("mode", m))
        .and(query_param("output", "json"))
        .and(query_param("apikey", "0123456789abcdef0123456789abcdef"))
}

fn nzb() -> Vec<u8> {
    fixture("sabnzbd/release.nzb").into_bytes()
}

#[tokio::test]
async fn schema_defaults_match_readme() {
    let s = schema("sabnzbd").unwrap();
    let def = |n: &str| s.fields.iter().find(|f| f.name == n).unwrap().default.clone();
    assert_eq!(def("name"), Some(json!("SABnzbd")));
    assert_eq!(def("port"), Some(json!(8080)));
    assert_eq!(def("url_base"), Some(json!("/sabnzbd")));
    assert_eq!(def("movie_category"), Some(json!("movies")));
    assert_eq!(def("tv_category"), Some(json!("tv")));
    assert_eq!(def("grab_method"), Some(json!("upload_nzb")));
    assert_eq!(def("recent_priority"), Some(json!("Default")));
    let secrets: Vec<_> = s.secret_fields().collect();
    assert!(secrets.contains(&"api_key") && secrets.contains(&"password"));
    // API key is required.
    let err = build_err(json!({}));
    assert!(err.contains("API key"), "{err}");
}

fn build_err(settings: serde_json::Value) -> String {
    match reelix_downloaders::build("sabnzbd", &settings, &BuildContext::default()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected error"),
    }
}

#[tokio::test]
async fn add_uploads_nzb_with_category_priority_and_name() {
    let server = MockServer::start().await;
    mode("addfile")
        .and(method("POST"))
        .and(query_param("cat", "tv"))
        .and(query_param("priority", "1"))
        .and(query_param("nzbname", "Severance.S02E03.1080p.WEB.h264-ETHEL"))
        .and(body_string_contains("name=\"name\""))
        .and(body_string_contains("<nzb xmlns"))
        .respond_with(json_fixture("sabnzbd/add_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = sab(&server, json!({ "recent_priority": "High" })).await;
    assert_eq!(c.protocol(), Protocol::Usenet);
    assert_eq!(c.payload_preference(), PayloadPreference::File);
    let opts = AddOptions {
        media_kind: Some(MediaKind::Series),
        is_recent: true,
        title: "Severance.S02E03.1080p.WEB.h264-ETHEL".into(),
        ..Default::default()
    };
    let id = c
        .add(
            &ReleasePayload::NzbFile {
                bytes: nzb(),
                name: String::new(),
            },
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "SABnzbd_nzo_r4x1k2ab");
}

#[tokio::test]
async fn add_rejects_invalid_nzb_before_sending() {
    let server = MockServer::start().await;
    mode("addfile")
        .respond_with(json_fixture("sabnzbd/add_ok.json"))
        .expect(0)
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let bad = ReleasePayload::NzbFile {
        bytes: b"<html>429 Too Many Requests</html>".to_vec(),
        name: "x.nzb".into(),
    };
    let err = c.add(&bad, &AddOptions::default()).await.unwrap_err();
    assert!(matches!(err, ClientError::InvalidPayload(_)), "{err}");
    let err = c
        .add(
            &ReleasePayload::Magnet("magnet:?xt=urn:btih:abc".into()),
            &AddOptions::default(),
        )
        .await;
    assert!(matches!(err, Err(ClientError::InvalidPayload(_))));
}

#[tokio::test]
async fn add_url_modes() {
    let server = MockServer::start().await;
    let nzb_url = format!("{}/getnzb/abc.nzb?apikey=indexerkey", server.uri());
    // send_url: SABnzbd fetches the link itself.
    mode("addurl")
        .and(query_param("name", nzb_url.as_str()))
        .and(query_param("cat", "movies"))
        .and(query_param("priority", "-100"))
        .respond_with(json_fixture("sabnzbd/add_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = sab(&server, json!({ "grab_method": "send_url" })).await;
    assert_eq!(c.payload_preference(), PayloadPreference::Url);
    let opts = AddOptions {
        media_kind: Some(MediaKind::Movie),
        title: "Arrival".into(),
        ..Default::default()
    };
    c.add(&ReleasePayload::Url(nzb_url.clone()), &opts).await.unwrap();

    // upload_nzb with a URL: Reelix fetches and uploads.
    Mock::given(path("/getnzb/abc.nzb"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(nzb()))
        .expect(1)
        .mount(&server)
        .await;
    mode("addfile")
        .and(method("POST"))
        .and(query_param("priority", "-2"))
        .respond_with(json_fixture("sabnzbd/add_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let opts = AddOptions {
        add_paused: true,
        ..opts
    };
    c.add(&ReleasePayload::Url(nzb_url), &opts).await.unwrap();
}

#[tokio::test]
async fn add_without_nzo_id_is_rejected() {
    let server = MockServer::start().await;
    mode("addfile")
        .respond_with(json_fixture("sabnzbd/add_empty.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let err = c
        .add(
            &ReleasePayload::NzbFile {
                bytes: nzb(),
                name: "a.nzb".into(),
            },
            &AddOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Rejected(_)), "{err}");
}

async fn mount_items(server: &MockServer) {
    mode("queue")
        .and(query_param("cat", "movies"))
        .respond_with(json_fixture("sabnzbd/queue_movies.json"))
        .mount(server)
        .await;
    mode("queue")
        .and(query_param("cat", "tv"))
        .respond_with(json_fixture("sabnzbd/queue_tv_paused.json"))
        .mount(server)
        .await;
    mode("history")
        .and(query_param("category", "movies"))
        .respond_with(json_fixture("sabnzbd/history_movies.json"))
        .mount(server)
        .await;
    mode("history")
        .and(query_param("category", "tv"))
        .respond_with(json_fixture("sabnzbd/history_tv.json"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn items_merge_queue_and_history_with_status_mapping() {
    let server = MockServer::start().await;
    mount_items(&server).await;
    let c = sab(&server, json!({})).await;
    let items = c.items().await.unwrap();
    assert_eq!(items.len(), 10);
    let get = |id: &str| items.iter().find(|i| i.download_id.as_str() == id).unwrap();

    let dl = get("SABnzbd_nzo_q1");
    assert_eq!(dl.state, DownloadState::Downloading);
    assert_eq!(dl.eta_secs, Some(104));
    assert_eq!(dl.priority, Some(Priority::High));
    assert!((dl.progress() - (1.0 - 4911.20 / 6189.43)).abs() < 1e-3);
    assert_eq!(get("SABnzbd_nzo_q2").state, DownloadState::Queued);
    assert_eq!(get("SABnzbd_nzo_q3").state, DownloadState::Queued);
    assert_eq!(get("SABnzbd_nzo_q3").message.as_deref(), Some("Fetching NZB"));
    // Globally paused queue.
    assert_eq!(get("SABnzbd_nzo_t1").state, DownloadState::Paused);

    let done = get("SABnzbd_nzo_h1");
    assert_eq!(done.completed_at, Some(1730190000));
    assert_eq!(done.state, DownloadState::Completed);
    assert_eq!(
        done.output_path.as_deref(),
        Some("/downloads/complete/movies/Prisoners.2013.1080p.BluRay.x264-SPARKS")
    );
    assert!(done.can_move_files && done.can_be_removed);
    assert_eq!(done.total_bytes, 9_876_543_210);

    let f = |id: &str| (get(id).state, get(id).failure);
    assert_eq!(
        f("SABnzbd_nzo_h2"),
        (DownloadState::Failed, Some(UsenetFailure::MissingArticles))
    );
    assert_eq!(
        f("SABnzbd_nzo_h3"),
        (DownloadState::Failed, Some(UsenetFailure::PasswordProtected))
    );
    assert_eq!(
        f("SABnzbd_nzo_h4"),
        (DownloadState::Failed, Some(UsenetFailure::DiskFull))
    );
    assert!(!UsenetFailure::DiskFull.action().blocklist);
    assert!(
        get("SABnzbd_nzo_h3")
            .message
            .as_deref()
            .unwrap()
            .contains("password")
    );

    let pp = get("SABnzbd_nzo_h5");
    assert_eq!(pp.state, DownloadState::PostProcessing);
    assert_eq!(pp.message.as_deref(), Some("Extracting: Unpacking: 03/07"));
    assert_eq!(get("SABnzbd_nzo_th1").state, DownloadState::PostProcessing);
    // Only Reelix categories were queried: every item is movies or tv.
    assert!(
        items
            .iter()
            .all(|i| matches!(i.category.as_deref(), Some("movies" | "tv")))
    );
}

#[tokio::test]
async fn remove_deletes_from_queue_or_history() {
    let server = MockServer::start().await;
    mode("queue")
        .and(query_param("nzo_ids", "SABnzbd_nzo_q1"))
        .and(query_param_is_missing("name"))
        .respond_with(json_fixture("sabnzbd/queue_movies.json"))
        .mount(&server)
        .await;
    mode("queue")
        .and(query_param("nzo_ids", "SABnzbd_nzo_h1"))
        .and(query_param_is_missing("name"))
        .respond_with(json_fixture("sabnzbd/queue_empty.json"))
        .mount(&server)
        .await;
    mode("queue")
        .and(query_param("name", "delete"))
        .and(query_param("value", "SABnzbd_nzo_q1"))
        .and(query_param("del_files", "1"))
        .respond_with(json_fixture("sabnzbd/status_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    mode("history")
        .and(query_param("name", "delete"))
        .and(query_param("value", "SABnzbd_nzo_h1"))
        .and(query_param("del_files", "0"))
        .and(query_param("archive", "0"))
        .respond_with(json_fixture("sabnzbd/status_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    c.remove(&"SABnzbd_nzo_q1".into(), true).await.unwrap();
    c.remove(&"SABnzbd_nzo_h1".into(), false).await.unwrap();
}

#[tokio::test]
async fn priority_pause_resume() {
    let server = MockServer::start().await;
    mode("queue")
        .and(query_param("name", "priority"))
        .and(query_param("value", "SABnzbd_nzo_q2"))
        .and(query_param("value2", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"position": 0}"#))
        .expect(1)
        .mount(&server)
        .await;
    for action in ["pause", "resume"] {
        mode("queue")
            .and(query_param("name", action))
            .respond_with(json_fixture("sabnzbd/status_ok.json"))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = sab(&server, json!({})).await;
    let id: DownloadId = "SABnzbd_nzo_q2".into();
    c.set_priority(&id, Priority::Force).await.unwrap();
    c.pause(&id).await.unwrap();
    c.resume(&id).await.unwrap();
}

#[tokio::test]
async fn test_passes_all_checks() {
    let server = MockServer::start().await;
    mode("version")
        .respond_with(json_fixture("sabnzbd/version_4.json"))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/config_misc.json"))
        .mount(&server)
        .await;
    mode("get_cats")
        .respond_with(json_fixture("sabnzbd/cats.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    c.test().await.unwrap();
    let r = c.test_report().await;
    let names: Vec<_> = r.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["connect", "auth", "version", "categories"]);
    assert!(r.ok());
    assert_eq!(c.categories().await.unwrap(), ["movies", "tv", "software"]);
}

#[tokio::test]
async fn test_reports_missing_category_and_old_version() {
    let server = MockServer::start().await;
    mode("version")
        .respond_with(json_fixture("sabnzbd/version_old.json"))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/config_misc.json"))
        .mount(&server)
        .await;
    mode("get_cats")
        .respond_with(json_fixture("sabnzbd/cats.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    assert!(matches!(
        c.test().await,
        Err(ClientError::UnsupportedVersion { .. })
    ));

    let server = MockServer::start().await;
    mode("version")
        .respond_with(json_fixture("sabnzbd/version_4.json"))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/config_misc.json"))
        .mount(&server)
        .await;
    mode("get_cats")
        .respond_with(json_fixture("sabnzbd/cats.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({ "movie_category": "films" })).await;
    let r = c.test_report().await;
    assert!(!r.ok());
    assert!(r.first_failure().unwrap().contains("films"));
}

#[tokio::test]
async fn test_detects_nzb_key_and_bad_key() {
    let server = MockServer::start().await;
    mode("version")
        .respond_with(json_fixture("sabnzbd/version_4.json"))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/error_apikey.json"))
        .mount(&server)
        .await;
    // The NZB key may call addurl: it fails on the missing parameter, not on auth.
    mode("addurl")
        .respond_with(json_fixture("sabnzbd/error_param.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let err = c.test().await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Auth(m) if m.contains("NZB key")),
        "{err}"
    );
    let st = c.status().await.unwrap();
    assert_eq!(st.warnings[0].kind, HealthKind::NzbKeyOnly);

    let server = MockServer::start().await;
    mode("version")
        .respond_with(json_fixture("sabnzbd/version_4.json"))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/error_apikey.json"))
        .mount(&server)
        .await;
    mode("addurl")
        .respond_with(json_fixture("sabnzbd/error_apikey.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let r = c.test_report().await;
    assert!(r.checks[0].ok);
    assert_eq!(r.checks[1].name, "auth");
    assert!(!r.checks[1].ok);
    assert_eq!(c.status().await.unwrap().warnings[0].kind, HealthKind::AuthFailed);
}

#[tokio::test]
async fn unreachable_is_a_connection_error() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let c = build(
        "sabnzbd",
        json!({ "host": "127.0.0.1", "port": port, "api_key": "k" }),
    );
    assert!(matches!(c.test().await, Err(ClientError::Connection(_))));
}

#[tokio::test]
async fn health_checks_sab4_sorters_warnings_and_dirs() {
    let server = MockServer::start().await;
    mode("version")
        .respond_with(json_fixture("sabnzbd/version_4.json"))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/config_4.json"))
        .mount(&server)
        .await;
    mode("warnings")
        .respond_with(json_fixture("sabnzbd/warnings.json"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let mut st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("4.3.3"));
    let kinds: Vec<_> = st.warnings.iter().map(|w| w.kind).collect();
    assert_eq!(
        kinds,
        [
            HealthKind::SortingEnabled,
            HealthKind::ClientWarning,
            HealthKind::ClientWarning
        ]
    );
    assert!(st.warnings[0].message.contains("`tv`"));
    assert!(st.warnings[1].message.contains("Too little diskspace"));
    let dirs: Vec<_> = st
        .output_dirs
        .iter()
        .map(|d| (d.category.as_deref(), d.path.as_str()))
        .collect();
    assert_eq!(
        dirs,
        [
            (None, "/downloads/complete"),
            (Some("movies"), "/downloads/complete/movies"),
            (Some("tv"), "/mnt/media-staging/tv"),
        ]
    );
    // Remote path mapping: only /downloads is mapped to an existing local folder.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("complete/movies")).unwrap();
    let root = tmp.path().to_path_buf();
    st.check_output_dirs(|remote| remote.strip_prefix("/downloads/").map(|r| root.join(r)));
    let unreachable: Vec<_> = st
        .warnings
        .iter()
        .filter(|w| w.kind == HealthKind::OutputDirUnreachable)
        .collect();
    assert_eq!(unreachable.len(), 1, "{unreachable:?}");
    assert!(unreachable[0].message.contains("/mnt/media-staging/tv"));
}

#[tokio::test]
async fn health_checks_sab3_switches_retention_and_categories() {
    let server = MockServer::start().await;
    mode("version")
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"version":"3.7.2"}"#))
        .mount(&server)
        .await;
    mode("get_config")
        .respond_with(json_fixture("sabnzbd/config_3.json"))
        .mount(&server)
        .await;
    mode("warnings")
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"warnings":[]}"#))
        .mount(&server)
        .await;
    let c = sab(&server, json!({})).await;
    let st = c.status().await.unwrap();
    let kinds: Vec<_> = st.warnings.iter().map(|w| w.kind).collect();
    // tv missing; TV sorting on tv (date sorting is off); retention deletes immediately.
    assert_eq!(
        kinds,
        [
            HealthKind::CategoryMissing,
            HealthKind::SortingEnabled,
            HealthKind::HistoryRetention
        ]
    );
    assert!(st.warnings[1].message.starts_with("TV sorting"));
    assert!(st.warnings.iter().all(|w| w.settings_path.is_some()));
    assert_eq!(st.output_dirs[1].path, "/data/usenet/complete");
}

#[tokio::test]
async fn basic_auth_and_plain_text_errors() {
    let server = MockServer::start().await;
    mode("version")
        .and(wiremock::matchers::header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200).set_body_string("error: API Key Required\n"))
        .mount(&server)
        .await;
    let c = sab(&server, json!({ "username": "user", "password": "pass" })).await;
    assert!(matches!(c.test().await, Err(ClientError::Auth(_))));
}

#[tokio::test]
async fn items_include_the_book_and_music_categories() {
    let server = MockServer::start().await;
    mount_items(&server).await;
    for cat in ["books", "music"] {
        mode("queue")
            .and(query_param("cat", cat))
            .respond_with(json_fixture("sabnzbd/queue_empty.json"))
            .expect(1)
            .mount(&server)
            .await;
        mode("history")
            .and(query_param("category", cat))
            .respond_with(json_fixture("sabnzbd/history_empty.json"))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = sab(
        &server,
        json!({ "book_category": "books", "music_category": "music" }),
    )
    .await;
    assert_eq!(c.items().await.unwrap().len(), 10);
    assert_eq!(
        c.category_for_kind(reelix_core::ContentKind::Music).as_deref(),
        Some("music")
    );
}
