//! MusicBrainz + Cover Art Archive client against a mocked API with authored fixture payloads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reelix_metadata::http::RetryPolicy;
use reelix_metadata::{
    ArtistCredit, MetaError, MetadataStore, MusicBrainz, MusicBrainzOptions, MusicMetadataProvider,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
const OK_COMPUTER_CD: &str = "0b6b4ba0-d36f-47bd-b4ea-6a5b91842d29";
const UA: &str = "Reelix/0.0-test ( https://github.com/mvanhare/Reelix )";

fn fixture(name: &str) -> String {
    let p = format!("{}/tests/fixtures/musicbrainz/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(p).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "application/json")
}

/// Each test uses its own mock server, hence its own (shared-per-URL) rate limiter.
fn options(server: &MockServer, min_interval: Duration) -> MusicBrainzOptions {
    MusicBrainzOptions {
        base_url: format!("{}/ws/2", server.uri()),
        caa_url: format!("{}/caa", server.uri()),
        user_agent: UA.into(),
        retry: RetryPolicy {
            max_retries: 2,
            base_backoff: Duration::from_millis(5),
            max_wait: Duration::from_millis(50),
        },
        min_interval,
        ..Default::default()
    }
}

fn client(server: &MockServer) -> MusicBrainz {
    MusicBrainz::new(reqwest::Client::new(), options(server, Duration::from_millis(1)))
}

fn credit(id: &str, name: &str, join: &str) -> ArtistCredit {
    ArtistCredit {
        artist_id: id.into(),
        name: name.into(),
        join_phrase: join.into(),
    }
}

#[tokio::test]
async fn search_artists_sends_user_agent_and_json_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ws/2/artist"))
        .and(query_param("query", r"AC\/DC"))
        .and(query_param("fmt", "json"))
        .and(header("user-agent", UA))
        .respond_with(ok("search_artist_radiohead.json"))
        .expect(1)
        .mount(&server)
        .await;
    let mb = client(&server);
    let hits = mb.search_artists("AC/DC").await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, RADIOHEAD);
    assert_eq!(hits[0].name, "Radiohead");
    assert_eq!(hits[0].sort_name, "Radiohead");
    assert_eq!(hits[0].artist_type.as_deref(), Some("Group"));
    assert_eq!(hits[0].country.as_deref(), Some("GB"));
    assert_eq!(hits[0].score, 100);
    assert_eq!(hits[0].disambiguation, None);
    assert_eq!(hits[1].score, 62, "string scores are accepted");
    assert_eq!(hits[1].disambiguation.as_deref(), Some("Polish tribute act"));
    assert_eq!(hits[1].country, None);
    // Cached (expect(1)); blank queries never hit the API.
    assert_eq!(mb.search_artists("AC/DC").await.unwrap(), hits);
    assert!(mb.search_artists(" ").await.unwrap().is_empty());
}

#[tokio::test]
async fn search_release_groups_with_artist_name_or_mbid() {
    let server = MockServer::start().await;
    Mock::given(path("/ws/2/release-group"))
        .and(query_param(
            "query",
            "releasegroup:(OK Computer) AND artist:(Radiohead)",
        ))
        .respond_with(ok("search_release_group_ok_computer.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .and(query_param(
            "query",
            format!("releasegroup:(OK Computer) AND arid:{RADIOHEAD}"),
        ))
        .respond_with(ok("search_release_group_ok_computer.json"))
        .expect(1)
        .mount(&server)
        .await;
    let mb = client(&server);
    let hits = mb
        .search_release_groups("OK Computer", Some("Radiohead"))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    let h = &hits[0];
    assert_eq!(h.id, OK_COMPUTER);
    assert_eq!(h.title, "OK Computer");
    assert_eq!(h.artist_id, RADIOHEAD);
    assert_eq!(h.artist_name, "Radiohead");
    assert_eq!(h.primary_type.as_deref(), Some("Album"));
    assert!(h.secondary_types.is_empty());
    assert_eq!(h.first_release_date.as_deref(), Some("1997-05-21"));
    assert_eq!(h.score, 100);
    let by_id = mb
        .search_release_groups("OK Computer", Some(&RADIOHEAD.to_uppercase()))
        .await
        .unwrap();
    assert_eq!(by_id, hits);
}

