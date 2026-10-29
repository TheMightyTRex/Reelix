//! Adapter tests against wiremock with authored fixture payloads (no real network).

use reelix_core::MediaKind;
use reelix_lists::trakt::{DevicePoll, TraktDeviceAuth};
use reelix_lists::{BuildContext, ImportList, ListError, ListItem, build, schemas};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn json_fixture(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "application/json")
}

async fn setup() -> (MockServer, BuildContext) {
    let server = MockServer::start().await;
    let ctx = BuildContext::new(reqwest::Client::new()).with_all_base_urls(&server.uri());
    (server, ctx)
}

fn list(ctx: &BuildContext, imp: &str, settings: Value) -> Box<dyn ImportList> {
    build(imp, &settings, ctx).unwrap_or_else(|e| panic!("build {imp}: {e}"))
}

type Ids<'a> = (MediaKind, Option<u64>, Option<&'a str>, Option<u64>);

fn ids(items: &[ListItem]) -> Vec<Ids<'_>> {
    items
        .iter()
        .map(|i| (i.kind, i.tmdb_id, i.imdb_id.as_deref(), i.tvdb_id))
        .collect()
}

#[test]
fn schemas_are_unique_and_secrets_marked() {
    let all = schemas();
    let mut names: Vec<_> = all.iter().map(|s| s.implementation.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all.len());
    let tmdb = all.iter().find(|s| s.implementation == "tmdb").unwrap();
    assert_eq!(tmdb.secret_fields().collect::<Vec<_>>(), ["api_key"]);
    let trakt = all.iter().find(|s| s.implementation == "trakt").unwrap();
    assert!(trakt.secret_fields().any(|f| f == "access_token"));
}

#[tokio::test]
async fn invalid_settings_are_rejected() {
    let ctx = BuildContext::new(reqwest::Client::new());
    let cases = [
        ("tmdb", json!({"list_type": "collection", "api_key": "k"})),
        ("tmdb", json!({"list_type": "bogus", "api_key": "k"})),
        (
            "trakt",
            json!({"client_id": "c", "list_type": "user_list", "username": "bob"}),
        ),
        ("imdb", json!({"list_type": "list", "list_id": "ur1234567"})),
        ("mdblist", json!({"api_key": "k", "list_id": "???"})),
        (
            "jellyfin",
            json!({"url": "localhost:8096", "api_key": "k", "user": "alice"}),
        ),
        ("reelix", json!({"url": "http://x"})),
    ];
    for (imp, s) in cases {
        assert!(
            matches!(build(imp, &s, &ctx), Err(ListError::InvalidSettings(_))),
            "{imp} {s}"
        );
    }
}

