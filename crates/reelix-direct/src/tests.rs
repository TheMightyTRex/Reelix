use super::*;

fn request() -> DirectRequest {
    DirectRequest {
        url: "https://youtu.be/abcdefghijk?si=tracking".into(),
        source: DirectSource::Youtube,
        mode: DownloadMode::Audio,
        format: "original".into(),
        max_height: None,
        spotify_quality: None,
        playlist: false,
    }
}

#[test]
fn provider_urls_are_normalized_and_restricted() {
    let r = request();
    assert_eq!(
        validate(&r).unwrap(),
        "https://www.youtube.com/watch?v=abcdefghijk"
    );
    for url in [
        "http://youtube.com/watch?v=abcdefghijk",
        "https://youtube.com.evil.test/watch?v=abcdefghijk",
        "https://user@youtube.com/watch?v=abcdefghijk",
        "https://127.0.0.1/video",
        "--exec=evil",
        "https://youtube.com/redirect?q=https://localhost",
        "https://youtube.com/watch?v=--exec",
    ] {
        assert!(
            validate(&DirectRequest {
                url: url.into(),
                ..r.clone()
            })
            .is_err(),
            "{url}"
        );
    }
    let playlist = DirectRequest {
        url: "https://www.youtube.com/watch?v=abcdefghijk&list=PL_123&si=x".into(),
        playlist: true,
        ..r.clone()
    };
    assert_eq!(
        validate(&playlist).unwrap(),
        "https://www.youtube.com/playlist?list=PL_123"
    );
    assert!(
        validate(&DirectRequest {
            url: "https://youtube.com/playlist?list=PL123".into(),
            ..r.clone()
        })
        .is_err()
    );
    assert!(
        validate(&DirectRequest {
            mode: DownloadMode::Video,
            format: "mp4".into(),
            max_height: Some(9999),
            ..r.clone()
        })
        .is_err()
    );
    let spotify = DirectRequest {
        source: DirectSource::Spotify,
        url: "https://open.spotify.com/track/0123456789abcdefghijkl?si=secret".into(),
        ..r
    };
    assert_eq!(
        validate(&spotify).unwrap(),
        "https://open.spotify.com/track/0123456789abcdefghijkl"
    );
    assert!(
        validate(&DirectRequest {
            mode: DownloadMode::Video,
            ..spotify
        })
        .is_err()
    );
}

async fn service() -> (tempfile::TempDir, Arc<DirectService>) {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(DirectService::new(
        Db::in_memory().await.unwrap(),
        dir.path().to_path_buf(),
    ));
    let mut s = service.settings().await.unwrap();
    s.enabled = true;
    service.save_settings(s).await.unwrap();
    (dir, service)
}

#[tokio::test]
async fn queue_deduplicates_cancels_and_retries_without_losing_destination() {
    let (_dir, s) = service().await;
    let job = s.enqueue(request()).await.unwrap();
    assert!(
        s.enqueue(DirectRequest {
            url: "https://youtube.com/watch?v=abcdefghijk".into(),
            ..request()
        })
        .await
        .is_err()
    );
    assert!(s.retry(job.id).await.is_err());
    s.cancel(job.id).await.unwrap();
    assert_eq!(s.get(job.id).await.unwrap().state, "cancelled");
    s.retry(job.id).await.unwrap();
    assert_eq!(s.get(job.id).await.unwrap().output_dir, job.output_dir);
    assert_eq!(s.get(job.id).await.unwrap().state, "queued");
    assert!(s.cancel(99999).await.is_err());
    let mut settings = s.settings().await.unwrap();
    settings.enabled = false;
    s.save_settings(settings).await.unwrap();
    assert!(s.next().await.unwrap().is_none());
    assert!(s.enqueue(request()).await.is_err());
}

#[tokio::test]
async fn retry_cannot_duplicate_another_active_copy() {
    let (_dir, s) = service().await;
    let first = s.enqueue(request()).await.unwrap();
    s.cancel(first.id).await.unwrap();
    s.enqueue(request()).await.unwrap();
    assert!(s.retry(first.id).await.is_err());
}

