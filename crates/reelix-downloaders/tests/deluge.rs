//! Deluge adapter against recorded-style Web UI JSON-RPC responses.

mod common;

use common::{build, json_fixture, settings_for};
use reelix_core::MediaKind;
use reelix_downloaders::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COOKIE: &str = "_session_id=9f8e7d6c5b4a39281f0e9d8c7b6a5948372615049f8e7d6c5b4a392817";

fn call(m: &str) -> wiremock::MockBuilder {
    Mock::given(method("POST"))
        .and(path("/json"))
        .and(header("cookie", COOKIE))
        .and(body_partial_json(json!({ "method": m })))
}

async fn mount_login(server: &MockServer, connected: bool) {
    Mock::given(method("POST"))
        .and(path("/json"))
        .and(body_partial_json(
            json!({ "method": "auth.login", "params": ["deluge"] }),
        ))
        .respond_with(json_fixture("deluge/login_ok.json").insert_header(
            "set-cookie",
            format!("{COOKIE}; Expires=Tue, 29 Oct 2026 12:00:00 GMT; Path=/json").as_str(),
        ))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/json"))
        .and(body_partial_json(json!({ "method": "auth.login" })))
        .respond_with(json_fixture("deluge/login_fail.json"))
        .mount(server)
        .await;
    let f = if connected {
        "deluge/connected_true.json"
    } else {
        "deluge/connected_false.json"
    };
    call("web.connected")
        .respond_with(json_fixture(f))
        .mount(server)
        .await;
    call("web.get_hosts")
        .respond_with(json_fixture("deluge/hosts.json"))
        .mount(server)
        .await;
    call("web.connect")
        .and(body_partial_json(
            json!({ "params": ["c4b1e3a2f9d84e7a9b0c1d2e3f4a5b6c"] }),
        ))
        .respond_with(json_fixture("deluge/null.json"))
        .expect(if connected { 0 } else { 1 }..)
        .mount(server)
        .await;
    call("daemon.info")
        .respond_with(json_fixture("deluge/daemon_info.json"))
        .mount(server)
        .await;
    call("core.get_enabled_plugins")
        .respond_with(json_fixture("deluge/plugins.json"))
        .mount(server)
        .await;
    call("label.get_labels")
        .respond_with(json_fixture("deluge/labels.json"))
        .mount(server)
        .await;
}

fn client(server: &MockServer) -> Box<dyn DownloadClient> {
    build("deluge", settings_for(server, json!({})))
}

