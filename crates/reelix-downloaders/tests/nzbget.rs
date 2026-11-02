//! NZBGet adapter against recorded-style JSON-RPC responses.

mod common;

use common::{build, fixture, json_fixture, settings_for};
use reelix_core::MediaKind;
use reelix_downloaders::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn rpc(m: &str) -> wiremock::MockBuilder {
    Mock::given(method("POST"))
        .and(path("/jsonrpc"))
        // nzbget:tegbzn6789 (defaults)
        .and(header("authorization", "Basic bnpiZ2V0OnRlZ2J6bjY3ODk="))
        .and(body_partial_json(json!({ "method": m })))
}

async fn mount_basic(server: &MockServer) {
    rpc("version")
        .respond_with(json_fixture("nzbget/version.json"))
        .mount(server)
        .await;
    rpc("config")
        .respond_with(json_fixture("nzbget/config.json"))
        .mount(server)
        .await;
    rpc("status")
        .respond_with(json_fixture("nzbget/status.json"))
        .mount(server)
        .await;
    rpc("listgroups")
        .respond_with(json_fixture("nzbget/listgroups.json"))
        .mount(server)
        .await;
    rpc("history")
        .respond_with(json_fixture("nzbget/history.json"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn append_sends_base64_nzb_with_category_and_priority() {
    let server = MockServer::start().await;
    rpc("append")
        .respond_with(json_fixture("nzbget/append.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = build("nzbget", settings_for(&server, json!({})));
    let opts = AddOptions {
        media_kind: Some(MediaKind::Movie),
        priority: Some(Priority::Force),
        title: "Dune".into(),
        ..Default::default()
    };
    let nzb = fixture("sabnzbd/release.nzb").into_bytes();
    let id = c
        .add(
            &ReleasePayload::NzbFile {
                bytes: nzb,
                name: "x.nzb".into(),
            },
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "1337");
    // Partial-json can't wildcard array elements, so check the call shape manually.
    let req = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    let p = body["params"].as_array().unwrap();
    assert_eq!(p[0], "Dune.nzb");
    assert!(p[1].as_str().unwrap().starts_with("PD94bWwg")); // base64 of "<?xml "
    assert_eq!(p[2], "movies");
    assert_eq!(p[3], 900);
    assert_eq!(p[4], true);
    assert_eq!(p[5], false);
}

#[tokio::test]
async fn append_rejected() {
    let server = MockServer::start().await;
    rpc("append")
        .respond_with(json_fixture("nzbget/append_rejected.json"))
        .mount(&server)
        .await;
    let c = build("nzbget", settings_for(&server, json!({})));
    let err = c
        .add(
            &ReleasePayload::Url("http://idx/nzb/1".into()),
            &AddOptions::default(),
        )
        .await;
    assert!(matches!(err, Err(ClientError::Rejected(_))));
}

#[tokio::test]
async fn items_map_queue_and_history() {
    let server = MockServer::start().await;
    mount_basic(&server).await;
    let c = build("nzbget", settings_for(&server, json!({})));
    let items = c.items().await.unwrap();
    let ids: Vec<_> = items.iter().map(|i| i.download_id.as_str()).collect();
    // software and uncategorised entries are ignored, DELETED/MANUAL skipped.
    assert_eq!(ids, ["41", "42", "31", "32", "33"]);
    let get = |id: &str| items.iter().find(|i| i.download_id.as_str() == id).unwrap();
    assert_eq!(get("41").state, DownloadState::Downloading);
    assert_eq!(get("41").priority, Some(Priority::High));
    assert!((get("41").progress() - 0.5).abs() < 1e-9);
    assert_eq!(get("42").state, DownloadState::PostProcessing);
    assert_eq!(get("42").total_bytes, 1 << 32);
    assert_eq!(get("41").completed_at, None);
    assert!(items.iter().any(|i| i.completed_at == Some(1730150000)));
    let done = get("31");
    assert_eq!(done.state, DownloadState::Completed);
    assert_eq!(
        done.output_path.as_deref(),
        Some("/downloads/completed/movies/Oppenheimer.2023.1080p.BluRay.x264-SPARKS")
    );
    assert_eq!(done.total_bytes, (2u64 << 32) + 1_705_032_704);
    assert_eq!(get("32").failure, Some(UsenetFailure::PasswordProtected));
    assert_eq!(get("33").failure, Some(UsenetFailure::MissingArticles));
}

#[tokio::test]
async fn remove_picks_queue_or_history_command() {
    let server = MockServer::start().await;
    mount_basic(&server).await;
    rpc("editqueue")
        .and(body_partial_json(
            json!({ "params": ["GroupFinalDelete", "", [41]] }),
        ))
        .respond_with(json_fixture("nzbget/editqueue_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    rpc("editqueue")
        .and(body_partial_json(
            json!({ "params": ["HistoryDelete", "", [31]] }),
        ))
        .respond_with(json_fixture("nzbget/editqueue_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    rpc("editqueue")
        .and(body_partial_json(
            json!({ "params": ["GroupSetPriority", "50", [41]] }),
        ))
        .respond_with(json_fixture("nzbget/editqueue_ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = build("nzbget", settings_for(&server, json!({})));
    c.remove(&"41".into(), true).await.unwrap();
    c.remove(&"31".into(), false).await.unwrap();
    c.set_priority(&"41".into(), Priority::High).await.unwrap();
}

#[tokio::test]
async fn test_and_status() {
    let server = MockServer::start().await;
    mount_basic(&server).await;
    let c = build("nzbget", settings_for(&server, json!({})));
    // tv category is missing in NZBGet's config.
    let r = c.test_report().await;
    assert_eq!(r.checks.iter().filter(|c| c.ok).count(), 3);
    assert!(r.first_failure().unwrap().contains("tv"));
    let st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("21.1"));
    let kinds: Vec<_> = st.warnings.iter().map(|w| w.kind).collect();
    assert_eq!(kinds, [HealthKind::HistoryRetention, HealthKind::CategoryMissing]);
    assert_eq!(st.output_dirs[1].path, "/downloads/completed/movies");
    assert_eq!(c.categories().await.unwrap(), ["movies", "software"]);
}

#[tokio::test]
async fn wrong_password_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(path("/jsonrpc"))
        .respond_with(ResponseTemplate::new(401).set_body_string(fixture("nzbget/unauthorized.html")))
        .mount(&server)
        .await;
    let c = build("nzbget", settings_for(&server, json!({ "password": "nope" })));
    let r = c.test_report().await;
    assert!(r.checks[0].ok && !r.checks[1].ok);
    assert!(matches!(c.test().await, Err(ClientError::Auth(_))));
}
