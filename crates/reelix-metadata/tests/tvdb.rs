//! TVDB v4 adapter against a mocked API with authored fixture payloads.

use chrono::NaiveDate;
use reelix_core::MediaKind;
use reelix_metadata::{
    MetaError, MetadataProvider, SeriesExternalId, SeriesStatus, Tvdb, TvdbOptions, TvdbOrdering,
};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.fixture-token-1";

fn fixture(name: &str) -> serde_json::Value {
    let p = format!("{}/tests/fixtures/tvdb/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

fn client(server: &MockServer) -> Tvdb {
    Tvdb::new(
        reqwest::Client::new(),
        "tvdb-key".into(),
        None,
        TvdbOptions {
            base_url: server.uri(),
            ..Default::default()
        },
    )
}

async fn mount_login(server: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/login"))
        .and(body_json(serde_json::json!({"apikey": "tvdb-key"})))
        .respond_with(ok("login.json"))
        .expect(times)
        .mount(server)
        .await;
}

async fn mount_series(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/series/267440/extended"))
        .and(query_param("meta", "translations"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ok("series_267440_extended.json"))
        .mount(server)
        .await;
    for page in ["0", "1"] {
        Mock::given(method("GET"))
            .and(path("/series/267440/episodes/default/eng"))
            .and(query_param("page", page))
            .respond_with(ok(&format!("episodes_default_page{page}.json")))
            .expect(1)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn series_extended_with_paged_episodes() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
    mount_series(&server).await;
    let s = client(&server)
        .series(SeriesExternalId::Tvdb(267440))
        .await
        .unwrap();

    assert_eq!(s.title, "Attack on Titan", "English translation, not the alias");
    assert_eq!(s.original_title.as_deref(), Some("進撃の巨人"));
    assert!(
        s.overview
            .as_deref()
            .unwrap()
            .starts_with("Several hundred years ago")
    );
    assert_eq!((s.tvdb_id, s.tmdb_id), (Some(267440), Some(1429)));
    assert_eq!(s.imdb_id.as_deref(), Some("tt2560140"));
    assert_eq!(s.status, SeriesStatus::Ended);
    assert_eq!(s.network.as_deref(), Some("NHK General"));
    assert_eq!(s.year, Some(2013));
    assert_eq!(s.first_air_date, NaiveDate::from_ymd_opt(2013, 4, 7));
    assert_eq!(s.next_air_date, None);
    assert_eq!(s.runtime_minutes, Some(24));
    assert_eq!(s.genres.len(), 3);
    assert_eq!(s.alternative_titles.len(), 2);
    assert_eq!(
        s.images.poster.as_deref(),
        Some("https://artworks.thetvdb.com/banners/posters/267440-1.jpg")
    );
    assert_eq!(
        s.images.posters[0].url, "https://artworks.thetvdb.com/banners/posters/267440-7.jpg",
        "best score first"
    );
    assert!(s.images.backdrop.as_deref().unwrap().contains("/fanart/"));
    assert!(s.images.logo.as_deref().unwrap().contains("clearlogo"));

    let seasons: Vec<_> = s
        .seasons
        .iter()
        .map(|x| (x.season_number, x.episodes.len()))
        .collect();
    assert_eq!(seasons, [(0, 1), (1, 2), (2, 2)]);
    assert_eq!(
        s.seasons[1].tvdb_id,
        Some(501002),
        "official season type, not absolute"
    );
    assert_eq!(s.seasons[1].name.as_deref(), Some("Season 1"));
    assert_eq!(s.seasons[2].air_date, NaiveDate::from_ymd_opt(2017, 4, 1));
    let abs: Vec<_> = s
        .episodes()
        .map(|e| (e.season_number, e.episode_number, e.absolute_number))
        .collect();
    assert_eq!(
        abs,
        [
            (0, 1, None),
            (1, 1, Some(1)),
            (1, 2, Some(2)),
            (2, 1, Some(26)),
            (2, 2, Some(27))
        ]
    );
    assert_eq!(s.seasons[1].episodes[1].still, None);
    assert_eq!(s.seasons[2].episodes[1].finale.as_deref(), Some("midseason"));
}

#[tokio::test]
async fn absolute_ordering() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
    Mock::given(path("/series/267440/episodes/absolute/eng"))
        .respond_with(ok("episodes_absolute.json"))
        .mount(&server)
        .await;
    let eps = client(&server)
        .episodes(267440, TvdbOrdering::Absolute)
        .await
        .unwrap();
    let nums: Vec<_> = eps
        .iter()
        .map(|e| (e.season_number, e.episode_number, e.absolute_number))
        .collect();
    assert_eq!(nums, [(1, 1, Some(1)), (1, 2, Some(2)), (1, 26, Some(26))]);
}

#[tokio::test]
async fn token_is_reused_and_refreshed_on_401() {
    let server = MockServer::start().await;
    mount_login(&server, 2).await;
    Mock::given(path("/series/267440/extended"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"status": "failure", "message": "Unauthorized"})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_series(&server).await;
    let tvdb = client(&server);
    // First token gets rejected once -> second login; then the token is reused.
    let s = tvdb
        .series_with_ordering(267440, TvdbOrdering::Default)
        .await
        .unwrap();
    assert_eq!(s.tvdb_id, Some(267440));
    assert_eq!(tvdb.token().await.unwrap(), TOKEN);
}

#[tokio::test]
async fn failed_login_is_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(path("/login"))
        .respond_with(ResponseTemplate::new(401).set_body_json(fixture("login_failed.json")))
        .mount(&server)
        .await;
    let err = client(&server).search("x", MediaKind::Series).await.unwrap_err();
    assert!(
        matches!(err, MetaError::Unauthorized(ref m) if m == "Unauthorized"),
        "{err:?}"
    );
}

#[tokio::test]
async fn search_series_uses_translations() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
    Mock::given(path("/search"))
        .and(query_param("query", "attack on titan"))
        .and(query_param("type", "series"))
        .respond_with(ok("search_attack_on_titan.json"))
        .mount(&server)
        .await;
    let hits = client(&server)
        .search("attack on titan", MediaKind::Series)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "Attack on Titan");
    assert_eq!((hits[0].tvdb_id, hits[0].tmdb_id), (Some(267440), Some(1429)));
    assert_eq!(hits[0].year, Some(2013));
    assert!(hits[0].overview.as_deref().unwrap().starts_with("Several"));
    assert_eq!(hits[1].title, "Attack on Titan: Junior High");
    assert_eq!(hits[1].overview, None);
}

#[tokio::test]
async fn unsupported_operations() {
    let server = MockServer::start().await;
    let tvdb = client(&server);
    assert!(matches!(tvdb.movie(603).await, Err(MetaError::Unsupported(_))));
    assert!(matches!(
        tvdb.series(SeriesExternalId::Tmdb(1)).await,
        Err(MetaError::Unsupported(_))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}