#[tokio::test]
async fn artist_includes_every_release_group() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/artist/{RADIOHEAD}")))
        .and(query_param("inc", "genres+url-rels"))
        .respond_with(ok("artist_radiohead.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .and(query_param("artist", RADIOHEAD))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ok("artist_release_groups_page0.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .and(query_param("artist", RADIOHEAD))
        .and(query_param("offset", "2"))
        .respond_with(ok("artist_release_groups_page1.json"))
        .expect(1)
        .mount(&server)
        .await;
    let a = client(&server).artist(RADIOHEAD).await.unwrap();
    assert_eq!(a.id, RADIOHEAD);
    assert_eq!(a.name, "Radiohead");
    assert_eq!(a.disambiguation, None);
    assert_eq!(a.artist_type.as_deref(), Some("Group"));
    assert_eq!(a.country.as_deref(), Some("GB"));
    assert_eq!(a.genres, ["alternative rock", "art rock", "electronic"]);
    assert_eq!(
        a.links,
        [
            "https://www.radiohead.com/",
            "https://www.wikidata.org/wiki/Q44190"
        ]
    );
    // Sorted by first release date.
    assert_eq!(
        a.release_groups
            .iter()
            .map(|g| g.title.as_str())
            .collect::<Vec<_>>(),
        ["Pablo Honey", "OK Computer", "I Might Be Wrong: Live Recordings"]
    );
    let ok_computer = &a.release_groups[1];
    assert_eq!(ok_computer.rating, Some(4.6));
    assert_eq!(ok_computer.genres, ["art rock", "alternative rock"]);
    assert_eq!(a.release_groups[0].rating, None);
    assert_eq!(a.release_groups[2].secondary_types, ["Live"]);
}

#[tokio::test]
async fn release_group_lookup() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/release-group/{OK_COMPUTER}")))
        .and(query_param("inc", "artist-credits+genres+ratings"))
        .respond_with(ok("release_group_ok_computer.json"))
        .mount(&server)
        .await;
    let g = client(&server).release_group(OK_COMPUTER).await.unwrap();
    assert_eq!(g.id, OK_COMPUTER);
    assert_eq!(g.title, "OK Computer");
    assert_eq!(g.disambiguation, None);
    assert_eq!(g.primary_type.as_deref(), Some("Album"));
    assert_eq!(g.first_release_date.as_deref(), Some("1997-05-21"));
    assert_eq!(g.artist_credit, [credit(RADIOHEAD, "Radiohead", "")]);
    assert_eq!(g.genres, ["art rock", "alternative rock"]);
    assert_eq!(g.rating, Some(4.6));
}

#[tokio::test]
async fn releases_with_media_tracks_and_durations() {
    let server = MockServer::start().await;
    Mock::given(path("/ws/2/release"))
        .and(query_param("release-group", OK_COMPUTER))
        .and(query_param("inc", "recordings+media+labels+artist-credits"))
        .and(query_param("offset", "0"))
        .respond_with(ok("releases_ok_computer.json"))
        .expect(1)
        .mount(&server)
        .await;
    let rels = client(&server).releases(OK_COMPUTER).await.unwrap();
    assert_eq!(rels.len(), 2);
    let cd = &rels[0];
    assert_eq!(cd.id, OK_COMPUTER_CD);
    assert_eq!(cd.release_group_id, OK_COMPUTER);
    assert_eq!(cd.status.as_deref(), Some("Official"));
    assert_eq!(cd.country.as_deref(), Some("GB"));
    assert_eq!(cd.date.as_deref(), Some("1997-05-21"));
    assert_eq!(cd.label.as_deref(), Some("Parlophone"));
    assert_eq!(cd.catalog_number.as_deref(), Some("NODATA 02"));
    assert_eq!(cd.barcode.as_deref(), Some("724385522925"));
    assert_eq!(cd.disambiguation, None);
    assert_eq!(cd.media.len(), 1);
    let m = &cd.media[0];
    assert_eq!(m.position, 1);
    assert_eq!(m.format.as_deref(), Some("CD"));
    assert_eq!(m.title, None);
    assert_eq!(m.tracks.len(), 3);
    let t = &m.tracks;
    assert_eq!(t[0].title, "Airbag");
    assert_eq!(t[0].number, "1");
    assert_eq!(t[0].position, 1);
    assert_eq!(t[0].recording_id, "f1c1d4c0-1111-4a2b-8c3d-000000000001");
    assert_eq!(t[0].length_ms, Some(284_733), "track length wins");
    assert_eq!(t[1].length_ms, Some(383_000), "falls back to the recording");
    assert_eq!(
        t[0].artist_credit,
        [credit(RADIOHEAD, "Radiohead", "")],
        "falls back to the release credit"
    );
    assert_eq!(
        t[2].artist_credit,
        [
            credit(RADIOHEAD, "Radiohead", " feat. "),
            credit("9e8d7c6b-0000-4000-8000-0000000000ef", "Guest", "")
        ]
    );

    let vinyl = &rels[1];
    assert_eq!(vinyl.disambiguation.as_deref(), Some("180 g vinyl"));
    assert_eq!(vinyl.barcode, None);
    assert_eq!(vinyl.label, None);
    assert_eq!(vinyl.catalog_number, None);
    assert_eq!(vinyl.media.len(), 2);
    assert_eq!(vinyl.media[1].title.as_deref(), Some("Side B"));
    assert_eq!(vinyl.media[1].format.as_deref(), Some("12\" Vinyl"));
    assert_eq!(vinyl.media[1].tracks[0].number, "B1");
    assert_eq!(vinyl.media[1].tracks[0].position, 1);
}