#[tokio::test]
async fn add_magnet_creates_label_and_connects_daemon() {
    let server = MockServer::start().await;
    mount_login(&server, false).await;
    call("core.add_torrent_magnet")
        .and(body_partial_json(json!({ "params": ["magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK", { "add_paused": false, "stop_at_ratio": true, "stop_ratio": 1.0 }] })))
        .respond_with(json_fixture("deluge/add_hash.json"))
        .expect(1)
        .mount(&server)
        .await;
    call("label.add")
        .and(body_partial_json(json!({ "params": ["tv-reelix"] })))
        .respond_with(json_fixture("deluge/null.json"))
        .expect(1)
        .mount(&server)
        .await;
    call("label.set_torrent")
        .and(body_partial_json(
            json!({ "params": ["c12fe1c06bba254a9dc9f519b335aa7c1367a88a", "tv-reelix"] }),
        ))
        .respond_with(json_fixture("deluge/null.json"))
        .expect(1)
        .mount(&server)
        .await;
    call("core.queue_top")
        .respond_with(json_fixture("deluge/null.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let opts = AddOptions {
        media_kind: Some(MediaKind::Series),
        priority: Some(Priority::High),
        seed_ratio: Some(1.0),
        ..Default::default()
    };
    let id = c
        .add(
            &ReleasePayload::Magnet("magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK".into()),
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
}

#[tokio::test]
async fn items_by_label_with_state_mapping() {
    let server = MockServer::start().await;
    mount_login(&server, true).await;
    call("core.get_torrents_status")
        .and(body_partial_json(
            json!({ "params": [{ "label": "movies-reelix" }] }),
        ))
        .respond_with(json_fixture("deluge/torrents_movies.json"))
        .mount(&server)
        .await;
    call("core.get_torrents_status")
        .and(body_partial_json(json!({ "params": [{ "label": "tv-reelix" }] })))
        .respond_with(json_fixture("deluge/torrents_empty.json"))
        .mount(&server)
        .await;
    let c = client(&server);
    let mut items = c.items().await.unwrap();
    items.sort_by(|a, b| a.download_id.0.cmp(&b.download_id.0));
    use DownloadState::*;
    let states: Vec<_> = items.iter().map(|i| i.state).collect();
    assert_eq!(states, [Completed, Completed, Downloading, Warning]);
    assert!(!items[0].can_be_removed); // still seeding
    assert!(items[1].can_be_removed); // paused after reaching ratio
    assert_eq!(
        items[1].output_path.as_deref(),
        Some("/var/lib/deluge/Downloads/Perfect.Days.2023.1080p.BluRay.x264-USURY")
    );
    assert_eq!(items[2].remaining_bytes, 3_000_000_000);
    assert_eq!(items[2].eta_secs, Some(900));
    assert!(items[3].message.as_deref().unwrap().contains("No space"));
}

#[tokio::test]
async fn relogin_on_not_authenticated() {
    let server = MockServer::start().await;
    mount_login(&server, true).await;
    call("core.remove_torrent")
        .respond_with(json_fixture("deluge/not_authenticated.json"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    call("core.remove_torrent")
        .and(body_partial_json(json!({ "params": ["abc", true] })))
        .respond_with(json_fixture("deluge/true.json"))
        .expect(1)
        .mount(&server)
        .await;
    for m in ["core.pause_torrent", "core.resume_torrent"] {
        call(m)
            .and(body_partial_json(json!({ "params": [["abc"]] })))
            .respond_with(json_fixture("deluge/null.json"))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = client(&server);
    c.remove(&"abc".into(), true).await.unwrap();
    c.pause(&"abc".into()).await.unwrap();
    c.resume(&"abc".into()).await.unwrap();
}

#[tokio::test]
async fn test_status_and_wrong_password() {
    let server = MockServer::start().await;
    mount_login(&server, true).await;
    call("core.get_config_value")
        .respond_with(json_fixture("deluge/download_location.json"))
        .mount(&server)
        .await;
    let c = client(&server);
    c.test().await.unwrap();
    let st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("2.1.1"));
    assert_eq!(st.output_dirs[0].path, "/var/lib/deluge/Downloads");
    assert_eq!(st.warnings.len(), 1); // tv-reelix label not created yet
    assert_eq!(st.warnings[0].kind, HealthKind::CategoryMissing);

    let bad = build("deluge", settings_for(&server, json!({ "password": "nope" })));
    let r = bad.test_report().await;
    assert!(r.checks[0].ok && !r.checks[1].ok);
}

#[tokio::test]
async fn label_plugin_required() {
    let server = MockServer::start().await;
    call("core.get_enabled_plugins")
        .respond_with(json_fixture("deluge/plugins_nolabel.json"))
        .with_priority(1)
        .mount(&server)
        .await;
    mount_login(&server, true).await;
    let c = client(&server);
    let r = c.test_report().await;
    assert_eq!(r.checks.last().unwrap().name, "categories");
    assert!(!r.checks.last().unwrap().ok);
}

#[tokio::test]
async fn unreachable_web_ui() {
    let server = MockServer::start().await;
    Mock::given(path("/json"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&server)
        .await;
    let c = client(&server);
    assert!(matches!(
        c.test().await,
        Err(ClientError::Http { status: 502, .. })
    ));
}
