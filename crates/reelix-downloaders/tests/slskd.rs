mod common;

use common::{build, fixture, json_fixture, settings_for};
use reelix_core::Protocol;
use reelix_core::soulseek::{SoulseekFile, SoulseekRelease};
use reelix_downloaders::slskd::{Slskd, UserTransfers};
use reelix_downloaders::*;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings(server: &MockServer, extra: Value) -> Value {
    let mut s = settings_for(server, json!({"api_key": "key", "downloads_dir": "/downloads"}));
    s.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    s
}

fn release(username: &str) -> SoulseekRelease {
    let file = |n: &str, size| SoulseekFile {
        filename: format!(r"@@abcde\Music\Radiohead\1997 - OK Computer\{n}"),
        size,
        bit_rate: None,
        bit_depth: Some(16),
        sample_rate: Some(44100),
        length: Some(284),
        is_variable_bit_rate: None,
    };
    SoulseekRelease {
        username: username.into(),
        directory: r"@@abcde\Music\Radiohead\1997 - OK Computer".into(),
        files: vec![
            file("01 - Airbag.flac", 31457280),
            file("02 - Paranoid Android.flac", 41943040),
        ],
        has_free_upload_slot: true,
        queue_length: 0,
        upload_speed: 1048576,
    }
}

#[tokio::test]
async fn enqueue_posts_every_file_of_the_folder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/transfers/downloads/vinyl%20hoarder"))
        .and(header("X-API-Key", "key"))
        .and(body_json(json!([
            {"filename": "@@abcde\\Music\\Radiohead\\1997 - OK Computer\\01 - Airbag.flac", "size": 31457280},
            {"filename": "@@abcde\\Music\\Radiohead\\1997 - OK Computer\\02 - Paranoid Android.flac", "size": 41943040}
        ])))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "enqueued": [{"id": "0b6f7e1a-2c3d-4e5f-8a9b-0c1d2e3f4a5b", "username": "vinyl hoarder", "direction": "Download",
                          "filename": "@@abcde\\Music\\Radiohead\\1997 - OK Computer\\01 - Airbag.flac", "size": 31457280,
                          "state": "Queued, Remotely", "requestedAt": "2026-11-05T14:03:01.1234567", "bytesTransferred": 0,
                          "averageSpeed": 0, "attempts": 0, "removed": false, "bytesRemaining": 31457280, "percentComplete": 0}],
            "failed": []
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = build("slskd", settings(&server, json!({})));
    assert_eq!(c.protocol(), Protocol::Soulseek);
    let id = c
        .add(
            &ReleasePayload::Soulseek(Box::new(release("vinyl hoarder"))),
            &AddOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        id.as_str(),
        r"vinyl hoarder/@@abcde\Music\Radiohead\1997 - OK Computer"
    );

    // Other payloads are refused without a request.
    let err = c
        .add(
            &ReleasePayload::Magnet("magnet:?xt=urn:btih:abc".into()),
            &AddOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::InvalidPayload(_)), "{err:?}");
}

#[tokio::test]
async fn offline_peer_is_a_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/transfers/downloads/ghost"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("Failed to establish a direct or indirect message connection to ghost"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/transfers/downloads/picky"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"enqueued": [], "failed": [{}, {}]})))
        .mount(&server)
        .await;
    let c = build("slskd", settings(&server, json!({})));
    for user in ["ghost", "picky"] {
        let err = c
            .add(
                &ReleasePayload::Soulseek(Box::new(release(user))),
                &AddOptions::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Rejected(_)), "{user}: {err:?}");
    }
}

#[tokio::test]
async fn transfers_fold_into_one_item_per_folder() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/transfers/downloads"))
        .and(header("X-API-Key", "key"))
        .respond_with(json_fixture("slskd/downloads.json"))
        .mount(&server)
        .await;
    let c = build("slskd", settings(&server, json!({})));
    let items = c.items().await.unwrap();
    let by = |user: &str| {
        items
            .iter()
            .find(|i| i.download_id.as_str().starts_with(&format!("{user}/")))
            .unwrap()
    };
    let ok = by("vinylhoarder");
    assert_eq!(ok.state, DownloadState::Downloading);
    assert_eq!((ok.total_bytes, ok.remaining_bytes), (73400320, 31457280));
    assert_eq!(ok.eta_secs, Some(30));
    assert_eq!(ok.title, "1997 - OK Computer");
    assert!(ok.output_path.is_none() && !ok.can_be_removed);

    let bad = by("audiophile_99");
    assert_eq!(bad.state, DownloadState::Failed);
    assert_eq!(
        bad.message.as_deref(),
        Some("1 of 2 files failed: Transfer rejected: File not shared.")
    );
    assert!(bad.can_be_removed);

    let queued = by("slowpoke");
    assert_eq!(queued.state, DownloadState::Queued);
    assert_eq!(queued.message.as_deref(), Some("Queued by the peer (position 4)"));

    // A timed-out file slskd will retry (`nextAttemptAt`) is not a failure yet.
    assert_eq!(by("flaky").state, DownloadState::Downloading);
    assert!((by("flaky").progress() - 0.6).abs() < 1e-9);

    // Without "fail if any file fails" the folder completes with what arrived.
    let lenient = build("slskd", settings(&server, json!({"fail_on_file_error": false})));
    let items = lenient.items().await.unwrap();
    let partial = items
        .iter()
        .find(|i| i.download_id.as_str().starts_with("audiophile_99/"))
        .unwrap();
    assert_eq!(partial.state, DownloadState::Completed);
    assert_eq!(
        partial.output_path.as_deref(),
        Some("/downloads/Radiohead - OK Computer (1997) [24-96]")
    );
    assert_eq!(partial.completed_at, Some(1793887460));
    assert!(
        partial
            .message
            .as_deref()
            .unwrap()
            .starts_with("1 of 2 files failed")
    );
}

