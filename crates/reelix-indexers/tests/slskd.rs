mod common;

use std::time::Duration;

use common::{ctx, fixture};
use reelix_core::soulseek::SoulseekFile;
use reelix_core::{BookFormat, Protocol};
use reelix_indexers::slskd::{SlskdIndexer, folder_title, quality_tag};
use reelix_indexers::{Indexer, IndexerError, IndexerFlag, SearchQuery, build, categories, schemas};
use reelix_parser::{Quality, parse_book, parse_music};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEARCH: &str = r"^/api/v0/searches/[0-9a-f-]{36}$";

fn json_body(file: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(&format!("slskd/{file}")), "application/json")
}

fn indexer(server: &MockServer, extra: Value) -> SlskdIndexer {
    let mut s = json!({"base_url": server.uri(), "api_key": "key"});
    s.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    SlskdIndexer::from_settings(&s, &ctx(9, "slskd"))
        .unwrap()
        .with_timing(Duration::from_millis(20), Duration::from_millis(100))
}

/// POST → one running poll → complete → responses → DELETE.
async fn mount_search(server: &MockServer, text: &str) {
    Mock::given(method("POST"))
        .and(path("/api/v0/searches"))
        .and(header("X-API-Key", "key"))
        .and(body_partial_json(
            json!({"searchText": text, "searchTimeout": 100}),
        ))
        .respond_with(json_body("search_running.json"))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(SEARCH))
        .respond_with(json_body("search_running.json"))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(SEARCH))
        .respond_with(json_body("search_complete.json"))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v0/searches/[0-9a-f-]{36}/responses$"))
        .and(header("X-API-Key", "key"))
        .respond_with(json_body("responses.json"))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path_regex(SEARCH))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn album_search_groups_folders_and_filters_peers() {
    let server = MockServer::start().await;
    mount_search(&server, "Radiohead OK Computer").await;
    let ix = indexer(&server, json!({}));
    let found = ix
        .search(&SearchQuery::music("Radiohead", "OK Computer"))
        .await
        .unwrap();
    let titles: Vec<&str> = found.iter().map(|r| r.title.as_str()).collect();
    // Free slots first (fastest, shortest queue), the queue-200 peer is filtered out.
    assert_eq!(
        titles,
        [
            "Radiohead - 1997 - OK Computer [FLAC]",
            "Radiohead - Singles [MP3 320]",
            "Radiohead - OK Computer [V0] [MP3 V0]",
            "Radiohead - OK Computer (1997) [24-96] [FLAC 24-96]",
        ]
    );
    let flac = &found[0];
    assert_eq!(flac.protocol, Protocol::Soulseek);
    assert_eq!(
        flac.guid,
        r"slsk:vinylhoarder/@@abcde\Music\Radiohead\1997 - OK Computer"
    );
    assert_eq!((flac.size, flac.files), (Some(31457280 + 41943040), Some(2)));
    assert_eq!(flac.categories, [categories::AUDIO_LOSSLESS]);
    assert!(flac.publish_date.is_some() && flac.download_url.is_none());
    assert_eq!(flac.indexer_flags, [IndexerFlag::FreeUploadSlot]);
    let s = flac.soulseek.as_ref().unwrap();
    assert_eq!(
        (s.username.as_str(), s.queue_length, s.upload_speed),
        ("vinylhoarder", 0, 1048576)
    );
    // cover.jpg is not a wanted file type; remote names are kept verbatim for the enqueue.
    assert_eq!(
        s.files.iter().map(|f| f.filename.as_str()).collect::<Vec<_>>(),
        [
            r"@@abcde\Music\Radiohead\1997 - OK Computer\01 - Airbag.flac",
            r"@@abcde\Music\Radiohead\1997 - OK Computer\02 - Paranoid Android.flac"
        ]
    );
    assert_eq!(s.files[0].bit_depth, Some(16));
    let hires = found[3].soulseek.as_ref().unwrap();
    assert!(!hires.has_free_upload_slot && hires.queue_length == 12);
    assert!(found[3].indexer_flags.is_empty());

    // The titles score with the music parser's quality families.
    let q: Vec<Quality> = titles.iter().map(|t| parse_music(t).quality.quality).collect();
    assert_eq!(
        q,
        [
            Quality::Flac,
            Quality::Mp3_320,
            Quality::Mp3VbrV0,
            Quality::Flac24
        ]
    );
    let m = parse_music(titles[0]);
    assert_eq!(
        (m.artist.as_deref(), m.album.as_deref()),
        (Some("Radiohead"), Some("OK Computer"))
    );
}

