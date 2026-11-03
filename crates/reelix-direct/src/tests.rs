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

#[test]
fn ytdlp_progress_uses_total_or_estimate() {
    assert_eq!(ytdlp::parse_progress("5 10 NA"), Some(50.0));
    assert_eq!(ytdlp::parse_progress("5 NA 20"), Some(25.0));
    assert_eq!(ytdlp::parse_progress("5 NA NA"), None);
    assert_eq!(ytdlp::parse_progress("NA 10 NA"), None);
    assert!(ytdlp::video_id("oASNi8cLrGw") && !ytdlp::video_id("-x"));
}

/// A stand-in yt-dlp: lists a playlist, downloads `aaaaaaaaaaa` and rejects `bbbbbbbbbbb`.
#[cfg(unix)]
#[tokio::test]
async fn ytdlp_backend_lists_downloads_and_reports_errors() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, s) = service().await;
    let script = s.data_dir.join("fake-yt-dlp");
    let log = s.data_dir.join("args.log");
    tokio::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
case " $* " in *" --dump-single-json "*)
  echo 'WARNING: [youtube:tab] 1 unavailable video is hidden' >&2
  echo '{{"title":"List","entries":[{{"id":"aaaaaaaaaaa","title":"One"}},null,{{"id":"bad"}},{{"id":"aaaaaaaaaaa"}},{{"id":"bbbbbbbbbbb","title":"Two"}}]}}'
  exit 0;;
esac
prev=; for a in "$@"; do case "$prev" in -P) dir=$a;; -o) out=$a;; esac; prev=$a; done
id=${{a##*v=}}
if [ "$id" = bbbbbbbbbbb ]; then echo "ERROR: [youtube] $id: Sign in to confirm you're not a bot" >&2; exit 1; fi
file="$dir/$(echo "$out" | sed 's/%(ext)s/webm/')"
printf audio > "$file"
echo 'REELIX-PROGRESS 5 10 NA' >&2
printf '{{"filepath": "%s", "title": "Song/%s", "channel": "Chan - Topic"}}\n' "$file" "$id"
"#,
            log = log.display()
        ),
    )
    .await
    .unwrap();
    tokio::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .await
        .unwrap();
    let mut settings = s.settings().await.unwrap();
    settings.yt_dlp = script.to_string_lossy().into_owned();
    s.save_settings(settings).await.unwrap();
    let request = DirectRequest {
        url: "https://www.youtube.com/watch?v=aaaaaaaaaaa&list=PLlist".into(),
        playlist: true,
        ..request()
    };

    let preview = s.preview(request.clone()).await.unwrap();
    assert_eq!(preview.title, "List");
    assert_eq!(preview.items, ["One", "Two"]);

    s.enqueue(request).await.unwrap();
    let (job, settings) = s.next().await.unwrap().unwrap();
    let error = s
        .execute(&job, &settings, &CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "yt-dlp failed: [youtube] bbbbbbbbbbb: Sign in to confirm you're not a bot"
    );
    let folder = Path::new(&job.output_dir);
    // Audio is sorted into Artist/Album, using the playlist when there is no album.
    let output = folder.join("Chan/List/Song_aaaaaaaaaaa [aaaaaaaaaaa].webm");
    assert_eq!(tokio::fs::read(&output).await.unwrap(), b"audio");
    assert!(!folder.join(".aaaaaaaaaaa.audio.webm").exists());

    let args = tokio::fs::read_to_string(&log).await.unwrap();
    assert!(args.lines().all(|l| l.starts_with("--ignore-config ")));
    assert!(args.contains("-- https://www.youtube.com/playlist?list=PLlist"));
    assert!(args.contains("-f ba "));
    assert!(args.contains("-- https://www.youtube.com/watch?v=aaaaaaaaaaa"));

    // A retry skips the published video and fails at the same place.
    assert!(
        s.execute(&job, &settings, &CancellationToken::new())
            .await
            .is_err()
    );
    let args = tokio::fs::read_to_string(&log).await.unwrap();
    assert_eq!(args.matches("watch?v=aaaaaaaaaaa").count(), 1);
}

