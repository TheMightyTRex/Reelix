//! TMDB adapter against a mocked API with authored fixture payloads.

use std::time::{Duration, Instant};

use chrono::NaiveDate;
use reelix_core::MediaKind;
use reelix_metadata::http::RetryPolicy;
use reelix_metadata::{
    DiscoverFeed, EpisodeGroupSummary, ExternalSource, MetaError, MetadataProvider, MovieStatus, MultiHit,
    SeriesExternalId, SeriesStatus, TimeWindow, Tmdb, TmdbAuth, TmdbOptions, pick_aired_order,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MOVIE_APPEND: &str =
    "release_dates,alternative_titles,external_ids,credits,images,videos,keywords,recommendations,similar";
const TV_APPEND: &str =
    "external_ids,alternative_titles,content_ratings,credits,images,videos,keywords,recommendations,similar";

fn fixture(name: &str) -> serde_json::Value {
    let p = format!("{}/tests/fixtures/tmdb/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

fn opts(server: &MockServer) -> TmdbOptions {
    TmdbOptions {
        base_url: server.uri(),
        retry: RetryPolicy {
            max_retries: 2,
            base_backoff: Duration::from_millis(5),
            max_wait: Duration::from_secs(1),
        },
        ..Default::default()
    }
}

fn client(server: &MockServer) -> Tmdb {
    Tmdb::new(
        reqwest::Client::new(),
        TmdbAuth::ApiKey("k3y".into()),
        opts(server),
    )
}

fn date(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(y, m, d)
}

async fn mount_movie(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/movie/603"))
        .and(query_param("api_key", "k3y"))
        .and(query_param("language", "en-US"))
        .and(query_param("append_to_response", MOVIE_APPEND))
        .and(query_param("include_image_language", "en,null"))
        .respond_with(ok("movie_603.json"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn movie_details_are_fully_mapped() {
    let server = MockServer::start().await;
    mount_movie(&server).await;
    let m = client(&server).movie(603).await.unwrap();

    assert_eq!(m.title, "The Matrix");
    assert_eq!(m.sort_title, "matrix");
    assert_eq!(m.year, Some(1999));
    assert_eq!(m.imdb_id.as_deref(), Some("tt0133093"));
    assert_eq!(m.external_ids.wikidata.as_deref(), Some("Q83495"));
    assert_eq!(m.status, MovieStatus::Released);
    assert_eq!(m.runtime_minutes, Some(136));
    assert_eq!(m.certification.as_deref(), Some("R"));
    // US cinema + physical; no US digital release, so the earliest worldwide one.
    assert_eq!(m.release_dates.cinema, date(1999, 3, 31));
    assert_eq!(m.release_dates.physical, date(1999, 9, 21));
    assert_eq!(m.release_dates.digital, date(2008, 5, 15));
    assert_eq!(m.release_dates.all.len(), 6);
    assert_eq!(m.genres.len(), 2);
    assert_eq!(m.keywords.len(), 3);
    assert_eq!(m.studios.len(), 3);
    assert_eq!(m.studios[1].origin_country, None);
    assert_eq!(
        m.studios[0].logo_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w500/at4uYdwAAgNRKhZuuFX8ShKSybw.png")
    );
    let c = m.collection.as_ref().unwrap();
    assert_eq!((c.tmdb_id, c.name.as_str()), (2344, "The Matrix Collection"));
    assert_eq!(m.alternative_titles.len(), 3);
    assert_eq!(m.alternative_titles[2].kind.as_deref(), Some("sequel numbering"));

    assert_eq!(m.credits.cast[0].name, "Keanu Reeves", "cast sorted by order");
    assert_eq!(
        m.credits.cast[0].profile_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w185/4D0PpNI0kmP58hgrwGC3wCjxhnm.jpg")
    );
    assert_eq!(m.credits.cast[2].profile_url, None);
    assert!(
        m.credits
            .crew
            .iter()
            .any(|c| c.job == "Director" && c.name == "Lana Wachowski")
    );

    assert_eq!(
        m.images.poster.as_deref(),
        Some("https://image.tmdb.org/t/p/w500/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg")
    );
    assert_eq!(
        m.images.backdrop.as_deref(),
        Some("https://image.tmdb.org/t/p/w1280/fNG7i7RqMErkcqhohV2a6cV1Ehy.jpg")
    );
    assert_eq!(
        m.images.logo.as_deref(),
        Some("https://image.tmdb.org/t/p/w500/2ABqoQqlKUk1jOJvTk2Ql6AVQZb.png")
    );
    assert_eq!(m.images.posters.len(), 2);
    assert_eq!(
        m.images.posters[0].url,
        "https://image.tmdb.org/t/p/original/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg"
    );
    assert_eq!(m.images.posters[1].language, None);

    let trailer = m.videos.iter().find(|v| v.kind == "Trailer").unwrap();
    assert_eq!(
        trailer.url.as_deref(),
        Some("https://www.youtube.com/watch?v=vKQi3bBA1y8")
    );
    assert!(trailer.official);
    assert_eq!(m.videos[1].url.as_deref(), Some("https://vimeo.com/123456789"));

    assert_eq!(m.recommendations.len(), 1);
    assert_eq!(m.recommendations[0].title, "The Matrix Reloaded");
    assert_eq!(m.recommendations[0].year, Some(2003));
    assert_eq!(m.similar[0].kind, MediaKind::Movie);
    assert_eq!(m.similar[0].backdrop, None);

    // Round-trips through serde (domain types are persisted in the metadata cache).
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(
        serde_json::from_str::<reelix_metadata::MovieMetadata>(&json).unwrap(),
        m
    );
}

#[tokio::test]
async fn region_selects_certification_and_dates() {
    let server = MockServer::start().await;
    mount_movie(&server).await;
    let tmdb = Tmdb::new(
        reqwest::Client::new(),
        TmdbAuth::ApiKey("k3y".into()),
        TmdbOptions {
            region: Some("GB".into()),
            ..opts(&server)
        },
    );
    let m = tmdb.movie_details(603).await.unwrap();
    assert_eq!(m.certification.as_deref(), Some("15"));
    assert_eq!(m.release_dates.cinema, date(1999, 6, 11));
}

#[tokio::test]
async fn bearer_token_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/genre/movie/list"))
        .and(header("authorization", "Bearer v4-token"))
        .respond_with(ok("genre_movie_list.json"))
        .expect(1)
        .mount(&server)
        .await;
    let tmdb = Tmdb::new(
        reqwest::Client::new(),
        TmdbAuth::Bearer("v4-token".into()),
        opts(&server),
    );
    let genres = tmdb.genres(MediaKind::Movie).await.unwrap();
    assert_eq!(genres.len(), 9);
    assert_eq!(genres[7].name, "Science Fiction");
    let req = &server.received_requests().await.unwrap()[0];
    assert!(!req.url.query().unwrap_or_default().contains("api_key"));
}

#[tokio::test]
async fn responses_are_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/person/6384"))
        .respond_with(ok("person_6384.json"))
        .expect(1)
        .mount(&server)
        .await;
    let tmdb = client(&server);
    let p = tmdb.person(6384).await.unwrap();
    let again = tmdb.clone().person(6384).await.unwrap();
    assert_eq!(p, again);
    assert_eq!(p.name, "Keanu Reeves");
    assert_eq!(p.birthday, date(1964, 9, 2));
    assert_eq!(p.deathday, None);
    assert_eq!(p.imdb_id.as_deref(), Some("nm0000206"));
    assert_eq!(
        p.profile_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w185/4D0PpNI0kmP58hgrwGC3wCjxhnm.jpg")
    );
}

#[tokio::test]
async fn retries_429_honoring_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/person/6384"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/person/6384"))
        .respond_with(ok("person_6384.json"))
        .mount(&server)
        .await;
    let p = client(&server).person(6384).await.unwrap();
    assert_eq!(p.tmdb_id, 6384);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn persistent_429_becomes_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .mount(&server)
        .await;
    let err = client(&server).person(1).await.unwrap_err();
    assert!(
        matches!(
            err,
            MetaError::RateLimited {
                retry_after_secs: Some(0)
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "1 try + 2 retries"
    );
}

#[tokio::test]
async fn error_statuses_are_typed() {
    let server = MockServer::start().await;
    Mock::given(path("/movie/1"))
        .respond_with(ResponseTemplate::new(404).set_body_json(fixture("error_404.json")))
        .mount(&server)
        .await;
    Mock::given(path("/movie/2"))
        .respond_with(ResponseTemplate::new(401).set_body_json(fixture("error_401.json")))
        .mount(&server)
        .await;
    Mock::given(path("/movie/3"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad"))
        .mount(&server)
        .await;
    let tmdb = client(&server);
    assert!(matches!(tmdb.movie(1).await, Err(MetaError::NotFound)));
    match tmdb.movie(2).await {
        Err(MetaError::Unauthorized(msg)) => assert!(msg.starts_with("Invalid API key")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        tmdb.movie(3).await,
        Err(MetaError::Status { status: 400, .. })
    ));
}

async fn mount_tv(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/tv/1396"))
        .and(query_param("append_to_response", TV_APPEND))
        .respond_with(ok("tv_1396.json"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tv/1396"))
        .and(query_param(
            "append_to_response",
            "season/0,season/1,season/2,season/3",
        ))
        .respond_with(ok("tv_1396_seasons.json"))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn series_with_seasons_and_episodes() {
    let server = MockServer::start().await;
    mount_tv(&server).await;
    let s = client(&server)
        .series(SeriesExternalId::Tmdb(1396))
        .await
        .unwrap();

    assert_eq!(s.title, "Breaking Bad");
    assert_eq!((s.tmdb_id, s.tvdb_id), (Some(1396), Some(81189)));
    assert_eq!(s.imdb_id.as_deref(), Some("tt0903747"));
    assert_eq!(s.status, SeriesStatus::Ended);
    assert_eq!(s.show_type.as_deref(), Some("Scripted"));
    assert_eq!(s.network.as_deref(), Some("AMC"));
    assert_eq!(s.certification.as_deref(), Some("TV-MA"));
    assert_eq!(s.runtime_minutes, Some(45));
    assert_eq!(s.year, Some(2008));
    assert_eq!(s.last_air_date, date(2010, 6, 13));
    assert_eq!(s.next_air_date, None);
    assert_eq!(s.alternative_titles.len(), 2);
    assert_eq!(s.keywords.len(), 2);
    assert_eq!(s.credits.cast[0].name, "Bryan Cranston");
    assert_eq!(s.recommendations[0].kind, MediaKind::Series);
    assert_eq!(s.recommendations[0].year, Some(2015));

    let numbers: Vec<u32> = s.seasons.iter().map(|x| x.season_number).collect();
    assert_eq!(numbers, [0, 1, 2, 3]);
    // Season 3 was not in the batch response: summary only.
    assert_eq!(s.seasons[3].episode_count, 13);
    assert!(s.seasons[3].episodes.is_empty());

    let s1 = &s.seasons[1];
    assert_eq!(s1.episode_count, 3);
    assert_eq!(s1.episodes[0].title.as_deref(), Some("Pilot"), "episodes sorted");
    assert_eq!(s1.episodes[0].air_date, date(2008, 1, 20));
    assert_eq!(s1.episodes[0].runtime_minutes, Some(59));
    assert_eq!(
        s1.episodes[0].still.as_deref(),
        Some("https://image.tmdb.org/t/p/w300/ydlY3iPfeOAvu8gVqrxPoMvzNCn.jpg")
    );
    assert_eq!(s1.episodes[2].still, None);

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
            (1, 3, Some(3)),
            (2, 1, Some(4)),
            (2, 2, Some(5))
        ]
    );
    assert_eq!(s.seasons[2].episodes[1].finale.as_deref(), Some("mid_season"));
    assert_eq!(s.seasons[2].episodes[0].finale, None);
}

#[tokio::test]
async fn series_by_tvdb_id_resolves_through_find() {
    let server = MockServer::start().await;
    Mock::given(path("/find/81189"))
        .and(query_param("external_source", "tvdb_id"))
        .respond_with(ok("find_tvdb_81189.json"))
        .mount(&server)
        .await;
    mount_tv(&server).await;
    let s = client(&server)
        .series(SeriesExternalId::Tvdb(81189))
        .await
        .unwrap();
    assert_eq!(s.tmdb_id, Some(1396));
}

#[tokio::test]
async fn find_by_imdb() {
    let server = MockServer::start().await;
    Mock::given(path("/find/tt0133093"))
        .and(query_param("external_source", "imdb_id"))
        .respond_with(ok("find_imdb_tt0133093.json"))
        .mount(&server)
        .await;
    let r = client(&server)
        .find(ExternalSource::Imdb, "tt0133093")
        .await
        .unwrap();
    assert!(r.series.is_empty());
    assert_eq!(r.movies[0].tmdb_id, Some(603));
    assert_eq!(r.movies[0].imdb_id.as_deref(), Some("tt0133093"));
}

#[tokio::test]
async fn search_movie_series_and_multi() {
    let server = MockServer::start().await;
    Mock::given(path("/search/movie"))
        .and(query_param("query", "the matrix"))
        .and(query_param("year", "1999"))
        .and(query_param("include_adult", "false"))
        .respond_with(ok("search_movie_matrix.json"))
        .mount(&server)
        .await;
    Mock::given(path("/search/movie"))
        .and(query_param("query", "matrix"))
        .respond_with(ok("search_movie_matrix.json"))
        .mount(&server)
        .await;
    Mock::given(path("/search/multi"))
        .and(query_param("query", "breaking bad"))
        .and(query_param("page", "1"))
        .respond_with(ok("search_multi.json"))
        .mount(&server)
        .await;
    let tmdb = client(&server);

    let page = tmdb.search_movies("the matrix", Some(1999), 1).await.unwrap();
    assert_eq!(page.total_results, 2);
    assert_eq!(page.results[0].year, Some(1999));
    assert_eq!(page.results[1].release_date, None);
    assert_eq!(page.results[1].overview, None);

    let hits = tmdb.search("matrix", MediaKind::Movie).await.unwrap();
    assert_eq!(hits.len(), 2);

    let multi = tmdb.search_multi("breaking bad", 0).await.unwrap();
    assert_eq!(multi.results.len(), 3);
    assert!(
        matches!(&multi.results[0], MultiHit::Media(h) if h.kind == MediaKind::Series && h.title == "Breaking Bad")
    );
    assert!(
        matches!(&multi.results[1], MultiHit::Media(h) if h.kind == MediaKind::Movie && h.year == Some(2019))
    );
    assert!(matches!(&multi.results[2], MultiHit::Person(p) if p.name == "Bryan Cranston"));
}

#[tokio::test]
async fn discover_feeds_hit_the_right_endpoints() {
    let server = MockServer::start().await;
    Mock::given(path("/trending/all/week"))
        .respond_with(ok("trending_all_week.json"))
        .mount(&server)
        .await;
    Mock::given(path("/discover/tv"))
        .and(query_param("with_networks", "174"))
        .and(query_param("sort_by", "popularity.desc"))
        .and(query_param("page", "2"))
        .respond_with(ok("discover_tv_network_174.json"))
        .mount(&server)
        .await;
    Mock::given(path("/movie/upcoming"))
        .and(query_param("region", "DE"))
        .respond_with(ok("search_movie_matrix.json"))
        .mount(&server)
        .await;
    let tmdb = client(&server);

    let trending = tmdb
        .discover(
            DiscoverFeed::Trending {
                kind: None,
                window: TimeWindow::Week,
            },
            1,
        )
        .await
        .unwrap();
    assert_eq!(trending.total_pages, 500);
    assert_eq!(trending.results.len(), 2, "persons are dropped");
    assert_eq!(trending.results[0].kind, MediaKind::Movie);
    assert_eq!(trending.results[1].kind, MediaKind::Series);
    assert_eq!(trending.results[1].title, "Arcane");

    let net = tmdb
        .discover(DiscoverFeed::Network { network_id: 174 }, 2)
        .await
        .unwrap();
    assert_eq!(net.page, 2);
    assert!(net.results.iter().all(|h| h.kind == MediaKind::Series));
    assert_eq!(net.results[1].title, "The Walking Dead");

    let de = Tmdb::new(
        reqwest::Client::new(),
        TmdbAuth::ApiKey("k3y".into()),
        TmdbOptions {
            region: Some("DE".into()),
            ..opts(&server)
        },
    );
    assert_eq!(
        de.discover(DiscoverFeed::Upcoming, 1)
            .await
            .unwrap()
            .results
            .len(),
        2
    );
}

#[tokio::test]
async fn every_feed_maps_to_a_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok("search_movie_matrix.json"))
        .mount(&server)
        .await;
    let tmdb = client(&server);
    let feeds = [
        (
            DiscoverFeed::Trending {
                kind: Some(MediaKind::Series),
                window: TimeWindow::Day,
            },
            "/trending/tv/day",
        ),
        (
            DiscoverFeed::Popular {
                kind: MediaKind::Movie,
            },
            "/movie/popular",
        ),
        (
            DiscoverFeed::Popular {
                kind: MediaKind::Series,
            },
            "/tv/popular",
        ),
        (
            DiscoverFeed::TopRated {
                kind: MediaKind::Movie,
            },
            "/movie/top_rated",
        ),
        (
            DiscoverFeed::TopRated {
                kind: MediaKind::Series,
            },
            "/tv/top_rated",
        ),
        (DiscoverFeed::Upcoming, "/movie/upcoming"),
        (DiscoverFeed::NowPlaying, "/movie/now_playing"),
        (DiscoverFeed::OnTheAir, "/tv/on_the_air"),
        (DiscoverFeed::AiringToday, "/tv/airing_today"),
        (
            DiscoverFeed::Genre {
                kind: MediaKind::Movie,
                genre_id: 878,
            },
            "/discover/movie?with_genres=878",
        ),
        (
            DiscoverFeed::Studio { company_id: 420 },
            "/discover/movie?with_companies=420",
        ),
        (
            DiscoverFeed::Keyword {
                kind: MediaKind::Series,
                keyword_id: 210024,
            },
            "/discover/tv?with_keywords=210024",
        ),
    ];
    for (feed, _) in &feeds {
        tmdb.discover(feed.clone(), 1).await.unwrap();
    }
    let reqs = server.received_requests().await.unwrap();
    for ((_, expected), req) in feeds.iter().zip(&reqs) {
        let (p, q) = expected.split_once('?').unwrap_or((expected, ""));
        assert_eq!(req.url.path(), p);
        assert!(
            req.url.query().unwrap_or_default().contains(q),
            "{} lacks {q}",
            req.url
        );
    }
}

#[tokio::test]
async fn collection_parts_sorted_by_release() {
    let server = MockServer::start().await;
    Mock::given(path("/collection/2344"))
        .respond_with(ok("collection_2344.json"))
        .mount(&server)
        .await;
    let c = client(&server).collection(2344).await.unwrap();
    let ids: Vec<_> = c.parts.iter().map(|p| p.tmdb_id.unwrap()).collect();
    assert_eq!(ids, [603, 604, 605, 1000001]);
    assert!(
        c.images
            .poster
            .unwrap()
            .ends_with("/w500/bV9qTVHTVf0gkW0j7p7M0ILD4pG.jpg")
    );
}

#[tokio::test]
async fn client_side_rate_limit_applies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok("genre_movie_list.json"))
        .mount(&server)
        .await;
    let tmdb = Tmdb::new(
        reqwest::Client::new(),
        TmdbAuth::ApiKey("k".into()),
        TmdbOptions {
            rate_limit: 2,
            rate_period: Duration::from_millis(300),
            cache_entries: 0,
            ..opts(&server)
        },
    );
    let start = Instant::now();
    for _ in 0..3 {
        tmdb.genres(MediaKind::Series).await.unwrap();
    }
    assert!(
        start.elapsed() >= Duration::from_millis(300),
        "third request waited for the window"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

fn group(id: &str, name: &str, kind: u8, episodes: u32) -> EpisodeGroupSummary {
    EpisodeGroupSummary {
        id: id.into(),
        name: name.into(),
        kind,
        episode_count: episodes,
        group_count: 1,
    }
}

#[test]
fn the_aired_order_group_is_picked() {
    use reelix_metadata::episode_group_type::*;
    let dvd = group("a", "DVD Order", DVD, 100);
    let absolute = group("b", "Absolute", ABSOLUTE, 100);
    let tv = group("c", "Broadcast", TV, 50);
    let air = group("d", "Original air date", ORIGINAL_AIR_DATE, 40);
    let aired = group("e", "Aired Order", ORIGINAL_AIR_DATE, 30);
    let tvdb = group("f", "TVDB Order", TV, 20);
    let tvdb_dvd = group("g", "TVDB DVD", DVD, 200);
    assert_eq!(pick_aired_order(&[dvd.clone(), absolute.clone()]), None);
    assert_eq!(pick_aired_order(&[dvd.clone(), tv.clone()]), Some(&tv));
    assert_eq!(pick_aired_order(&[tv.clone(), air.clone()]), Some(&air));
    assert_eq!(
        pick_aired_order(&[air.clone(), aired.clone(), tv.clone()]),
        Some(&aired)
    );
    assert_eq!(
        pick_aired_order(&[aired.clone(), tvdb.clone(), tvdb_dvd.clone()]),
        Some(&tvdb)
    );
    // Ties go to the group with the most episodes; empty groups are never picked.
    let bigger = group("h", "Aired Order (full)", ORIGINAL_AIR_DATE, 31);
    assert_eq!(pick_aired_order(&[aired.clone(), bigger.clone()]), Some(&bigger));
    assert_eq!(
        pick_aired_order(&[group("i", "Aired", ORIGINAL_AIR_DATE, 0)]),
        None
    );
}

#[tokio::test]
async fn aired_order_reads_the_picked_episode_group() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/tv/1100/episode_groups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 1100,
            "results": [
                {"id": "5aa0000000000000000000d1", "name": "DVD Order", "type": 3, "episode_count": 13, "group_count": 1},
                {"id": "5aa0000000000000000000a1", "name": "Aired Order", "type": 1, "episode_count": 13, "group_count": 2,
                 "description": "", "network": null}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tv/episode_group/5aa0000000000000000000a1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "5aa0000000000000000000a1",
            "name": "Aired Order",
            "type": 1,
            "groups": [
                {"id": "x1", "name": "Season 3", "order": 3, "locked": true, "episodes": [
                    {"id": 1, "name": "Heart of Archness (1)", "season_number": 3, "episode_number": 1, "order": 0},
                    {"id": 2, "name": "Space Race (1)", "season_number": 4, "episode_number": 1, "order": 10}
                ]},
                {"id": "x0", "name": "Specials", "order": 0, "episodes": [
                    {"id": 3, "name": "Pilot", "season_number": 0, "episode_number": 2, "order": 0}
                ]}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tmdb = client(&server);
    let g = tmdb.aired_order(1100).await.unwrap().unwrap();
    assert_eq!(g.name, "Aired Order");
    assert_eq!(g.numbering(), [(3, 1, 3, 1), (3, 11, 4, 1), (0, 1, 0, 2)]);
    // Cached: a second refresh within the details lifetime sends no request.
    assert_eq!(tmdb.aired_order(1100).await.unwrap().unwrap(), g);
}

#[tokio::test]
async fn a_series_without_an_aired_order_group_has_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/tv/1200/episode_groups"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 1200, "results": []})),
        )
        .mount(&server)
        .await;
    assert_eq!(client(&server).aired_order(1200).await.unwrap(), None);
    assert!(client(&server).episode_group("../movie/1").await.is_err());
}
