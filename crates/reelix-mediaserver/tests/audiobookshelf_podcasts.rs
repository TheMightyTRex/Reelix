//! Audiobookshelf podcast libraries (read by radio) against mocked HTTP.

use chrono::{TimeZone, Utc};
use reelix_mediaserver::audiobookshelf::{AbsPodcastEpisode, Audiobookshelf};
use reelix_mediaserver::{BuildContext, MsError};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BATCH: &str = include_str!("fixtures/abs_podcast_batch.json");

fn abs(ms: &MockServer) -> Audiobookshelf {
    Audiobookshelf::from_settings(
        &json!({ "url": ms.uri(), "api_key": "abstoken" }),
        &BuildContext::new(reqwest::Client::new(), "reelix-test-client"),
    )
    .unwrap()
}

async fn libraries(ms: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/libraries"))
        .and(header("Authorization", "Bearer abstoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "libraries": [
            { "id": "lib-audio", "name": "Audiobooks", "mediaType": "book",
              "folders": [{ "id": "f1", "fullPath": "/audiobooks" }] },
            { "id": "lib-pod", "name": "Podcasts", "mediaType": "podcast",
              "folders": [{ "id": "f3", "fullPath": "/podcasts" }] }
        ]})))
        .mount(ms)
        .await;
}

fn summaries(ids: &[String]) -> Value {
    json!({
        "results": ids.iter().map(|id| json!({ "id": id, "mediaType": "podcast",
            "media": { "metadata": { "title": id }, "numEpisodes": 1 } })).collect::<Vec<_>>(),
        "total": 101,
        "limit": 100,
    })
}

#[tokio::test]
async fn lists_only_podcast_libraries() {
    let ms = MockServer::start().await;
    libraries(&ms).await;
    let libs = abs(&ms).podcast_libraries().await.unwrap();
    assert_eq!(libs.len(), 1);
    assert_eq!(libs[0].id, "lib-pod");
    assert_eq!(libs[0].folders[0].full_path, "/podcasts");
    // The book refresh still leaves podcast libraries out.
    let books = abs(&ms).libraries().await.unwrap();
    assert_eq!(
        books.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        ["lib-audio"]
    );
}

#[tokio::test]
async fn pages_items_and_reads_details_in_batches() {
    let ms = MockServer::start().await;
    // 101 items: a full first page, then one more.
    let mut ids: Vec<String> = vec!["li_archers".into(), "li_quiz".into()];
    ids.extend((0..99).map(|i| format!("li_empty_{i}")));
    Mock::given(method("GET"))
        .and(path("/api/libraries/lib-pod/items"))
        .and(query_param("page", "0"))
        .and(query_param("limit", "100"))
        .and(query_param("minified", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(summaries(&ids[..100])))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/libraries/lib-pod/items"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(summaries(&ids[100..])))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/items/batch/get"))
        .and(header("Authorization", "Bearer abstoken"))
        .and(body_string_contains("\"li_archers\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(BATCH))
        .expect(1)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/items/batch/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "libraryItems": [] })))
        .expect(4)
        .mount(&ms)
        .await;

    let eps = abs(&ms).podcast_episodes("lib-pod").await.unwrap();
    // The episode without an audio file is not in the library.
    assert_eq!(eps.len(), 2);
    assert_eq!(
        eps[0],
        AbsPodcastEpisode {
            item_id: "li_archers".into(),
            episode_id: "ep_a1".into(),
            item_title: Some("The Archers".into()),
            title: Some("Ambridge in Spring".into()),
            season: Some(20),
            episode: Some(3),
            guid: Some("urn:bbc:podcast:m001abcd".into()),
            enclosure_url: Some("https://media.example/archers/m001abcd.mp3".into()),
            published_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap()),
            duration_secs: Some(773),
            path: Some("/podcasts/The Archers/The Archers - S20E03 - Ambridge in Spring.mp3".into()),
            size: Some(12_345_678),
        }
    );
    let q = &eps[1];
    assert_eq!(
        (q.item_id.as_str(), q.item_title.as_deref(), q.season, q.episode),
        ("li_quiz", Some("The News Quiz"), Some(2), Some(7))
    );
    assert_eq!(
        q.published_at,
        Some(Utc.with_ymd_and_hms(2024, 2, 1, 12, 0, 0).unwrap())
    );
    assert_eq!(
        (q.duration_secs, q.size, q.guid.as_deref()),
        (Some(1800), None, None)
    );

    // Batches hold at most 25 items.
    let batches: Vec<Value> = ms
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/items/batch/get")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(batches.len(), 5);
    assert!(
        batches
            .iter()
            .all(|b| b["libraryItemIds"].as_array().unwrap().len() <= 25)
    );
}

#[tokio::test]
async fn a_page_with_nothing_new_ends_the_listing() {
    let ms = MockServer::start().await;
    let ids: Vec<String> = (0..100).map(|i| format!("li_{i}")).collect();
    // A server that ignores `page` keeps sending the same full page.
    Mock::given(method("GET"))
        .and(path("/api/libraries/lib-pod/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results":
            ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>() })))
        .expect(2)
        .mount(&ms)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/items/batch/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "libraryItems": [] })))
        .mount(&ms)
        .await;
    assert!(abs(&ms).podcast_episodes("lib-pod").await.unwrap().is_empty());
}

#[tokio::test]
async fn oversized_responses_are_refused() {
    let ms = MockServer::start().await;
    let huge = format!(
        "{{\"results\": [], \"pad\": \"{}\"}}",
        "x".repeat(33 * 1024 * 1024)
    );
    Mock::given(method("GET"))
        .and(path("/api/libraries/lib-pod/items"))
        .respond_with(ResponseTemplate::new(200).set_body_string(huge))
        .mount(&ms)
        .await;
    let err = abs(&ms).podcast_episodes("lib-pod").await.unwrap_err();
    assert!(
        matches!(&err, MsError::Parse(m) if m.contains("larger than")),
        "{err}"
    );
}

#[tokio::test]
async fn scans_one_library_and_reports_errors() {
    let ms = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/libraries/lib-pod/scan"))
        .and(header("Authorization", "Bearer abstoken"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ms)
        .await;
    abs(&ms).scan_library("lib-pod").await.unwrap();
    assert!(matches!(
        abs(&ms).scan_library("missing").await,
        Err(MsError::NotFound(_))
    ));
    Mock::given(method("GET"))
        .and(path("/api/libraries"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&ms)
        .await;
    assert!(matches!(
        abs(&ms).podcast_libraries().await,
        Err(MsError::Unauthorized)
    ));
}