#[tokio::test]
async fn release_lookup_reads_its_group() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/release/{OK_COMPUTER_CD}")))
        .and(query_param(
            "inc",
            "recordings+media+labels+artist-credits+release-groups",
        ))
        .respond_with(ok("release_ok_computer_cd.json"))
        .mount(&server)
        .await;
    let r = client(&server).release(OK_COMPUTER_CD).await.unwrap();
    assert_eq!(r.release_group_id, OK_COMPUTER);
    assert_eq!(r.catalog_number.as_deref(), Some("7243 8 55229 2 5"));
    let lengths: Vec<_> = r.media[0].tracks.iter().map(|t| t.length_ms).collect();
    assert_eq!(lengths, [Some(284_733), Some(383_493)]);
}

#[tokio::test]
async fn cover_art_prefers_front_and_upgrades_to_https() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/caa/release-group/{OK_COMPUTER}")))
        .and(header("user-agent", UA))
        .respond_with(ok("caa_release_group_ok_computer.json"))
        .mount(&server)
        .await;
    Mock::given(path(format!("/caa/release/{OK_COMPUTER_CD}")))
        .respond_with(ok("caa_release_group_ok_computer.json"))
        .mount(&server)
        .await;
    let mb = client(&server);
    let art = mb.cover_art(OK_COMPUTER).await.unwrap().unwrap();
    let base = format!("https://coverartarchive.org/release/{OK_COMPUTER_CD}/829521797");
    assert_eq!(art.front.as_deref(), Some(format!("{base}.jpg").as_str()));
    assert_eq!(art.thumb_500.as_deref(), Some(format!("{base}-500.jpg").as_str()));
    assert_eq!(art.thumb_250.as_deref(), Some(format!("{base}-250.jpg").as_str()));
    assert_eq!(mb.release_cover_art(OK_COMPUTER_CD).await.unwrap(), Some(art));
}

#[tokio::test]
async fn missing_cover_art_is_none() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/caa/release-group/{OK_COMPUTER}")))
        .respond_with(ResponseTemplate::new(404).set_body_string("No cover art found"))
        .mount(&server)
        .await;
    Mock::given(path(format!("/caa/release-group/{RADIOHEAD}")))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let mb = client(&server);
    assert_eq!(mb.cover_art(OK_COMPUTER).await.unwrap(), None);
    assert!(matches!(
        mb.cover_art(RADIOHEAD).await,
        Err(MetaError::Status { status: 400, .. })
    ));
}