#[tokio::test]
async fn tmdb_popular_pages_and_dedupes() {
    let (server, ctx) = setup().await;
    for (page, f) in [("1", "tmdb_popular_p1.json"), ("2", "tmdb_popular_p2.json")] {
        Mock::given(method("GET"))
            .and(path("/movie/popular"))
            .and(query_param("api_key", "0123456789abcdef0123456789abcdef"))
            .and(query_param("page", page))
            .and(query_param("language", "en-US"))
            .respond_with(json_fixture(f))
            .expect(1)
            .mount(&server)
            .await;
    }
    let l = list(
        &ctx,
        "tmdb",
        json!({"api_key": "0123456789abcdef0123456789abcdef", "list_type": "popular_movies"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        items.iter().map(|i| i.tmdb_id.unwrap()).collect::<Vec<_>>(),
        [603, 550, 27205]
    );
    assert_eq!(items[0].title, "The Matrix");
    assert_eq!(items[0].year, Some(1999));
}

#[tokio::test]
async fn tmdb_list_mixed_media_with_bearer_token() {
    let (server, ctx) = setup().await;
    let token = format!("eyJhbGciOiJIUzI1NiJ9.{}.sig", "a".repeat(60));
    Mock::given(method("GET"))
        .and(path("/list/8224"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .and(query_param_is_missing("api_key"))
        .respond_with(json_fixture("tmdb_list.json"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "tmdb",
        json!({"api_key": token, "list_type": "list", "list_id": "8224"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Series, Some(1399), None, None),
            (MediaKind::Movie, Some(157336), None, None)
        ]
    );
    assert_eq!(items[0].title, "Game of Thrones");
    assert_eq!(items[0].year, Some(2011));
}

#[tokio::test]
async fn tmdb_collection_person_company() {
    let (server, ctx) = setup().await;
    Mock::given(path("/collection/2344"))
        .respond_with(json_fixture("tmdb_collection.json"))
        .mount(&server)
        .await;
    Mock::given(path("/person/6384/combined_credits"))
        .respond_with(json_fixture("tmdb_person_credits.json"))
        .mount(&server)
        .await;
    Mock::given(path("/discover/movie"))
        .and(query_param("with_companies", "1"))
        .respond_with(json_fixture("tmdb_discover.json"))
        .mount(&server)
        .await;
    Mock::given(path("/discover/movie"))
        .and(query_param("with_keywords", "9715"))
        .respond_with(json_fixture("tmdb_discover.json"))
        .mount(&server)
        .await;

    let base = json!({"api_key": "0123456789abcdef0123456789abcdef"});
    let with = |extra: Value| {
        let mut v = base.clone();
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    let c = list(
        &ctx,
        "tmdb",
        with(json!({"list_type": "collection", "list_id": "2344"})),
    )
    .fetch()
    .await
    .unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!(c[2].year, None);

    let cast = list(
        &ctx,
        "tmdb",
        with(json!({"list_type": "person", "list_id": "6384"})),
    )
    .fetch()
    .await
    .unwrap();
    assert_eq!(
        cast.iter().map(|i| i.tmdb_id.unwrap()).collect::<Vec<_>>(),
        [603, 245891]
    );
    let both = list(
        &ctx,
        "tmdb",
        with(json!({"list_type": "person", "list_id": "6384", "person_credits": "both"})),
    )
    .fetch()
    .await
    .unwrap();
    assert_eq!(both.len(), 3);
    assert_eq!(both[2].kind, MediaKind::Series);
    let movies_only = list(
        &ctx,
        "tmdb",
        with(json!({"list_type": "person", "list_id": "6384", "person_credits": "both", "filter": "movies"})),
    )
    .fetch()
    .await
    .unwrap();
    assert_eq!(movies_only.len(), 2);

    let co = list(
        &ctx,
        "tmdb",
        with(json!({"list_type": "company", "list_id": "1"})),
    )
    .fetch()
    .await
    .unwrap();
    assert_eq!(co[0].tmdb_id, Some(11));
    let kw = list(
        &ctx,
        "tmdb",
        with(json!({"list_type": "keyword", "list_id": "9715"})),
    )
    .fetch()
    .await
    .unwrap();
    assert_eq!(kw.len(), 1);
}

#[tokio::test]
async fn tmdb_bad_key_is_unauthorized() {
    let (server, ctx) = setup().await;
    Mock::given(path("/movie/upcoming"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"status_code": 7, "status_message": "Invalid API key"})),
        )
        .mount(&server)
        .await;
    let l = list(&ctx, "tmdb", json!({"api_key": "bad", "list_type": "upcoming"}));
    assert!(matches!(l.test().await, Err(ListError::Unauthorized)));
}

#[tokio::test]
async fn per_instance_base_url_overrides_context() {
    let server = MockServer::start().await;
    Mock::given(path("/3/movie/now_playing"))
        .respond_with(json_fixture("tmdb_discover.json"))
        .mount(&server)
        .await;
    // Context points at an unroutable default; the setting wins.
    let ctx = BuildContext::new(reqwest::Client::new()).with_all_base_urls("http://127.0.0.1:1");
    let l = list(
        &ctx,
        "tmdb",
        json!({"api_key": "k", "list_type": "now_playing", "base_url": format!("{}/3/", server.uri())}),
    );
    assert_eq!(l.fetch().await.unwrap().len(), 1);
}

#[tokio::test]
async fn trakt_watchlist_paginates_with_headers() {
    let (server, ctx) = setup().await;
    for (page, f) in [
        ("1", "trakt_watchlist_movies.json"),
        ("2", "trakt_watchlist_movies_p2.json"),
    ] {
        Mock::given(method("GET"))
            .and(path("/users/me/watchlist/movies"))
            .and(header("trakt-api-version", "2"))
            .and(header("trakt-api-key", "cid"))
            .and(header("authorization", "Bearer tok"))
            .and(query_param("page", page))
            .respond_with(json_fixture(f).insert_header("x-pagination-page-count", "2"))
            .expect(1)
            .mount(&server)
            .await;
    }
    let l = list(
        &ctx,
        "trakt",
        json!({"client_id": "cid", "access_token": "tok", "list_type": "watchlist", "limit": 0}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Movie, Some(693134), Some("tt15239678"), None),
            (MediaKind::Movie, Some(329865), Some("tt2543164"), None),
            (MediaKind::Movie, Some(949), Some("tt0113277"), None),
        ]
    );
    assert_eq!(items[0].title, "Dune: Part Two");
}

#[tokio::test]
async fn trakt_user_list_trending_popular() {
    let (server, ctx) = setup().await;
    Mock::given(path("/users/alice/lists/favs/items"))
        .respond_with(json_fixture("trakt_list_items.json"))
        .mount(&server)
        .await;
    Mock::given(path("/shows/trending"))
        .respond_with(json_fixture("trakt_trending_shows.json"))
        .mount(&server)
        .await;
    Mock::given(path("/movies/popular"))
        .and(query_param("limit", "2"))
        .respond_with(json_fixture("trakt_popular_movies.json"))
        .mount(&server)
        .await;

    let ul = list(
        &ctx,
        "trakt",
        json!({"client_id": "c", "list_type": "user_list", "username": "alice", "list_slug": "favs"}),
    );
    let items = ul.fetch().await.unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        (items[0].kind, items[0].tvdb_id),
        (MediaKind::Series, Some(81189))
    );

    let tr = list(
        &ctx,
        "trakt",
        json!({"client_id": "c", "list_type": "trending", "media": "shows"}),
    );
    let items = tr.fetch().await.unwrap();
    assert!(items.iter().all(|i| i.kind == MediaKind::Series));
    assert_eq!(items[0].title, "Severance");

    // The fixture returns 3 rows; the limit truncates.
    let pop = list(
        &ctx,
        "trakt",
        json!({"client_id": "c", "list_type": "popular", "limit": 2}),
    );
    assert_eq!(pop.fetch().await.unwrap().len(), 2);
}

#[tokio::test]
async fn trakt_device_flow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device/code"))
        .and(body_partial_json(json!({"client_id": "cid"})))
        .respond_with(json_fixture("trakt_device_code.json"))
        .mount(&server)
        .await;
    // First poll is pending, the second succeeds.
    Mock::given(method("POST"))
        .and(path("/oauth/device/token"))
        .respond_with(ResponseTemplate::new(400))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/device/token"))
        .and(body_partial_json(
            json!({"client_id": "cid", "client_secret": "sec"}),
        ))
        .respond_with(json_fixture("trakt_token.json"))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_partial_json(
            json!({"grant_type": "refresh_token", "refresh_token": "r1"}),
        ))
        .respond_with(json_fixture("trakt_token.json"))
        .mount(&server)
        .await;

    let auth = TraktDeviceAuth::new(reqwest::Client::new(), &server.uri(), "cid", "sec");
    let code = auth.start().await.unwrap();
    assert_eq!(code.user_code, "5055CC52");
    let token = auth.wait_for_token(&code).await.unwrap();
    assert!(token.access_token.starts_with("dbaf97"));
    assert_eq!(token.expires_at().unwrap().timestamp(), 1_767_225_600 + 7_776_000);
    assert_eq!(
        auth.refresh("r1").await.unwrap().refresh_token,
        token.refresh_token
    );
}

