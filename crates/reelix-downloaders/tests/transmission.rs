//! Transmission adapter against recorded-style RPC responses.

mod common;

use common::{build, json_fixture, settings_for};
use reelix_core::MediaKind;
use reelix_downloaders::torrent::sample_torrent;
use reelix_downloaders::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION: &str = "kLX7ZHN3uHW2i2NJWtpTnaZvhCdyNPBL9yKjI7HbR3P3XAdP";

const SESSION_HEADER: &str = "X-Transmission-Session-Id";

fn rpc(m: &str) -> wiremock::MockBuilder {
    Mock::given(method("POST"))
        .and(path("/transmission/rpc"))
        .and(header(SESSION_HEADER, SESSION))
        .and(body_partial_json(json!({ "method": m })))
}

async fn mount_409(server: &MockServer) {
    // Requests without the session header get the 409 handshake (lower priority than method mocks).
    Mock::given(method("POST"))
        .and(path("/transmission/rpc"))
        .respond_with(
            ResponseTemplate::new(409)
                .insert_header(SESSION_HEADER, SESSION)
                .set_body_string(
                    "<h1>409: Conflict</h1><p>Your request had an invalid session-id header.</p>",
                ),
        )
        .with_priority(10)
        .mount(server)
        .await;
}

async fn setup(session_fixture: &str) -> MockServer {
    let server = MockServer::start().await;
    mount_409(&server).await;
    rpc("session-get")
        .respond_with(json_fixture(session_fixture))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn add_file_with_category_dir_label_and_seed_limits() {
    let server = setup("transmission/session_get.json").await;
    rpc("torrent-add")
        .and(header_exists("authorization"))
        .and(body_partial_json(json!({ "arguments": {
            "download-dir": "/downloads/complete/tv-reelix",
            "labels": ["tv-reelix"],
            "paused": false,
            "bandwidthPriority": 1
        }})))
        .respond_with(json_fixture("transmission/torrent_added.json"))
        .expect(1)
        .mount(&server)
        .await;
    rpc("torrent-set")
        .and(body_partial_json(json!({ "arguments": {
            "ids": ["c12fe1c06bba254a9dc9f519b335aa7c1367a88a"],
            "seedRatioLimit": 1.0, "seedRatioMode": 1, "seedIdleLimit": 60, "seedIdleMode": 1
        }})))
        .respond_with(json_fixture("transmission/ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = build(
        "transmission",
        settings_for(&server, json!({ "username": "u", "password": "p" })),
    );
    let opts = AddOptions {
        media_kind: Some(MediaKind::Series),
        priority: Some(Priority::High),
        seed_ratio: Some(1.0),
        seed_time_minutes: Some(60),
        ..Default::default()
    };
    let id = c
        .add(
            &ReleasePayload::TorrentFile {
                bytes: sample_torrent("f.mkv"),
                name: "f.torrent".into(),
            },
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
}

#[tokio::test]
async fn duplicate_returns_existing_hash_and_errors_surface() {
    let server = setup("transmission/session_get.json").await;
    rpc("torrent-add")
        .and(body_partial_json(json!({ "arguments": { "filename": "magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A" } })))
        .respond_with(json_fixture("transmission/torrent_duplicate.json"))
        .mount(&server)
        .await;
    rpc("torrent-add")
        .respond_with(json_fixture("transmission/invalid_torrent.json"))
        .mount(&server)
        .await;
    let c = build("transmission", settings_for(&server, json!({})));
    let id = c
        .add(
            &ReleasePayload::Magnet("magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A".into()),
            &AddOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
    let err = c
        .add(
            &ReleasePayload::TorrentFile {
                bytes: sample_torrent("x"),
                name: "x.torrent".into(),
            },
            &AddOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("corrupt"), "{err}");
}

#[tokio::test]
async fn items_filter_by_dir_or_label_and_map_states() {
    let server = setup("transmission/session_get.json").await;
    rpc("torrent-get")
        .respond_with(json_fixture("transmission/torrent_get.json"))
        .mount(&server)
        .await;
    let c = build("transmission", settings_for(&server, json!({})));
    let items = c.items().await.unwrap();
    let ids: Vec<_> = items.iter().map(|i| &i.download_id.as_str()[..4]).collect();
    assert_eq!(ids, ["aa11", "bb22", "cc33", "ee55"]); // debian iso is not ours
    use DownloadState::*;
    let states: Vec<_> = items.iter().map(|i| i.state).collect();
    assert_eq!(states, [Completed, Completed, Warning, Queued]);
    assert!(!items[0].can_be_removed);
    assert!(items[1].can_be_removed);
    assert_eq!(items[1].category.as_deref(), Some("movies-reelix"));
    assert_eq!(
        items[1].output_path.as_deref(),
        Some("/downloads/complete/movies-reelix/The.Zone.of.Interest.2023.1080p.BluRay.x264-PiGNUS")
    );
    assert!(items[2].message.as_deref().unwrap().contains("404"));
    assert_eq!(items[2].eta_secs, Some(140));
    assert_eq!(items[3].eta_secs, None);
}

#[tokio::test]
async fn remove_pause_resume_priority() {
    let server = setup("transmission/session_get.json").await;
    rpc("torrent-remove")
        .and(body_partial_json(
            json!({ "arguments": { "ids": ["aa11"], "delete-local-data": true } }),
        ))
        .respond_with(json_fixture("transmission/ok.json"))
        .expect(1)
        .mount(&server)
        .await;
    for m in [
        "torrent-stop",
        "torrent-start",
        "queue-move-top",
        "torrent-start-now",
    ] {
        rpc(m)
            .respond_with(json_fixture("transmission/ok.json"))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = build("transmission", settings_for(&server, json!({})));
    let id: DownloadId = "aa11".into();
    c.remove(&id, true).await.unwrap();
    c.pause(&id).await.unwrap();
    c.resume(&id).await.unwrap();
    c.set_priority(&id, Priority::Force).await.unwrap();
}

#[tokio::test]
async fn test_and_status() {
    let server = setup("transmission/session_get.json").await;
    let c = build("transmission", settings_for(&server, json!({})));
    c.test().await.unwrap();
    let st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("4.0.6 (38c164933e)"));
    let dirs: Vec<_> = st.output_dirs.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        dirs,
        [
            "/downloads/complete",
            "/downloads/complete/movies-reelix",
            "/downloads/complete/tv-reelix"
        ]
    );

    let old = setup("transmission/session_old.json").await;
    let c = build("transmission", settings_for(&old, json!({})));
    assert!(matches!(
        c.test().await,
        Err(ClientError::UnsupportedVersion { .. })
    ));
    assert_eq!(
        c.status().await.unwrap().warnings[0].kind,
        HealthKind::VersionTooOld
    );
}

#[tokio::test]
async fn unauthorized() {
    let server = MockServer::start().await;
    Mock::given(path("/transmission/rpc"))
        .respond_with(ResponseTemplate::new(401).set_body_string("<h1>401: Unauthorized</h1>"))
        .mount(&server)
        .await;
    let c = build(
        "transmission",
        settings_for(&server, json!({ "username": "u", "password": "bad" })),
    );
    let r = c.test_report().await;
    assert!(r.checks[0].ok && !r.checks[1].ok);
}
