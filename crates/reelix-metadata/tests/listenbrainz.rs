//! ListenBrainz client against a mocked API with fixture payloads shaped like real responses.

use std::time::Duration;

use reelix_metadata::http::RetryPolicy;
use reelix_metadata::{ListenBrainz, ListenBrainzOptions, MetaError, StatsRange};
use wiremock::matchers::{path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok(name: &str) -> ResponseTemplate {
    let p = format!(
        "{}/tests/fixtures/listenbrainz/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    ResponseTemplate::new(200).set_body_raw(std::fs::read_to_string(p).unwrap(), "application/json")
}

fn client(server: &MockServer) -> ListenBrainz {
    ListenBrainz::new(
        reqwest::Client::new(),
        ListenBrainzOptions {
            base_url: server.uri(),
            caa_url: "https://caa.test".into(),
            retry: RetryPolicy {
                max_retries: 1,
                base_backoff: Duration::from_millis(5),
                max_wait: Duration::from_millis(20),
            },
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn top_release_groups_maps_albums_and_covers() {
    let server = MockServer::start().await;
    Mock::given(path("/1/stats/sitewide/release-groups"))
        .and(query_param("range", "week"))
        .and(query_param("count", "80"))
        .respond_with(ok("sitewide_release_groups.json"))
        .mount(&server)
        .await;
    let albums = client(&server)
        .top_release_groups(StatsRange::Week, 40)
        .await
        .unwrap();
    // Entries without a release-group id are skipped, and ARIRANG (listed again under the same
    // id, and once more under another id with the same title and artist) appears once.
    assert_eq!(albums.len(), 2, "{albums:?}");
    assert_eq!(
        albums[0].release_group_mbid,
        "21159e3f-172e-43f6-aa7d-8e06a81fea49"
    );
    assert_eq!(albums[0].title, "ARIRANG");
    assert_eq!(albums[0].artist_name, "BTS");
    assert_eq!(
        albums[0].artist_mbid.as_deref(),
        Some("0d79fe8e-ba27-4859-bb8c-2f255f346853")
    );
    assert_eq!(
        albums[0].cover_url.as_deref(),
        Some("https://caa.test/release/909d000d-56e9-4b38-aa3f-008d38436ea2/44649468687-250.jpg")
    );
    assert_eq!(albums[1].title, "OK Computer");
    assert_eq!(albums[1].cover_url, None);
}

#[tokio::test]
async fn new_releases_keeps_albums_and_eps_of_popular_artists_most_popular_first() {
    let server = MockServer::start().await;
    Mock::given(path("/1/stats/sitewide/artists"))
        .and(query_param("range", "year"))
        .respond_with(ok("sitewide_artists.json"))
        .mount(&server)
        .await;
    Mock::given(path("/1/explore/fresh-releases/"))
        .and(query_param("days", "14"))
        .and(query_param("past", "true"))
        .and(query_param("future", "false"))
        .respond_with(ok("fresh_releases.json"))
        .mount(&server)
        .await;
    let albums = client(&server).new_releases(14, 40).await.unwrap();
    let got: Vec<_> = albums
        .iter()
        .map(|a| (a.artist_name.as_str(), a.title.as_str(), a.album_type.as_deref()))
        .collect();
    // Singles and artists outside the popular list are left out, the duplicate Dylan release
    // group appears once, and Queens of the Stone Age (more popular) comes before Bob Dylan,
    // newest release first.
    assert_eq!(
        got,
        [
            ("Queens of the Stone Age", "Perfecth", Some("Album")),
            ("Queens of the Stone Age", "Run Away From the Mob", Some("EP")),
            ("Bob Dylan", "The Rolling Thunder Revue 1976", Some("Album")),
        ]
    );
    assert_eq!(albums[0].cover_url, None);
    assert_eq!(
        albums[1].cover_url.as_deref(),
        Some("https://caa.test/release/rel-qotsa-ep/222-250.jpg")
    );
    assert_eq!(albums[2].year, Some(2026));
    assert_eq!(albums[2].release_group_mbid, "rg-dylan");

    let first = client(&server).new_releases(14, 1).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "Perfecth");
}

#[tokio::test]
async fn stats_not_calculated_yet_is_an_empty_chart() {
    let server = MockServer::start().await;
    Mock::given(path("/1/stats/sitewide/release-groups"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let albums = client(&server)
        .top_release_groups(StatsRange::Month, 10)
        .await
        .unwrap();
    assert!(albums.is_empty());
}

#[tokio::test]
async fn server_errors_are_reported() {
    let server = MockServer::start().await;
    Mock::given(path("/1/stats/sitewide/release-groups"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let err = client(&server)
        .top_release_groups(StatsRange::Week, 10)
        .await
        .unwrap_err();
    assert!(matches!(err, MetaError::Status { status: 503, .. }), "{err:?}");
}
