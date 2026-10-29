mod common;

use chrono::{Duration, Utc};
use common::{ctx, fixture, store};
use reelix_core::provider::FieldKind;
use reelix_indexers::{
    DefinitionStore, IndexerError, IndexerFlag, SearchParam, SearchQuery, all_schemas, build,
};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn html(rel: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(rel), "text/html; charset=utf-8")
}

fn redirect(to: &str) -> ResponseTemplate {
    ResponseTemplate::new(302).insert_header("Location", to)
}

#[test]
fn loads_definitions_and_builds_schemas() {
    let s = store();
    let ids: Vec<String> = s.list().iter().map(|d| d.id.clone()).collect();
    assert_eq!(ids, ["kestrel", "lumentracker", "owlbarn"]);
    let schemas = all_schemas(&s);
    let lumen = schemas
        .iter()
        .find(|s| s.implementation == "definition:lumentracker")
        .unwrap();
    let names: Vec<&str> = lumen.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        ["base_url", "username", "password", "freeleech", "sort"],
        "info settings are hidden"
    );
    assert_eq!(lumen.secret_fields().collect::<Vec<_>>(), ["password"]);
    assert!(lumen.fields[1].required);
    assert_eq!(lumen.fields[3].kind, FieldKind::Bool);
    assert!(matches!(&lumen.fields[4].kind, FieldKind::Select { options } if options.len() == 3));
    assert_eq!(lumen.fields[0].default, Some(json!("https://lumen.invalid/")));
}

#[test]
fn broken_definitions_are_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("good.yml"), fixture("definitions/kestrel.yml")).unwrap();
    std::fs::write(dir.path().join("syntax.yml"), "id: [unclosed").unwrap();
    let bad_sel =
        fixture("definitions/lumentracker.yml").replace("table#torrents > tbody > tr.torrent", "tr[[[");
    std::fs::write(dir.path().join("selector.yaml"), bad_sel).unwrap();
    let no_dl = "id: x\nname: X\nlinks: [https://x.invalid/]\nsearch:\n  path: s\n  rows: {selector: tr}\n  fields:\n    title: {selector: a}\n";
    std::fs::write(dir.path().join("nodl.yml"), no_dl).unwrap();
    std::fs::write(dir.path().join("dup.yml"), fixture("definitions/kestrel.yml")).unwrap();
    std::fs::write(dir.path().join("README.txt"), "ignored").unwrap();
    let (s, errors) = DefinitionStore::load_dir(dir.path()).unwrap();
    assert_eq!(s.list().len(), 1);
    assert_eq!(errors.len(), 4, "{errors:?}");
    assert!(errors.iter().any(|e| e.message.contains("bad selector")));
    assert!(errors.iter().any(|e| e.message.contains("duplicate")));
    assert!(errors.iter().any(|e| e.message.contains("download")));
}