#[test]
fn spotify_login_parses_redirects_and_derives_pkce_challenge() {
    // Cross-checked with Python's hashlib + base64.urlsafe_b64encode.
    assert_eq!(
        spotify_login::challenge("dBjftJeZ4CVP-mJ92K9gGr6idmTw9sQKkhoSkpSB2dsJ5Os"),
        "AyBqLtPLE8iRaZZPvidswhB_UDCV4TBo7-gY9b1xDFg"
    );
    let pair = |code: &str, state: &str| (code.to_owned(), state.to_owned());
    assert_eq!(
        spotify_login::parse_redirect(" http://127.0.0.1:8898/login?code=abc&state=xyz ").unwrap(),
        pair("abc", "xyz")
    );
    assert_eq!(
        spotify_login::parse_redirect("?code=abc&state=xyz").unwrap(),
        pair("abc", "xyz")
    );
    assert!(
        spotify_login::parse_redirect("http://127.0.0.1:8898/login?error=access_denied&state=xyz")
            .unwrap_err()
            .to_string()
            .contains("access_denied")
    );
    assert!(spotify_login::parse_redirect("http://127.0.0.1:8898/login?state=xyz").is_err());
}

#[tokio::test]
async fn spotify_login_is_single_use_and_tied_to_the_newest_attempt() {
    let (_dir, s) = service().await;
    assert!(s.spotify_login_finish("?code=c&state=none").await.is_err());
    let first = s.spotify_login_start().await.unwrap();
    let second = s.spotify_login_start().await.unwrap();
    let state = |start: &SpotifyLoginStart| {
        url::Url::parse(&start.auth_url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned()
    };
    let auth = url::Url::parse(&second.auth_url).unwrap();
    let query: std::collections::HashMap<_, _> = auth.query_pairs().into_owned().collect();
    assert_eq!(auth.host_str(), Some("accounts.spotify.com"));
    assert_eq!(query["redirect_uri"], spotify_login::REDIRECT_URI);
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["scope"], "streaming user-read-private");
    assert_ne!(state(&first), state(&second));
    assert!(s.spotify_status().await.unwrap().pending);

    let stale = s
        .spotify_login_finish(&format!("?code=c&state={}", state(&first)))
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("different Spotify login"));
    assert!(s.spotify_status().await.unwrap().pending);

    // The loopback callback answers the browser and ignores unrelated requests.
    if second.automatic {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let get = |path: String| async move {
            let mut stream = tokio::net::TcpStream::connect("127.0.0.1:8898").await.unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        };
        assert!(get("/favicon.ico".into()).await.starts_with("HTTP/1.1 404"));
        let page = get(format!("/login?code=c&state={}", state(&first))).await;
        assert!(page.contains("Spotify login failed") && page.contains("different Spotify login"));
        assert!(s.spotify_status().await.unwrap().pending);
    }
    s.spotify_disconnect().await.unwrap();
    assert!(!s.spotify_status().await.unwrap().pending);
}

#[tokio::test]
async fn spotify_code_exchange_sends_pkce_verifier() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };
    let (_dir, s) = service().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("code_verifier=the-verifier"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A8898%2Flogin",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "token", "token_type": "Bearer", "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;
    let token = s
        .exchange_code(&server.uri(), "client", "the-code", "the-verifier")
        .await
        .unwrap();
    assert_eq!(token, "token");
    let error = s
        .exchange_code(&server.uri(), "client", "other", "the-verifier")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HTTP 404"));
}

#[tokio::test]
async fn spotify_disconnect_deletes_only_the_managed_session_file() {
    let (_dir, s) = service().await;
    let managed = s.data_dir.join("spotify-credentials.json");
    let manual = s.data_dir.join("manual.json");
    for (path, kept) in [(&manual, true), (&managed, false)] {
        tokio::fs::write(path, "{}").await.unwrap();
        let mut settings = s.settings().await.unwrap();
        settings.spotify_credentials = path.to_string_lossy().into_owned();
        s.save_settings(settings).await.unwrap();
        let status = s.spotify_disconnect().await.unwrap();
        assert!(!status.connected && status.credentials_path.is_empty());
        assert_eq!(path.exists(), kept);
    }
}
