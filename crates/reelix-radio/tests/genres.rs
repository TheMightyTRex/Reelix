//! Shared genres across every source (RFX-93), Podcast Index categories (RFX-100), the broadcaster of each
//! show (RFX-101) and what `GET /radio/providers` says about each source (RFX-102), against saved responses.

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::radio::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;

use reelix_radio::providers::{AppleProvider, BbcProvider, PodcastIndexProvider, RssProvider};
use reelix_radio::*;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/providers/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn service(s: &MockServer) -> RadioService {
    let http = reqwest::Client::new();
    let svc = RadioService::new(RadioDeps {
        db: Db::in_memory().await.unwrap(),
        events: EventBus::default(),
        http: http.clone(),
        providers: ProviderRegistry::default()
            .with(Arc::new(
                AppleProvider::new(http.clone())
                    .with_base_url(&s.uri())
                    .with_charts_url(&s.uri())
                    .with_interval(Duration::ZERO),
            ))
            .with(Arc::new(
                PodcastIndexProvider::new(http.clone())
                    .with_base_url(&s.uri())
                    .with_interval(Duration::ZERO),
            ))
            .with(Arc::new(RssProvider::new(http))),
        library_sync: None,
    });
    svc.set_secrets(SecretBox::from_key([9; 32]));
    svc
}

#[tokio::test]
async fn a_genre_row_gathers_every_source_in_turn() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/us/rss/toppodcasts/limit=50/genre=1303/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/javascript; charset=UTF-8")
                .set_body_string(fixture("apple_genre_chart.json")),
        )
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/podcasts/trending"))
        .and(query_param("cat", "Comedy"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("podcastindex_trending.json")))
        .expect(1)
        .mount(&s)
        .await;
    let svc = service(&s).await;
    svc.set_settings(RadioSettingsDto {
        podcastindex_api_key: Some("k".into()),
        podcastindex_api_secret: Some("s".into()),
        default_follow: RadioFollow::FromNowOn,
        ..RadioSettingsDto::default()
    })
    .await
    .unwrap();

    // The shared genres come first in Discover, in their own group, for every source.
    let lists = svc.discover_lists().await.unwrap();
    let comedy = lists
        .categories
        .iter()
        .find(|c| c.id == "all-genre-comedy")
        .unwrap();
    assert_eq!(comedy.name, "Comedy");
    assert_eq!(comedy.provider_name, "Every source");
    assert_eq!(comedy.group.as_deref(), Some("Genres"));
    assert_eq!(svc.genres().len(), reelix_radio::genres::GENRES.len());

    let hits = svc.discover("all-genre-comedy").await.unwrap();
    // Apple's chart (2) and Podcast Index's trending (2), taken in turn.
    let from: Vec<&str> = hits.iter().map(|h| h.provider.as_str()).collect();
    assert_eq!(from, ["apple", "podcastindex", "apple", "podcastindex"]);
    // RFX-101: "In Our Time" is by BBC Radio 4, so it is a radio station's show; the others are not.
    let iot = hits.iter().find(|h| h.title == "In Our Time").unwrap();
    assert_eq!(iot.broadcaster.as_deref(), Some("BBC"));
    assert!(
        hits.iter()
            .filter(|h| h.title != "In Our Time")
            .all(|h| h.broadcaster.is_none())
    );

    assert!(matches!(
        svc.discover("all-genre-nope").await,
        Err(RadioError::NotFound(_))
    ));
}

#[tokio::test]
async fn podcast_index_only_offers_the_shared_genres_categories() {
    let p = PodcastIndexProvider::new(reqwest::Client::new());
    assert!(matches!(
        p.discover("category-Gardening", &SearchPrefs::default()).await,
        Err(ProviderError::NotFound(_))
    ));
}

#[test]
fn what_each_source_says_about_itself() {
    let bbc = BbcProvider::new(reqwest::Client::new());
    let stations = bbc.stations();
    assert_eq!(stations.len(), 66);
    let kent = stations.iter().find(|s| s.id == "bbc_radio_kent").unwrap();
    assert_eq!(kent.name, "BBC Radio Kent");
    assert_eq!(kent.group.as_deref(), Some("Local"));
    assert!(
        kent.logo_url
            .as_deref()
            .unwrap()
            .contains("/networks/bbc_radio_kent/")
    );
    assert!(!bbc.key_needed());
    assert!(bbc.episodes_arrive().starts_with("Programme pages"));
    assert!(PodcastIndexProvider::new(reqwest::Client::new()).key_needed());

    let dtos = ProviderRegistry::default()
        .with(Arc::new(bbc))
        .with(Arc::new(PodcastIndexProvider::new(reqwest::Client::new())))
        .dtos(&[]);
    assert_eq!(dtos[0].stations.len(), 66);
    assert!(
        dtos[1].key_needed && !dtos[1].enabled,
        "needs its key before it is on"
    );
    assert_eq!(dtos[1].episodes_arrive, "Podcast feeds");
}
