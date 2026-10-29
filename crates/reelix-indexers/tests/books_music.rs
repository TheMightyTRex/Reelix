//! Book and music searches: caps parsing, structured params, plain-search fallback,
//! per-indexer categories and category exclusions (Newznab/Torznab and YAML definitions).

mod common;

use std::sync::Arc;

use common::{ctx, fixture, fixture_path};
use reelix_core::BookFormat;
use reelix_indexers::{DefinitionStore, SearchKind, SearchParam, SearchQuery, build, categories};
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn xml(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/rss+xml")
}

async fn caps(server: &MockServer, file: &str) {
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(xml(fixture(file)))
        .mount(server)
        .await;
}

fn newznab(server: &MockServer, extra: serde_json::Value) -> Box<dyn reelix_indexers::Indexer> {
    let mut s = json!({"base_url": server.uri(), "api_key": "k"});
    s.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    build("newznab", &s, &ctx(1, "Media")).unwrap()
}

fn guids(r: &[reelix_indexers::ReleaseInfo]) -> Vec<&str> {
    r.iter().map(|r| r.guid.as_str()).collect()
}

#[tokio::test]
async fn caps_parse_book_and_audio_search() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps_media.xml").await;
    let c = newznab(&server, json!({})).capabilities().await.unwrap();
    assert!(c.book_search.supports(SearchParam::Author) && c.book_search.supports(SearchParam::Title));
    assert!(c.audio_search.supports(SearchParam::Artist) && c.audio_search.supports(SearchParam::Year));
    assert!(c.mode(SearchKind::Music).supports(SearchParam::Album));
    assert!(c.category_ids().contains(&categories::AUDIO_LOSSLESS));

    // Torznab proxies call it `music-search`.
    let server = MockServer::start().await;
    caps(&server, "torznab_caps_music.xml").await;
    let idx = build("torznab", &json!({"base_url": server.uri()}), &ctx(2, "Proxy")).unwrap();
    let c = idx.capabilities().await.unwrap();
    assert!(c.audio_search.supports(SearchParam::Artist));
    assert!(c.book_search.supports(SearchParam::Author) && !c.book_search.supports(SearchParam::Title));
}