async fn lumen_login(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/login.php"))
        .and(body_string_contains("username=tester"))
        .and(body_string_contains("password=p%40ss"))
        .and(body_string_contains("keeplogged=1"))
        .respond_with(
            redirect("/index.php")
                .append_header("Set-Cookie", "uid=7; path=/")
                .append_header("Set-Cookie", "pass=h4sh; HttpOnly"),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/index.php"))
        .and(header("cookie", "uid=7; pass=h4sh"))
        .respond_with(html("html/lumen_index.html"))
        .mount(server)
        .await;
}

fn lumen(server: &MockServer, extra: serde_json::Value) -> Box<dyn reelix_indexers::Indexer> {
    let mut settings = json!({"base_url": server.uri(), "username": "tester", "password": "p@ss"});
    settings
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    build("definition:lumentracker", &settings, &ctx(9, "Lumen")).unwrap()
}

#[tokio::test]
async fn html_definition_login_and_search() {
    let server = MockServer::start().await;
    lumen_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/browse.php"))
        .and(header("cookie", "uid=7; pass=h4sh"))
        .and(query_param("search", "Harbor Lights 2025"))
        .and(query_param("searchin", "title"))
        .and(query_param("sort", "seeders"))
        .and(query_param("fl", "1"))
        .and(query_param("c10", "1"))
        .respond_with(html("html/lumen_browse.html"))
        .expect(1)
        .mount(&server)
        .await;

    let idx = lumen(&server, json!({"sort": "seeders", "freeleech": true}));
    let caps = idx.capabilities().await.unwrap();
    assert!(caps.movie_search.supports(SearchParam::ImdbId));
    assert!(caps.category_ids().contains(&2045));

    let q = SearchQuery {
        term: Some("Harbor Lights: 2025".into()),
        categories: vec![2040],
        ..SearchQuery::default()
    };
    let r = idx.search(&q).await.unwrap();
    assert_eq!(r.len(), 2, "row without a download link is skipped");
    let a = &r[0];
    assert_eq!(a.title, "Harbor Lights 2025 1080p BluRay DTS-HD MA 5.1 x264-GLOW");
    assert_eq!(
        a.info_url.as_deref(),
        Some(format!("{}/details.php?id=5501", server.uri()).as_str())
    );
    assert_eq!(a.guid, a.info_url.clone().unwrap());
    assert_eq!(
        a.download_url.as_deref(),
        Some(format!("{}/download.php?id=5501&passkey=pk1", server.uri()).as_str())
    );
    assert_eq!(a.categories, [2040]);
    assert_eq!(a.size, Some(15_247_133_901));
    assert_eq!((a.seeders, a.peers, a.grabs), (Some(311), Some(320), Some(1204)));
    assert_eq!(a.imdb_id.as_deref(), Some("tt4455667"));
    assert_eq!(a.publish_date.unwrap().to_rfc3339(), "2026-04-28T07:15:00+00:00");
    assert_eq!(a.download_volume_factor, 0.0);
    assert_eq!((a.minimum_ratio, a.minimum_seed_time), (Some(1.0), Some(259_200)));
    assert!(a.indexer_flags.contains(&IndexerFlag::Freeleech));
    let b = &r[1];
    assert_eq!(b.categories, [2045]);
    assert_eq!(b.upload_volume_factor, 2.0);
    assert_eq!(b.indexer_flags, [IndexerFlag::DoubleUpload]);
    assert!(b.imdb_id.is_none());
}

#[tokio::test]
async fn html_definition_imdb_search_and_session_expiry() {
    let server = MockServer::start().await;
    lumen_login(&server).await;
    // First search hits an expired session and is bounced to the login page.
    Mock::given(path("/browse.php"))
        .respond_with(redirect("/login.php?returnto=browse.php"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/login.php"))
        .respond_with(html("html/lumen_login_failed.html"))
        .mount(&server)
        .await;
    Mock::given(path("/browse.php"))
        .and(query_param("search", "tt4455667"))
        .and(query_param("searchin", "imdb"))
        .respond_with(html("html/lumen_browse.html"))
        .mount(&server)
        .await;
    let idx = lumen(&server, json!({}));
    let q = SearchQuery {
        imdb_id: Some("4455667".into()),
        ..SearchQuery::movie()
    };
    assert_eq!(idx.search(&q).await.unwrap().len(), 2);
}

#[tokio::test]
async fn html_definition_login_and_search_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login.php"))
        .and(body_string_contains("username=wrong"))
        .respond_with(html("html/lumen_login_failed.html"))
        .mount(&server)
        .await;
    lumen_login(&server).await;
    Mock::given(path("/browse.php"))
        .respond_with(html("html/lumen_search_error.html"))
        .mount(&server)
        .await;

    let bad = lumen(&server, json!({"username": "wrong"}));
    match bad.test().await {
        Err(IndexerError::Auth(m)) => assert!(m.contains("incorrect"), "{m}"),
        other => panic!("{other:?}"),
    }
    let ok = lumen(&server, json!({}));
    match ok.search(&SearchQuery::basic("x")).await {
        Err(IndexerError::Api { description, .. }) => assert!(description.contains("maintenance")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        build("definition:lumentracker", &json!({"base_url": server.uri()}), &ctx(1, "x")),
        Err(IndexerError::Config(m)) if m.contains("Username")
    ));
}

#[tokio::test]
async fn json_definition_search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents"))
        .and(query_param("imdb", "9988776"))
        .and(query_param("query", "Northbound Freight"))
        .and(query_param("type", "movies"))
        .and(query_param("limit", "100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("kestrel_search.json"), "application/json"),
        )
        .mount(&server)
        .await;
    let idx = build(
        "definition:kestrel",
        &json!({"base_url": server.uri()}),
        &ctx(4, "Kestrel"),
    )
    .unwrap();
    let q = SearchQuery {
        term: Some("Northbound Freight".into()),
        imdb_id: Some("tt9988776".into()),
        categories: vec![2000],
        ..SearchQuery::movie()
    };
    let r = idx.search(&q).await.unwrap();
    assert_eq!(r.len(), 2);
    let a = &r[0];
    assert_eq!(
        a.info_url.as_deref(),
        Some(format!("{}/torrent/90211", server.uri()).as_str())
    );
    assert_eq!(
        a.info_hash.as_deref(),
        Some("c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00")
    );
    assert!(
        a.magnet_url
            .as_deref()
            .unwrap()
            .starts_with("magnet:?xt=urn:btih:C0FFEE00")
    );
    assert!(a.download_url.is_none());
    assert_eq!(a.categories, [2000]);
    assert_eq!(
        (a.size, a.seeders, a.peers, a.grabs, a.files),
        (Some(6_012_954_214), Some(820), Some(861), Some(15022), Some(3))
    );
    assert_eq!(a.imdb_id.as_deref(), Some("tt9988776"));
    assert_eq!(a.tmdb_id, Some(501_234));
    assert_eq!(a.publish_date.unwrap().to_rfc3339(), "2026-04-25T19:02:11+00:00");
    assert!(a.indexer_flags.contains(&IndexerFlag::Freeleech));
    let b = &r[1];
    assert_eq!(b.publish_date.unwrap().timestamp(), 1_777_200_000);
    assert!(b.imdb_id.is_none() && b.grabs.is_none());
    assert!(matches!(
        idx.download(b.magnet_url.as_deref().unwrap()).await,
        Err(IndexerError::MagnetRedirect(_))
    ));
}

async fn owl_login(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/account/login"))
        .respond_with(html("html/owl_login.html").insert_header("Set-Cookie", "session=pre; Path=/"))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/account/login/submit"))
        .and(header("cookie", "session=pre"))
        .and(body_string_contains("_token=csrf-7f3a"))
        .and(body_string_contains("remember=1"))
        .and(body_string_contains("user=owl"))
        .and(body_string_contains("pass=hoot"))
        .respond_with(redirect("/account").insert_header("Set-Cookie", "session=authed; Path=/; HttpOnly"))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/account/login/submit"))
        .and(body_string_contains("pass=wrong"))
        .respond_with(html("html/owl_login_failed.html"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/account"))
        .and(header("cookie", "session=authed"))
        .respond_with(html("html/owl_account.html"))
        .mount(server)
        .await;
}

fn owl(server: &MockServer, pass: &str) -> Box<dyn reelix_indexers::Indexer> {
    build(
        "definition:owlbarn",
        &json!({"base_url": server.uri(), "username": "owl", "password": pass}),
        &ctx(5, "Owl"),
    )
    .unwrap()
}

#[tokio::test]
async fn form_login_multi_path_search_and_download() {
    let server = MockServer::start().await;
    owl_login(&server).await;
    Mock::given(path("/films/search"))
        .and(query_param("q", "quiet orchard"))
        .and(header("cookie", "session=authed"))
        .respond_with(html("html/owl_films.html"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/series/search"))
        .and(header("cookie", "session=authed"))
        .respond_with(html("html/owl_series.html"))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/files/301"))
        .respond_with(html("html/owl_details.html"))
        .mount(&server)
        .await;
    Mock::given(path("/get/301/quiet-orchard.torrent"))
        .and(header("cookie", "session=authed"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"d4:infod4:name3:abcee".to_vec()))
        .mount(&server)
        .await;

    let idx = owl(&server, "hoot");
    let all = idx.search(&SearchQuery::basic("Quiet Orchard")).await.unwrap();
    assert_eq!(all.len(), 3, "both paths searched when no category is given");
    let a = &all[0];
    assert_eq!(a.title, "Quiet Orchard 2023 720p WEB-DL AAC2.0 H.264-BRAMBLE");
    assert_eq!(a.categories, [2000]);
    assert_eq!(a.size, Some(2_576_980_378));
    assert_eq!((a.seeders, a.peers), (Some(75), Some(78)));
    assert_eq!(
        a.info_hash.as_deref(),
        Some("1111222233334444555566667777888899990000")
    );
    assert!(a.download_url.as_deref().unwrap().ends_with("/files/301"));
    let age = Utc::now() - a.publish_date.unwrap();
    assert!((age - Duration::hours(3)).num_seconds().abs() < 60, "{age}");
    assert!(all[1].download_url.is_none() && all[1].magnet_url.is_some());
    assert_eq!(all[2].categories, [5000]);

    let tv = SearchQuery {
        term: Some("Quiet Orchard".into()),
        season: Some(1),
        episode: Some("1".into()),
        categories: vec![5000],
        ..SearchQuery::tv()
    };
    let r = idx.search(&tv).await.unwrap();
    assert_eq!(r.len(), 1, "only the series path matches TV categories");

    let bytes = idx.download("/files/301").await.unwrap();
    assert!(bytes.starts_with(b"d4:info"));
}

#[tokio::test]
async fn form_login_error_message() {
    let server = MockServer::start().await;
    owl_login(&server).await;
    match owl(&server, "wrong").test().await {
        Err(IndexerError::Auth(m)) => assert_eq!(m, "Invalid credentials"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn cloudflare_challenge_solved_via_flaresolverr() {
    let site = MockServer::start().await;
    let fs = MockServer::start().await;
    let json_body = fixture("kestrel_search.json");
    // With clearance cookie + matching UA the site answers directly.
    Mock::given(path("/api/v2/torrents"))
        .and(header_regex("cookie", "cf_clearance=ok123"))
        .and(header("user-agent", "Mozilla/5.0 (FlareTest)"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(json_body.clone(), "application/json"))
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/api/v2/torrents"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Server", "cloudflare")
                .set_body_raw(fixture("html/cloudflare_challenge.html"), "text/html"),
        )
        .mount(&site)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1"))
        .and(body_string_contains("\"cmd\":\"request.get\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "message": "Challenge solved!",
            "solution": {
                "url": format!("{}/api/v2/torrents?query=x", site.uri()),
                "status": 200,
                "response": json_body,
                "cookies": [{"name": "cf_clearance", "value": "ok123", "domain": "127.0.0.1"}],
                "userAgent": "Mozilla/5.0 (FlareTest)"
            }
        })))
        .expect(1)
        .mount(&fs)
        .await;

    let mut c = ctx(1, "k");
    let no_fs = build("definition:kestrel", &json!({"base_url": site.uri()}), &c).unwrap();
    assert!(matches!(
        no_fs.search(&SearchQuery::basic("x")).await,
        Err(IndexerError::Cloudflare(_))
    ));

    c.flaresolverr = Some(url::Url::parse(&format!("{}/", fs.uri())).unwrap());
    let idx = build("definition:kestrel", &json!({"base_url": site.uri()}), &c).unwrap();
    assert_eq!(
        idx.search(&SearchQuery::basic("x")).await.unwrap().len(),
        2,
        "solved by FlareSolverr"
    );
    assert_eq!(
        idx.search(&SearchQuery::basic("x")).await.unwrap().len(),
        2,
        "reuses clearance cookie"
    );
}
