//! rTorrent adapter against recorded-style XML-RPC responses.

mod common;

use common::{build, fixture, settings_for};
use reelix_core::MediaKind;
use reelix_downloaders::torrent::{info_hash, sample_torrent};
use reelix_downloaders::*;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn xml(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/xml")
        .set_body_string(fixture(&format!("rtorrent/{name}")))
}

fn call(m: &str) -> wiremock::MockBuilder {
    Mock::given(method("POST"))
        .and(path("/RPC2"))
        .and(body_string_contains(
            format!("<methodName>{m}</methodName>").as_str(),
        ))
}

fn client(server: &MockServer) -> Box<dyn DownloadClient> {
    build(
        "rtorrent",
        settings_for(server, json!({ "tv_category": "tv reelix" })),
    )
}

#[tokio::test]
async fn add_raw_with_label_and_priority() {
    let server = MockServer::start().await;
    call("load.raw_start")
        .and(body_string_contains("<string>d.custom1.set=tv+reelix</string>"))
        .and(body_string_contains("<string>d.priority.set=3</string>"))
        .and(body_string_contains("<base64>"))
        .respond_with(xml("zero.xml"))
        .expect(1)
        .mount(&server)
        .await;
    call("load.normal")
        .and(body_string_contains("magnet:?xt=urn:btih:"))
        .respond_with(xml("zero.xml"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let bytes = sample_torrent("Shogun.S01E03.mkv");
    let opts = AddOptions {
        media_kind: Some(MediaKind::Series),
        priority: Some(Priority::High),
        ..Default::default()
    };
    let id = c
        .add(
            &ReleasePayload::TorrentFile {
                bytes: bytes.clone(),
                name: "s.torrent".into(),
            },
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), info_hash(&bytes).unwrap().to_ascii_uppercase());
    let paused = AddOptions {
        add_paused: true,
        ..Default::default()
    };
    c.add(
        &ReleasePayload::Magnet("magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A".into()),
        &paused,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn items_from_multicall() {
    let server = MockServer::start().await;
    call("d.multicall2")
        .and(body_string_contains("<string>d.custom1=</string>"))
        .respond_with(xml("multicall.xml"))
        .mount(&server)
        .await;
    let c = client(&server);
    let items = c.items().await.unwrap();
    assert_eq!(items.len(), 4); // the unlabelled ISO is ignored
    use DownloadState::*;
    let states: Vec<_> = items.iter().map(|i| i.state).collect();
    assert_eq!(states, [Completed, Completed, Downloading, Warning]);
    assert!(!items[0].can_be_removed); // still seeding
    assert!(items[1].can_be_removed);
    assert_eq!(items[1].seed_ratio, Some(2.0));
    assert_eq!(items[2].category.as_deref(), Some("tv reelix"));
    assert_eq!(items[2].eta_secs, Some(100));
    assert!(items[3].message.as_deref().unwrap().contains("unregistered"));
    assert_eq!(
        items[0].output_path.as_deref(),
        Some("/home/rtorrent/downloads/The.Holdovers.2023.1080p.BluRay.x264-PiGNUS")
    );
}

#[tokio::test]
async fn remove_uses_rtorrent_hash_casing_and_erasedata() {
    let server = MockServer::start().await;
    call("d.multicall2")
        .respond_with(xml("multicall.xml"))
        .mount(&server)
        .await;
    call("d.custom5.set")
        .and(body_string_contains(
            "<string>6F9D2A1B0C3E4D5F6071829304B5C6D7E8F90A12</string>",
        ))
        .respond_with(xml("zero.xml"))
        .expect(1)
        .mount(&server)
        .await;
    call("d.erase")
        .and(body_string_contains("6F9D2A1B0C3E4D5F6071829304B5C6D7E8F90A12"))
        .respond_with(xml("zero.xml"))
        .expect(1)
        .mount(&server)
        .await;
    for m in ["d.stop", "d.start", "d.priority.set"] {
        call(m)
            .respond_with(xml("zero.xml"))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = client(&server);
    let id: DownloadId = "6f9d2a1b0c3e4d5f6071829304b5c6d7e8f90a12".into();
    c.remove(&id, true).await.unwrap();
    c.pause(&id).await.unwrap();
    c.resume(&id).await.unwrap();
    c.set_priority(&id, Priority::Low).await.unwrap();
    assert!(matches!(
        c.pause(&"nope".into()).await,
        Err(ClientError::NotFound(_))
    ));
}

#[tokio::test]
async fn faults_test_and_status() {
    let server = MockServer::start().await;
    call("system.client_version")
        .respond_with(xml("version.xml"))
        .mount(&server)
        .await;
    call("directory.default")
        .respond_with(xml("directory.xml"))
        .mount(&server)
        .await;
    call("d.erase")
        .respond_with(xml("fault.xml"))
        .mount(&server)
        .await;
    call("d.multicall2")
        .respond_with(xml("multicall.xml"))
        .mount(&server)
        .await;
    let c = client(&server);
    c.test().await.unwrap();
    let st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("0.9.8"));
    assert_eq!(st.output_dirs[0].path, "/home/rtorrent/downloads");
    let err = c
        .remove(&"5e8c1f0a9b2d3c4e5f60718293a4b5c6d7e8f901".into(), false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("info-hash"), "{err}");

    let old = MockServer::start().await;
    call("system.client_version")
        .respond_with(xml("version_old.xml"))
        .mount(&old)
        .await;
    let c = client(&old);
    assert!(matches!(
        c.test().await,
        Err(ClientError::UnsupportedVersion { .. })
    ));
    assert!(!c.test_report().await.ok());
}