#[tokio::test]
async fn book_search_uses_structured_params_and_drops_mags() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps_media.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "book"))
        .and(query_param("author", "Frank Herbert"))
        .and(query_param("title", "Dune"))
        .and(query_param_is_missing("q"))
        .and(query_param("cat", "7000,7020"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    let q = SearchQuery {
        book_format: Some(BookFormat::Ebook),
        ..SearchQuery::book("Frank Herbert", "Dune")
    };
    let r = newznab(&server, json!({})).search(&q).await.unwrap();
    // The 7010 (Mags) item is excluded; everything else is left to the decision engine.
    assert_eq!(guids(&r), ["book-epub", "book-m4b", "album-flac", "album-video"]);
}

#[tokio::test]
async fn format_less_book_search_uses_ebook_and_audiobook_categories() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps_media.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "book"))
        .and(query_param("cat", "7000,7020,3030"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    newznab(&server, json!({}))
        .search(&SearchQuery::book("Frank Herbert", "Dune"))
        .await
        .unwrap();
}

#[tokio::test]
async fn audiobook_search_uses_the_audiobook_categories() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps_media.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "book"))
        .and(query_param("cat", "3030"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    let q = SearchQuery {
        book_format: Some(BookFormat::Audiobook),
        ..SearchQuery::book("Frank Herbert", "Dune")
    };
    let r = newznab(&server, json!({})).search(&q).await.unwrap();
    // Nothing is excluded for audiobooks; mags are not an ebook query here either.
    assert_eq!(r.len(), 5);
}

#[tokio::test]
async fn music_search_sends_artist_album_year_and_drops_audio_video() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps_media.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "music"))
        .and(query_param("artist", "Radiohead"))
        .and(query_param("album", "OK Computer"))
        .and(query_param("year", "1997"))
        .and(query_param("cat", "3000,3010,3040"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    let q = SearchQuery {
        year: Some(1997),
        ..SearchQuery::music("Radiohead", "OK Computer")
    };
    let r = newznab(&server, json!({})).search(&q).await.unwrap();
    // 3020 (Audio/Video) and 3030 (audiobook) are excluded from music results.
    assert_eq!(guids(&r), ["book-epub", "mag", "album-flac"]);
}

#[tokio::test]
async fn per_indexer_categories_and_explicit_overrides() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps_media.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "music"))
        .and(query_param("cat", "3040"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "book"))
        .and(query_param("cat", "7010"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    let idx = newznab(
        &server,
        json!({"book_categories": [7020], "music_categories": ["3040"]}),
    );
    idx.search(&SearchQuery::music("Radiohead", "OK Computer"))
        .await
        .unwrap();
    // Explicit query categories win; asking for Mags keeps them.
    let q = SearchQuery {
        categories: vec![categories::BOOKS_MAGS],
        book_format: Some(BookFormat::Ebook),
        ..SearchQuery::book("", "Dune Fan Magazine")
    };
    let r = idx.search(&q).await.unwrap();
    assert!(guids(&r).contains(&"mag"));
}

#[tokio::test]
async fn falls_back_to_plain_search_without_book_or_audio_caps() {
    let server = MockServer::start().await;
    // newznab_caps.xml has no book-search and audio-search available="no".
    caps(&server, "newznab_caps.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "search"))
        .and(query_param("q", "Frank Herbert Dune"))
        .and(query_param("cat", "7000,7020"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "search"))
        .and(query_param("q", "Radiohead OK Computer"))
        .and(query_param("cat", "3000,3010,3040"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    let idx = newznab(&server, json!({}));
    let q = SearchQuery {
        book_format: Some(BookFormat::Ebook),
        ..SearchQuery::book("Frank Herbert", "Dune")
    };
    assert_eq!(idx.search(&q).await.unwrap().len(), 4);
    let q = SearchQuery {
        year: Some(1997),
        ..SearchQuery::music("Radiohead", "OK Computer")
    };
    assert_eq!(idx.search(&q).await.unwrap().len(), 3);
    // Nothing to search for → no request.
    assert!(idx.search(&SearchQuery::music("", "")).await.unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_book_params_go_into_q() {
    let server = MockServer::start().await;
    caps(&server, "torznab_caps_music.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "book"))
        .and(query_param("author", "Frank Herbert"))
        .and(query_param("q", "Dune"))
        .and(query_param_is_missing("title"))
        .respond_with(xml(fixture("newznab_media_search.xml")))
        .expect(1)
        .mount(&server)
        .await;
    let idx = build("torznab", &json!({"base_url": server.uri()}), &ctx(2, "Proxy")).unwrap();
    idx.search(&SearchQuery::book("Frank Herbert", "Dune"))
        .await
        .unwrap();
}

fn media_ctx(id: i64) -> reelix_indexers::BuildContext {
    let (store, errors) = DefinitionStore::load_dir(&fixture_path("definitions-media")).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let mut c = ctx(id, "Quillfeed");
    c.definitions = Some(Arc::new(store));
    c
}

fn json_body(v: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(v.to_string(), "application/json")
}

#[tokio::test]
async fn definition_book_and_music_searches() {
    let site = MockServer::start().await;
    let results = json!({"results": [
        {"cat": "ebooks", "name": "Frank Herbert - Dune [EPUB]", "link": "/dl/1.torrent", "size": 1000},
        {"cat": "mags", "name": "Dune Fan Magazine 42", "link": "/dl/2.torrent", "size": 1000},
        {"cat": "flac", "name": "Radiohead - OK Computer [FLAC]", "link": "/dl/3.torrent", "size": 1000}
    ]});
    // book-search is declared → Query.Type = book; default ebook categories map to the site ids.
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("type", "book"))
        .and(query_param("author", "Frank Herbert"))
        .and(query_param("title", "Dune"))
        .and(query_param("q", "Frank Herbert Dune"))
        .and(query_param("cats", "ebooks"))
        .respond_with(json_body(results.clone()))
        .expect(1)
        .mount(&site)
        .await;
    // No music-search mode → plain search with built keywords.
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("type", "search"))
        .and(query_param("artist", "Radiohead"))
        .and(query_param("q", "Radiohead OK Computer"))
        .and(query_param("cats", "flac,mp3"))
        .respond_with(json_body(results))
        .expect(1)
        .mount(&site)
        .await;
    let idx = build(
        "definition:quillfeed",
        &json!({"base_url": site.uri()}),
        &media_ctx(5),
    )
    .unwrap();
    let c = idx.capabilities().await.unwrap();
    assert!(c.book_search.supports(SearchParam::Author) && !c.audio_search.available);
    assert!(c.category_ids().contains(&categories::AUDIO_LOSSLESS));

    let q = SearchQuery {
        book_format: Some(BookFormat::Ebook),
        ..SearchQuery::book("Frank Herbert", "Dune")
    };
    let r = idx.search(&q).await.unwrap();
    let titles: Vec<_> = r.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Frank Herbert - Dune [EPUB]", "Radiohead - OK Computer [FLAC]"]
    );
    assert_eq!(r[0].categories, [categories::BOOKS_EBOOK]);

    let r = idx
        .search(&SearchQuery::music("Radiohead", "OK Computer"))
        .await
        .unwrap();
    assert_eq!(r.len(), 3);
}

#[tokio::test]
async fn definition_without_book_categories_is_not_searched_for_books() {
    let site = MockServer::start().await;
    // Any request would fail the test (`expect(0)`).
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&site)
        .await;
    let idx = build(
        "definition:kestrel",
        &json!({"base_url": site.uri()}),
        &ctx(6, "Kestrel"),
    )
    .unwrap();
    assert!(
        idx.search(&SearchQuery::book("Frank Herbert", "Dune"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        idx.search(&SearchQuery::music("Radiohead", "OK Computer"))
            .await
            .unwrap()
            .is_empty()
    );
}