#[tokio::test]
async fn invalid_credentials_fail_and_interrupted_work_is_requeued() {
    let (_dir, s) = service().await;
    let mut settings = s.settings().await.unwrap();
    let credentials = s.data_dir.join("invalid-credentials.json");
    tokio::fs::write(&credentials, "{}").await.unwrap();
    settings.spotify_credentials = credentials.to_string_lossy().into_owned();
    s.save_settings(settings).await.unwrap();
    let job = s
        .enqueue(DirectRequest {
            source: DirectSource::Spotify,
            url: "https://open.spotify.com/track/0123456789abcdefghijkl".into(),
            ..request()
        })
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let handle = tokio::spawn(s.clone().run(stop.clone()));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if s.get(job.id).await.unwrap().state == "failed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    handle.await.unwrap().unwrap();
    sqlx::query("UPDATE direct_downloads SET state='running' WHERE id=?")
        .bind(job.id)
        .execute(s.db.pool())
        .await
        .unwrap();
    let stop = CancellationToken::new();
    stop.cancel();
    s.clone().run(stop).await.unwrap();
    assert_eq!(s.get(job.id).await.unwrap().state, "queued");
}

#[tokio::test]
async fn queue_survives_reopening_database() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("queue.db").to_string_lossy());
    let db = Db::connect(&url).await.unwrap();
    let service = DirectService::new(db.clone(), dir.path().to_path_buf());
    let mut settings = service.settings().await.unwrap();
    settings.enabled = true;
    service.save_settings(settings).await.unwrap();
    let job = service.enqueue(request()).await.unwrap();
    db.pool().close().await;
    let reopened = DirectService::new(Db::connect(&url).await.unwrap(), dir.path().to_path_buf());
    assert_eq!(reopened.get(job.id).await.unwrap(), job);
}

#[test]
fn native_quality_ranges_and_filenames() {
    use librespot_metadata::audio::file::AudioFileFormat::*;
    assert_eq!(
        spotify::quality_order("auto", false).unwrap(),
        vec![OGG_VORBIS_160, OGG_VORBIS_96]
    );
    assert_eq!(spotify::quality_order("auto", true).unwrap()[0], OGG_VORBIS_320);
    assert!(spotify::quality_order("very_high", false).is_err());
    assert_eq!(transfer::parse_range("bytes 10-19/20").unwrap(), (10, 20));
    assert!(transfer::parse_range("bytes 20-10/30").is_err());
    assert!(transfer::parse_range("bytes 0-30/30").is_err());
    assert_eq!(process::safe_name("../A:B\\C"), "_A_B_C");
    assert_eq!(process::safe_name("CON"), "_CON");
    assert_eq!(process::safe_name(".."), "untitled");
}

#[tokio::test]
async fn native_transfer_resumes_and_restarts_when_range_is_ignored() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };
    let (_dir, s) = service().await;
    let server = MockServer::start().await;
    let output = s.data_dir.join("audio.part");
    tokio::fs::write(&output, b"hello").await.unwrap();
    Mock::given(method("GET"))
        .and(path("/resume"))
        .and(header("range", "bytes=5-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 5-10/11")
                .set_body_bytes(b" world".to_vec()),
        )
        .mount(&server)
        .await;
    s.transfer(&format!("{}/resume", server.uri()), &output, 0, None)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"hello world");
    Mock::given(path("/ignored"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"new data".to_vec()))
        .mount(&server)
        .await;
    s.transfer(&format!("{}/ignored", server.uri()), &output, 0, None)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"new data");
    Mock::given(path("/wrong"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-1/10")
                .set_body_bytes(b"no".to_vec()),
        )
        .mount(&server)
        .await;
    assert!(
        s.transfer(&format!("{}/wrong", server.uri()), &output, 0, None)
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"new data");
}