#[tokio::test]
async fn peer_filters_and_result_cap() {
    let server = MockServer::start().await;
    mount_search(&server, "Radiohead OK Computer").await;
    let ix = indexer(
        &server,
        json!({"require_free_slot": true, "minimum_files": 2, "max_queue_length": 0, "max_results": 2,
               "min_upload_speed_kbs": 300}),
    );
    let found = ix
        .search(&SearchQuery::music("Radiohead", "OK Computer"))
        .await
        .unwrap();
    let users: Vec<&str> = found
        .iter()
        .map(|r| r.soulseek.as_ref().unwrap().username.as_str())
        .collect();
    // No queue limit lets busy_peer in; v0fan is too slow, audiophile has no free slot, the
    // one-file Singles folder is below the minimum.
    assert_eq!(users, ["vinylhoarder", "busy_peer"]);
    assert_eq!(found[1].title, "Radiohead - OK Computer [MP3 320]");
}

#[tokio::test]
async fn search_cancelled_at_deadline_returns_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/searches"))
        .respond_with(json_body("search_running.json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(SEARCH))
        .respond_with(json_body("search_running.json"))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(SEARCH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path_regex(SEARCH))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let ix = indexer(&server, json!({}));
    let q = SearchQuery::book("Frank Herbert", "Dune");
    assert!(ix.search(&q).await.unwrap().is_empty());
}

#[tokio::test]
async fn busy_search_slot_is_retried_and_auth_errors_surface() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/searches"))
        .respond_with(
            ResponseTemplate::new(429).set_body_string("Only one concurrent operation is permitted"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_search(&server, "Radiohead OK Computer").await;
    let ix = indexer(&server, json!({}));
    assert_eq!(
        ix.search(&SearchQuery::music("Radiohead", "OK Computer"))
            .await
            .unwrap()
            .len(),
        4
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let err = indexer(&server, json!({}))
        .search(&SearchQuery::music("A", "B"))
        .await
        .unwrap_err();
    assert!(matches!(err, IndexerError::Auth(_)), "{err:?}");
}

#[tokio::test]
async fn video_searches_never_reach_slskd() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let ix = indexer(&server, json!({}));
    let movie = SearchQuery {
        term: Some("The Matrix".into()),
        ..SearchQuery::movie()
    };
    let tv = SearchQuery {
        term: Some("Show".into()),
        season: Some(1),
        ..SearchQuery::tv()
    };
    for q in [movie, tv, SearchQuery::basic("Show 05")] {
        assert!(ix.search(&q).await.unwrap().is_empty());
    }
    assert!(ix.rss(&[]).await.unwrap().is_empty());
    // Radio: basic queries in the audio categories are searched.
    let mut radio = SearchQuery::basic("The Archers S20E03");
    radio.categories = categories::DEFAULT_RADIO.to_vec();
    assert_eq!(
        SlskdIndexer::search_text(&radio).as_deref(),
        Some("The Archers S20E03")
    );
    let mut book = SearchQuery::book("J.R.R. Tolkien", "The Hobbit: There and Back");
    book.book_format = Some(BookFormat::Ebook);
    assert_eq!(
        SlskdIndexer::search_text(&book).as_deref(),
        Some("J R R Tolkien The Hobbit There and Back")
    );
}

#[tokio::test]
async fn connection_test_checks_key_and_login() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v0/session"))
        .and(header("X-API-Key", "key"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v0/application"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": {"full": "0.26.2.0", "current": "0.26.2"},
            "server": {"address": "vps.slsknet.org:2271", "state": "Connected, LoggedIn", "isConnected": true, "isLoggedIn": true}
        })))
        .mount(&server)
        .await;
    let ix = build(
        "slskd",
        &json!({"base_url": server.uri(), "api_key": "key"}),
        &ctx(1, "slskd"),
    )
    .unwrap();
    assert_eq!(ix.protocol(), Protocol::Soulseek);
    ix.test().await.unwrap();
    let caps = ix.capabilities().await.unwrap();
    assert!(caps.audio_search.available && caps.book_search.available && !caps.movie_search.available);

    let bad = build(
        "slskd",
        &json!({"base_url": server.uri(), "api_key": "wrong"}),
        &ctx(1, "slskd"),
    )
    .unwrap();
    // No mock matches the wrong key: wiremock answers 404.
    assert!(bad.test().await.is_err());
    assert!(schemas().iter().any(|s| s.implementation == "slskd"));
    // The API key is required and secret.
    assert!(build("slskd", &json!({"base_url": server.uri()}), &ctx(1, "x")).is_err());
}