#[tokio::test]
async fn trakt_device_poll_status_mapping() {
    let server = MockServer::start().await;
    let auth = TraktDeviceAuth::new(reqwest::Client::new(), &server.uri(), "c", "s");
    for (status, expected) in [
        (400, DevicePoll::Pending),
        (404, DevicePoll::InvalidCode),
        (409, DevicePoll::AlreadyUsed),
        (410, DevicePoll::Expired),
        (418, DevicePoll::Denied),
        (429, DevicePoll::SlowDown),
    ] {
        server.reset().await;
        Mock::given(path("/oauth/device/token"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        assert_eq!(auth.poll("x").await.unwrap(), expected, "{status}");
    }
}

#[tokio::test]
async fn imdb_csv_export() {
    let (server, ctx) = setup().await;
    Mock::given(path("/list/ls012345678/export"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(fixture("imdb_list_export.csv"), "text/csv"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "imdb",
        json!({"list_type": "list", "list_id": "https://www.imdb.com/list/ls012345678/"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| (i.kind, i.imdb_id.as_deref().unwrap(), i.year))
            .collect::<Vec<_>>(),
        [
            (MediaKind::Movie, "tt0133093", Some(1999)),
            (MediaKind::Series, "tt0903747", Some(2008)),
            (MediaKind::Series, "tt5491994", Some(2016)),
            (MediaKind::Movie, "tt0111161", Some(1994)),
        ]
    );
    assert_eq!(items[1].title, "Breaking Bad");
}

#[tokio::test]
async fn imdb_watchlist_and_charts() {
    let (server, ctx) = setup().await;
    Mock::given(path("/user/ur7654321/watchlist/export"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(fixture("imdb_list_export.csv"), "text/csv"))
        .mount(&server)
        .await;
    Mock::given(path("/chart/top/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("imdb_chart_top.html")))
        .mount(&server)
        .await;
    Mock::given(path("/chart/tvmeter/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("imdb_chart_tvmeter.html")))
        .mount(&server)
        .await;

    let wl = list(
        &ctx,
        "imdb",
        json!({"list_type": "watchlist", "list_id": "ur7654321", "filter": "series"}),
    );
    assert_eq!(wl.fetch().await.unwrap().len(), 2);

    let top = list(&ctx, "imdb", json!({"list_type": "top250"}))
        .fetch()
        .await
        .unwrap();
    assert_eq!(top.len(), 3);
    assert_eq!(
        (top[0].title.as_str(), top[0].year),
        ("The Shawshank Redemption", Some(1994))
    );
    assert_eq!(top[2].imdb_id.as_deref(), Some("tt0468569"));

    let tv = list(&ctx, "imdb", json!({"list_type": "tvmeter"}))
        .fetch()
        .await
        .unwrap();
    assert_eq!(
        ids(&tv),
        [
            (MediaKind::Series, None, Some("tt11280740"), None),
            (MediaKind::Series, None, Some("tt14452776"), None)
        ]
    );
}

#[tokio::test]
async fn mdblist_items_by_slug() {
    let (server, ctx) = setup().await;
    Mock::given(path("/lists/alice/top-scifi/items"))
        .and(query_param("apikey", "mk"))
        .and(query_param("offset", "0"))
        .respond_with(json_fixture("mdblist_items.json"))
        .expect(1)
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "mdblist",
        json!({"api_key": "mk", "list_id": "https://mdblist.com/lists/alice/top-scifi"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Movie, Some(438631), Some("tt1160419"), None),
            (MediaKind::Movie, Some(335984), Some("tt1856101"), None),
            (MediaKind::Series, Some(93405), Some("tt10919420"), Some(383275)),
        ]
    );
    assert_eq!(items[2].year, Some(2021));
}

#[tokio::test]
async fn mdblist_bad_key() {
    let (server, ctx) = setup().await;
    Mock::given(path("/lists/42/items"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let l = list(&ctx, "mdblist", json!({"api_key": "nope", "list_id": "42"}));
    assert!(matches!(l.test().await, Err(ListError::Unauthorized)));
}

#[tokio::test]
async fn plex_watchlist_with_guid_fallback() {
    let (server, ctx) = setup().await;
    Mock::given(path("/library/sections/watchlist/all"))
        .and(header("x-plex-token", "ptok"))
        .and(query_param("includeGuids", "1"))
        .respond_with(json_fixture("plex_watchlist.json"))
        .mount(&server)
        .await;
    Mock::given(path("/library/metadata/5d9c086c46115600200aa2fe"))
        .and(header("x-plex-token", "ptok"))
        .respond_with(json_fixture("plex_metadata_show.json"))
        .expect(1)
        .mount(&server)
        .await;
    let l = list(&ctx, "plex_watchlist", json!({"token": "ptok"}));
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Movie, Some(872585), Some("tt15398776"), Some(342816)),
            (MediaKind::Series, Some(100088), Some("tt3581920"), Some(392256)),
        ]
    );
    assert_eq!(items[1].title, "The Last of Us");
}

#[tokio::test]
async fn jellyfin_favourites_resolves_user_name() {
    let (server, ctx) = setup().await;
    let auth = r#"Token="jkey""#;
    Mock::given(path("/Users"))
        .respond_with(json_fixture("jellyfin_users.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/Users/6c1b52a0e4f94d9c8a5b2d7e9f013a44/Items"))
        .and(query_param("Filters", "IsFavorite"))
        .and(query_param("IncludeItemTypes", "Movie,Series"))
        .respond_with(json_fixture("jellyfin_favourites.json"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "jellyfin",
        json!({"url": format!("{}/", server.uri()), "api_key": "jkey", "user": "Alice"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Movie, Some(78), Some("tt0083658"), None),
            (MediaKind::Series, Some(1437), Some("tt0303461"), Some(78874)),
            (MediaKind::Movie, Some(16320), None, None),
        ]
    );
    let reqs = server.received_requests().await.unwrap();
    assert!(reqs.iter().all(|r| {
        r.headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .contains(auth)
    }));
}

#[tokio::test]
async fn emby_favourites_by_id_uses_token_header() {
    let (server, ctx) = setup().await;
    Mock::given(path("/Users/a1b2c3d4e5f60718293a4b5c6d7e8f90/Items"))
        .and(header("x-emby-token", "ekey"))
        .and(query_param("IncludeItemTypes", "Series"))
        .respond_with(json_fixture("jellyfin_favourites.json"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "emby",
        json!({"url": server.uri(), "api_key": "ekey", "user": "a1b2c3d4e5f60718293a4b5c6d7e8f90", "filter": "series"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Firefly");
    assert_eq!(l.implementation(), "emby");
}

#[tokio::test]
async fn url_list_mixed_elements() {
    let (server, ctx) = setup().await;
    Mock::given(path("/lists/mine.json"))
        .and(header("authorization", "Bearer bt"))
        .respond_with(json_fixture("url_list.json"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "url_list",
        json!({"url": format!("{}/lists/mine.json", server.uri()), "bearer_token": "bt"}),
    );
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Movie, Some(603), None, None),
            (MediaKind::Movie, Some(550), None, None),
            (MediaKind::Movie, None, Some("tt0816692"), None),
            (MediaKind::Movie, Some(27205), None, None),
            (MediaKind::Series, None, None, Some(81189)),
        ]
    );
    assert_eq!(items[4].year, Some(2008));
}

#[tokio::test]
async fn url_list_rejects_non_array() {
    let (server, ctx) = setup().await;
    Mock::given(path("/x.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"oops": 1})))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "url_list",
        json!({"url": format!("{}/x.json", server.uri())}),
    );
    assert!(matches!(l.fetch().await, Err(ListError::Parse(_))));
}

#[tokio::test]
async fn reelix_instance_pages_movies_and_series() {
    let (server, ctx) = setup().await;
    for (page, f) in [("1", "reelix_movies_p1.json"), ("2", "reelix_movies_p2.json")] {
        Mock::given(path("/api/v1/movies"))
            .and(header("x-api-key", "rlx_key"))
            .and(query_param("page", page))
            .respond_with(json_fixture(f))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(path("/api/v1/series"))
        .respond_with(json_fixture("reelix_series.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/system/status"))
        .and(header("x-api-key", "rlx_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.1.0"})))
        .mount(&server)
        .await;
    let l = list(&ctx, "reelix", json!({"url": server.uri(), "api_key": "rlx_key"}));
    l.test().await.unwrap();
    let items = l.fetch().await.unwrap();
    assert_eq!(
        ids(&items),
        [
            (MediaKind::Movie, Some(348), Some("tt0078748"), None),
            (MediaKind::Movie, Some(679), Some("tt0090605"), None),
            (MediaKind::Movie, Some(8077), None, None),
            (MediaKind::Series, Some(63639), Some("tt3230854"), Some(280619)),
        ]
    );
}

#[tokio::test]
async fn rate_limit_reports_retry_after() {
    let (server, ctx) = setup().await;
    Mock::given(path("/movies/anticipated"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "trakt",
        json!({"client_id": "c", "list_type": "anticipated"}),
    );
    match l.fetch().await {
        Err(ListError::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)))
        }
        other => panic!("{other:?}"),
    }
}