#[test]
fn completed_folder_lands_under_the_last_remote_component() {
    let users: Vec<UserTransfers> = serde_json::from_str(&fixture("slskd/downloads.json")).unwrap();
    let mut files = users[0].directories[0].files.clone();
    files[1].state = "Completed, Succeeded".into();
    files[1].bytes_transferred = files[1].size;
    let c = Slskd::from_settings(
        &json!({"api_key": "k", "downloads_dir": "/app/downloads/"}),
        &BuildContext::default(),
    )
    .unwrap();
    let item = c.item("vinylhoarder", &users[0].directories[0].directory, &files);
    assert_eq!(item.state, DownloadState::Completed);
    assert_eq!(
        item.output_path.as_deref(),
        Some("/app/downloads/1997 - OK Computer")
    );
    assert_eq!((item.remaining_bytes, item.progress()), (0, 1.0));
    assert!(item.can_move_files && item.can_be_removed);
    // Windows-hosted slskd.
    let w = Slskd::from_settings(
        &json!({"api_key": "k", "downloads_dir": r"D:\slskd\downloads"}),
        &BuildContext::default(),
    )
    .unwrap();
    assert_eq!(w.output_path("music/Artist/Album"), r"D:\slskd\downloads\Album");
}

#[tokio::test]
async fn remove_clears_every_transfer_of_the_folder() {
    let server = MockServer::start().await;
    let users: Value = serde_json::from_str(&fixture("slskd/downloads.json")).unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v0/transfers/downloads/vinylhoarder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&users[0]))
        .mount(&server)
        .await;
    for id in [
        "0b6f7e1a-2c3d-4e5f-8a9b-0c1d2e3f4a5b",
        "9a8b7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d",
    ] {
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v0/transfers/downloads/vinylhoarder/{id}")))
            .and(query_param("remove", "true"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    // A user without downloads left answers 404: nothing to do.
    Mock::given(method("GET"))
        .and(path("/api/v0/transfers/downloads/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = build("slskd", settings(&server, json!({})));
    c.remove(
        &DownloadId(r"vinylhoarder/@@abcde\Music\Radiohead\1997 - OK Computer".into()),
        true,
    )
    .await
    .unwrap();
    c.remove(&DownloadId("gone/x".into()), false).await.unwrap();
}

#[tokio::test]
async fn test_report_and_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/session"))
        .and(header("X-API-Key", "key"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v0/session"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v0/application"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": {"full": "0.26.2.0", "current": "0.26.2", "latest": "0.26.2", "isUpdateAvailable": false},
            "server": {"address": "vps.slsknet.org:2271", "state": "Disconnected", "isConnected": false, "isLoggedIn": false}
        })))
        .mount(&server)
        .await;
    let c = build("slskd", settings(&server, json!({})));
    let r = c.test_report().await;
    let names: Vec<(&str, bool)> = r.checks.iter().map(|c| (c.name.as_str(), c.ok)).collect();
    assert_eq!(
        names,
        [("connection", true), ("api key", true), ("soulseek", false)]
    );
    assert!(c.test().await.is_err());
    let s = c.status().await.unwrap();
    assert_eq!(s.version.as_deref(), Some("0.26.2"));
    assert_eq!(s.output_dirs[0].path, "/downloads");
    assert_eq!(s.warnings[0].kind, HealthKind::ClientWarning);

    let wrong = build("slskd", settings(&server, json!({"api_key": "nope"})));
    let r = wrong.test_report().await;
    assert!(!r.ok());
    assert!(r.first_failure().unwrap().starts_with("api key"), "{r:?}");
}