fn file(name: &str, br: Option<u32>, depth: Option<u32>, rate: Option<u32>, vbr: bool) -> SoulseekFile {
    SoulseekFile {
        filename: format!(r"@@root\Shared\{name}"),
        size: 1,
        bit_rate: br,
        bit_depth: depth,
        sample_rate: rate,
        length: None,
        is_variable_bit_rate: Some(vbr),
    }
}

#[test]
fn quality_tags_and_titles() {
    let cases = [
        (
            vec![file("a.flac", None, Some(24), Some(192000), false)],
            "FLAC 24-192",
        ),
        (vec![file("a.mp3", Some(256), None, None, false)], "MP3 256"),
        (vec![file("a.mp3", Some(190), None, None, true)], "MP3 V2"),
        (vec![file("a.m4a", Some(256), None, None, false)], "AAC 256"),
        (vec![file("a.opus", None, None, None, false)], "OPUS"),
        (vec![file("a.m4b", None, None, None, false)], "M4B"),
        (
            vec![
                file("a.epub", None, None, None, false),
                file("a.jpg", None, None, None, false),
                file("b.epub", None, None, None, false),
            ],
            "EPUB",
        ),
    ];
    for (files, want) in cases {
        assert_eq!(quality_tag(&files).as_deref(), Some(want));
    }
    assert_eq!(
        parse_music("Artist - Album [MP3 256]").quality.quality,
        Quality::Mp3_256
    );
    assert_eq!(
        parse_music("Artist - Album [AAC 256]").quality.quality,
        Quality::Aac256
    );
    assert_eq!(
        parse_music("Artist - Album [OPUS]").quality.quality,
        Quality::Opus
    );
    let b = parse_book("Frank Herbert - Dune [EPUB]", Some(BookFormat::Ebook));
    assert_eq!(b.quality.quality, Quality::Epub);
    let a = parse_book("Frank Herbert - Dune [M4B]", Some(BookFormat::Audiobook));
    assert_eq!(a.quality.quality, Quality::AudiobookM4b);

    assert_eq!(
        folder_title(r"@@abcde\Music\Radiohead\1997 - OK Computer"),
        "Radiohead - 1997 - OK Computer"
    );
    assert_eq!(
        folder_title(r"D:\Shared\Radiohead - OK Computer"),
        "Radiohead - OK Computer"
    );
    assert_eq!(folder_title(r"@@abcde\Music\OK Computer"), "OK Computer");
    assert_eq!(folder_title("books/Frank Herbert/Dune"), "Frank Herbert - Dune");
    assert_eq!(
        folder_title(r"Music\Pink Floyd\The Wall\CD2"),
        "Pink Floyd - The Wall"
    );
}