#[tokio::test]
async fn native_spotify_decrypts_and_resumes_at_encrypted_byte_offset() {
    use aes::cipher::{KeyIvInit, StreamCipher};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, path},
    };
    let (_dir, s) = service().await;
    let server = MockServer::start().await;
    let key = [7u8; 16];
    let iv = [
        0x72, 0xe0, 0x67, 0xfb, 0xdd, 0xcb, 0xcf, 0x77, 0xeb, 0xe8, 0xbc, 0x64, 0x3f, 0x63, 0x0d, 0x93,
    ];
    let mut data = vec![0u8; 167];
    data.extend_from_slice(b"OggS native stream");
    ctr::Ctr128BE::<aes::Aes128>::new(&key.into(), &iv.into()).apply_keystream(&mut data);
    let output = s.data_dir.join("spotify.part");
    tokio::fs::write(&output, b"OggS ").await.unwrap();
    let offset = 172;
    Mock::given(path("/audio"))
        .and(header("range", "bytes=172-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header(
                    "content-range",
                    format!("bytes {offset}-{}/{}", data.len() - 1, data.len()),
                )
                .set_body_bytes(data[offset..].to_vec()),
        )
        .mount(&server)
        .await;
    s.transfer(&format!("{}/audio", server.uri()), &output, 0, Some(key))
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"OggS native stream");
}

#[tokio::test]
async fn cancelling_native_work_drops_the_transfer_future() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (_dir, s) = service().await;
    let job = s.enqueue(request()).await.unwrap();
    s.next().await.unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(tokio::sync::Notify::new());
    let running = {
        let s = s.clone();
        let dropped = dropped.clone();
        let started = started.clone();
        tokio::spawn(async move {
            let stop = CancellationToken::new();
            s.run_work(job.id, &stop, async move {
                let _guard = Guard(dropped);
                started.notify_one();
                std::future::pending::<Result<()>>().await
            })
            .await
        })
    };
    started.notified().await;
    s.cancel(job.id).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn accepts_native_and_zotify_session_files_without_python() {
    let (_dir, s) = service().await;
    let native = s.data_dir.join("native.json");
    let zotify = s.data_dir.join("zotify.json");
    tokio::fs::write(
        &native,
        br#"{"username":"test","auth_type":1,"auth_data":"AQID"}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        &zotify,
        br#"{"username":"test","type":"AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS","credentials":"AQID"}"#,
    )
    .await
    .unwrap();
    let left = spotify::read_credentials(&native).await.unwrap();
    let right = spotify::read_credentials(&zotify).await.unwrap();
    assert_eq!(left, right);
    assert_eq!(left.auth_data, vec![1, 2, 3]);
    tokio::fs::write(&native, b"{}").await.unwrap();
    assert!(spotify::read_credentials(&native).await.is_err());
}

#[tokio::test]
async fn native_transfer_follows_chunked_ranges_and_skips_completed_parts() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, path},
    };
    let (_dir, s) = service().await;
    let server = MockServer::start().await;
    let output = s.data_dir.join("chunks.part");
    for (range, content_range, body) in [
        ("bytes=0-", "bytes 0-2/6", b"abc"),
        ("bytes=3-", "bytes 3-5/6", b"def"),
    ] {
        Mock::given(path("/chunks"))
            .and(header("range", range))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-range", content_range)
                    .set_body_bytes(body.to_vec()),
            )
            .mount(&server)
            .await;
    }
    Mock::given(path("/chunks"))
        .and(header("range", "bytes=6-"))
        .respond_with(ResponseTemplate::new(416).insert_header("content-range", "bytes */6"))
        .mount(&server)
        .await;
    let url = format!("{}/chunks", server.uri());
    s.transfer(&url, &output, 0, None).await.unwrap();
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"abcdef");
    s.transfer(&url, &output, 0, None).await.unwrap();
}

#[tokio::test]
async fn youtube_transfer_keeps_player_agent_and_uses_bounded_ranges() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, path},
    };
    let (_dir, s) = service().await;
    let server = MockServer::start().await;
    let output = s.data_dir.join("youtube.part");
    Mock::given(path("/youtube"))
        .and(header("user-agent", "native-player-agent"))
        .and(header("range", "bytes=0-8388607"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-2/3")
                .set_body_bytes(b"abc".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    s.transfer_youtube(
        &format!("{}/youtube", server.uri()),
        &output,
        0,
        "native-player-agent",
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"abc");
}