#[tokio::test]
async fn errors_and_invalid_mbids() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/release-group/{OK_COMPUTER}")))
        .respond_with(
            ResponseTemplate::new(404).set_body_raw(fixture("error_not_found.json"), "application/json"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/ws/2/artist"))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error": "Invalid query"}"#))
        .mount(&server)
        .await;
    let mb = client(&server);
    assert!(matches!(
        mb.release_group(OK_COMPUTER).await,
        Err(MetaError::NotFound)
    ));
    match mb.search_artists("x").await {
        Err(MetaError::Status { status, message }) => {
            assert_eq!(status, 400);
            assert_eq!(message, "Invalid query");
        }
        other => panic!("unexpected {other:?}"),
    }
    // Malformed ids never reach the network.
    assert!(matches!(mb.artist("radiohead").await, Err(MetaError::NotFound)));
    assert!(matches!(
        mb.releases("../../admin").await,
        Err(MetaError::NotFound)
    ));
    assert!(matches!(mb.cover_art("nope").await, Err(MetaError::NotFound)));
}

#[tokio::test]
async fn service_unavailable_is_retried() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/release-group/{OK_COMPUTER}")))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .expect(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path(format!("/ws/2/release-group/{OK_COMPUTER}")))
        .respond_with(ok("release_group_ok_computer.json"))
        .mount(&server)
        .await;
    let g = client(&server).release_group(OK_COMPUTER).await.unwrap();
    assert_eq!(g.title, "OK Computer");
}

#[tokio::test]
async fn persistent_503_reports_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/release-group/{OK_COMPUTER}")))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).release_group(OK_COMPUTER).await,
        Err(MetaError::RateLimited { .. })
    ));
}

#[tokio::test]
async fn requests_are_spaced_across_clients() {
    let server = MockServer::start().await;
    Mock::given(path("/ws/2/artist"))
        .respond_with(ok("search_artist_radiohead.json"))
        .expect(4)
        .mount(&server)
        .await;
    let interval = Duration::from_millis(150);
    // Two independent clients for the same server share one limiter.
    let a = MusicBrainz::new(reqwest::Client::new(), options(&server, interval));
    let b = MusicBrainz::new(reqwest::Client::new(), options(&server, interval));
    let start = Instant::now();
    let (r1, r2) = tokio::join!(a.search_artists("one"), b.search_artists("two"));
    r1.unwrap();
    r2.unwrap();
    a.search_artists("three").await.unwrap();
    b.search_artists("four").await.unwrap();
    assert!(
        start.elapsed() >= interval * 3,
        "4 requests need at least 3 intervals, took {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn cover_art_is_not_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/caa/release-group/{OK_COMPUTER}")))
        .respond_with(ok("caa_release_group_ok_computer.json"))
        .mount(&server)
        .await;
    Mock::given(path(format!("/caa/release/{OK_COMPUTER_CD}")))
        .respond_with(ok("caa_release_group_ok_computer.json"))
        .mount(&server)
        .await;
    let mb = MusicBrainz::new(reqwest::Client::new(), options(&server, Duration::from_secs(5)));
    let start = Instant::now();
    mb.cover_art(OK_COMPUTER).await.unwrap();
    mb.release_cover_art(OK_COMPUTER_CD).await.unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[derive(Default)]
struct MemStore {
    rows: Mutex<HashMap<(String, String), String>>,
}

#[async_trait]
impl MetadataStore for MemStore {
    async fn load(&self, provider: &str, key: &str) -> Option<String> {
        self.rows
            .lock()
            .unwrap()
            .get(&(provider.to_string(), key.to_string()))
            .cloned()
    }

    async fn save(&self, provider: &str, key: &str, payload: &str, ttl: Duration) {
        assert_eq!(ttl, Duration::from_secs(12 * 3600));
        self.rows
            .lock()
            .unwrap()
            .insert((provider.to_string(), key.to_string()), payload.to_string());
    }
}

#[tokio::test]
async fn persistent_store_survives_a_new_client() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/ws/2/release-group/{OK_COMPUTER}")))
        .respond_with(ok("release_group_ok_computer.json"))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemStore::default());
    let first = client(&server);
    first.set_store(store.clone());
    let g = first.release_group(OK_COMPUTER).await.unwrap();
    assert!(store.rows.lock().unwrap().keys().all(|(p, _)| p == "musicbrainz"));
    let second = client(&server);
    second.set_store(store);
    assert_eq!(second.release_group(OK_COMPUTER).await.unwrap(), g);
}
