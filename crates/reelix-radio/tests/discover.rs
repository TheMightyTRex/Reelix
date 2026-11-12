//! Discover lists: Apple charts, BBC Sounds popular programmes and Podcast Index trending
//! against saved (trimmed) responses; which lists the settings offer; followed shows marked
//! by id, alias and feed URL.

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::radio::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;

use reelix_radio::providers::{AppleProvider, BbcProvider, PodcastIndexProvider, RssProvider, podcastindex};
use reelix_radio::*;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/providers/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn json(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(fixture(name))
}

fn gb() -> SearchPrefs {
    SearchPrefs {
        country: Some("GB".into()),
        language: Some("en".into()),
    }
}

const FEED: &str = include_str!("fixtures/plain.xml");

// ---------------------------------------------------------------- Apple

#[tokio::test]
async fn apple_top_chart_and_categories() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/gb/podcasts/top/50/podcasts.json"))
        .respond_with(json("apple_top_chart.json"))
        .expect(1) // cached for 30 minutes
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/gb/rss/toppodcasts/limit=50/genre=1303/json"))
        // The old iTunes RSS answers as JavaScript.
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/javascript; charset=UTF-8")
                .set_body_string(fixture("apple_genre_chart.json")),
        )
        .mount(&s)
        .await;
    let p = AppleProvider::new(reqwest::Client::new())
        .with_base_url(&s.uri())
        .with_charts_url(&s.uri())
        .with_interval(Duration::ZERO);

    let lists = p.discover_lists();
    assert_eq!(lists[0], DiscoverList::row("top", "Top podcasts"));
    assert!(lists.contains(&DiscoverList::category("genre-1303", "Comedy")));

    let top = p.discover("top", &gb()).await.unwrap();
    assert_eq!(top.len(), 3);
    assert_eq!(top[0].provider, "apple", "chart hits keep the Apple id");
    assert_eq!(top[0].external_id, "1743030473");
    assert_eq!(top[0].title, "The Rest Is Politics: US");
    assert_eq!(top[0].author.as_deref(), Some("Goalhanger"));
    assert!(
        top[0].feed_url.is_none(),
        "the feed is looked up only when opened"
    );
    let art = top[0].artwork_url.as_deref().unwrap();
    assert!(
        art.starts_with("https://is1-ssl.mzstatic.com/") && art.ends_with("/600x600bb.jpg"),
        "{art}"
    );
    assert_eq!(
        top[0].web_url.as_deref(),
        Some("https://podcasts.apple.com/gb/podcast/the-rest-is-politics-us/id1743030473")
    );
    // Second read comes from the cache (`expect(1)`).
    assert_eq!(p.discover("top", &gb()).await.unwrap(), top);

    let comedy = p.discover("genre-1303", &gb()).await.unwrap();
    assert_eq!(comedy.len(), 2);
    assert_eq!(comedy[0].external_id, "1838594107");
    assert_eq!(comedy[0].title, "The Romesh Ranganathan Show");
    assert_eq!(comedy[0].author.as_deref(), Some("Ranga Bee"));
    assert!(comedy[0].synopsis.as_deref().unwrap().starts_with("The world"));
    assert_eq!(
        comedy[0].web_url.as_deref(),
        Some("https://podcasts.apple.com/gb/podcast/the-romesh-ranganathan-show/id1838594107")
    );
    assert!(
        comedy[0]
            .artwork_url
            .as_deref()
            .unwrap()
            .ends_with("/600x600bb.jpg")
    );

    assert!(matches!(
        p.discover("genre-9999", &gb()).await,
        Err(ProviderError::NotFound(_))
    ));
}

#[test]
fn apple_chart_answers_are_checked() {
    use reelix_radio::providers::apple::{parse_genre_chart, parse_top_chart};
    assert!(parse_top_chart(&serde_json::json!({"feed": {}})).is_err());
    assert!(parse_genre_chart(&serde_json::json!({"nope": 1})).is_err());
    // An empty category has no entries; a single entry is an object.
    assert!(
        parse_genre_chart(&serde_json::json!({"feed": {"author": {}}}))
            .unwrap()
            .is_empty()
    );
    let one = serde_json::json!({"feed": {"entry": {
        "im:name": {"label": "Solo"},
        "id": {"attributes": {"im:id": "42"}},
    }}});
    assert_eq!(parse_genre_chart(&one).unwrap()[0].external_id, "42");
}

// ---------------------------------------------------------------- BBC

#[tokio::test]
async fn bbc_popular_programmes_per_category() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/programmes/playable"))
        .and(query_param("sort", "popular"))
        .and(query_param("category", "comedy"))
        .respond_with(json("bbc_popular.json"))
        .mount(&s)
        .await;
    let p = BbcProvider::new(reqwest::Client::new())
        .with_base_urls(&s.uri(), &s.uri())
        .with_interval(Duration::ZERO);
    assert!(
        p.discover_lists()
            .contains(&DiscoverList::category("category-comedy", "All comedy").in_group("Comedy"))
    );
    let hits = p.discover("category-comedy", &gb()).await.unwrap();
    // Six popular episodes of five programmes, in order.
    let ids: Vec<&str> = hits.iter().map(|h| h.external_id.as_str()).collect();
    assert_eq!(ids, ["m0005fdz", "m001x0mr", "b009v6h3", "b007mjjg", "b009v07q"]);
    assert_eq!(hits[0].provider, "bbc");
    assert_eq!(hits[0].title, "Elis James and John Robins");
    assert_eq!(hits[0].station_name.as_deref(), Some("Radio 5 Live"));
    assert!(hits[0].synopsis.is_some());
    assert!(
        hits[0]
            .artwork_url
            .as_deref()
            .unwrap()
            .starts_with("https://ichef.bbci.co.uk/images/ic/640x640/")
    );
    assert_eq!(
        hits[0].web_url.as_deref(),
        Some("https://www.bbc.co.uk/programmes/m0005fdz")
    );
    assert!(matches!(
        p.discover("category-gardening", &gb()).await,
        Err(ProviderError::NotFound(_))
    ));
    assert!(matches!(
        reelix_radio::providers::bbc::popular_hits(&serde_json::json!({"x": 1})),
        Err(ProviderError::Parse(_))
    ));
}

#[test]
fn bbc_lists_stations_and_every_category_in_groups() {
    let lists = BbcProvider::new(reqwest::Client::new()).discover_lists();
    let rows: Vec<&str> = lists
        .iter()
        .filter(|l| !l.category)
        .map(|l| l.id.as_str())
        .collect();
    assert_eq!(
        rows,
        [
            "popular",
            "station-bbc_radio_four",
            "station-bbc_radio_four_extra",
            "station-bbc_radio_two",
            "station-bbc_radio_five_live",
            "station-bbc_world_service",
        ]
    );
    assert!(lists.contains(&DiscoverList::row(
        "station-bbc_radio_four_extra",
        "Popular on Radio 4 Extra"
    )));
    let categories: Vec<&DiscoverList> = lists
        .iter()
        .filter(|l| l.category && !l.id.starts_with("network-"))
        .collect();
    assert_eq!(categories.len(), 32);
    assert!(categories.iter().all(|c| c.group.is_some()));
    // BBC Sounds has no science fiction category of its own: it is a drama sub-genre.
    assert!(
        lists.contains(
            &DiscoverList::category("category-drama-scifiandfantasy", "Science fiction and fantasy")
                .in_group("Drama")
        )
    );
    let mut groups: Vec<&str> = categories.iter().filter_map(|c| c.group.as_deref()).collect();
    groups.dedup();
    assert_eq!(groups, ["Comedy", "Drama", "Factual", "Sport and music"]);
}

#[test]
fn bbc_lists_every_station_by_group() {
    // RFX-94: all 66 networks BBC Sounds lists, national, the nations and local, each a category of its own.
    let lists = BbcProvider::new(reqwest::Client::new()).discover_lists();
    let networks: Vec<&DiscoverList> = lists.iter().filter(|l| l.id.starts_with("network-")).collect();
    assert_eq!(networks.len(), 66);
    assert!(networks.iter().all(|n| n.category));
    let mut groups: Vec<&str> = networks.iter().filter_map(|c| c.group.as_deref()).collect();
    groups.dedup();
    assert_eq!(
        groups,
        [
            "BBC stations: national",
            "BBC stations: nations",
            "BBC stations: local"
        ]
    );
    assert!(lists.contains(
        &DiscoverList::category("network-bbc_radio_kent", "BBC Radio Kent").in_group("BBC stations: local")
    ));
    assert!(
        lists.contains(
            &DiscoverList::category("network-bbc_radio_scotland", "BBC Radio Scotland")
                .in_group("BBC stations: nations")
        )
    );
}

#[tokio::test]
async fn bbc_popular_programmes_of_a_local_station() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/programmes/playable"))
        .and(query_param("network", "bbc_radio_kent"))
        .respond_with(json("bbc_popular_station.json"))
        .mount(&s)
        .await;
    let p = BbcProvider::new(reqwest::Client::new())
        .with_base_urls(&s.uri(), &s.uri())
        .with_interval(Duration::ZERO);
    assert!(
        !p.discover("network-bbc_radio_kent", &gb())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        p.discover("network-bbc_radio_nowhere", &gb()).await,
        Err(ProviderError::NotFound(_))
    ));
}

#[tokio::test]
async fn bbc_popular_programmes_per_station_and_sub_genre() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/programmes/playable"))
        .and(query_param("sort", "popular"))
        .and(query_param("network", "bbc_radio_four_extra"))
        .and(query_param_is_missing("category"))
        .respond_with(json("bbc_popular_station.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/programmes/playable"))
        .and(query_param("category", "drama-scifiandfantasy"))
        .and(query_param_is_missing("network"))
        .respond_with(json("bbc_popular.json"))
        .mount(&s)
        .await;
    let p = BbcProvider::new(reqwest::Client::new())
        .with_base_urls(&s.uri(), &s.uri())
        .with_interval(Duration::ZERO);
    let hits = p.discover("station-bbc_radio_four_extra", &gb()).await.unwrap();
    // Five popular episodes of four programmes, in order.
    let ids: Vec<&str> = hits.iter().map(|h| h.external_id.as_str()).collect();
    assert_eq!(ids, ["p0fsqm4x", "b009v07q", "b036y0y4", "b009v6h3"]);
    assert_eq!(hits[0].title, "Whodunnits");
    assert!(
        hits.iter()
            .all(|h| h.station_name.as_deref() == Some("Radio 4 Extra"))
    );
    assert!(
        !p.discover("category-drama-scifiandfantasy", &gb())
            .await
            .unwrap()
            .is_empty()
    );
    for unknown in ["station-bbc_radio_one_xtra", "station-", "nope"] {
        assert!(matches!(
            p.discover(unknown, &gb()).await,
            Err(ProviderError::NotFound(_))
        ));
    }
}

// ---------------------------------------------------------------- Podcast Index

#[tokio::test]
async fn podcastindex_trending_in_the_settings_language() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/podcasts/trending"))
        .and(query_param("lang", "en"))
        .and(|r: &Request| {
            let h = |n: &str| {
                r.headers
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            };
            h("authorization") == podcastindex::signature("KEY", "SECRET", &h("x-auth-date"))
        })
        .respond_with(json("podcastindex_trending.json"))
        .mount(&s)
        .await;
    let p = PodcastIndexProvider::new(reqwest::Client::new())
        .with_base_url(&s.uri())
        .with_interval(Duration::ZERO);
    assert!(p.discover("trending", &gb()).await.is_err(), "needs credentials");
    p.configure(&RadioSettingsDto {
        podcastindex_api_key: Some("KEY".into()),
        podcastindex_api_secret: Some("SECRET".into()),
        ..RadioSettingsDto::default()
    });
    let hits = p.discover("trending", &gb()).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].provider, "podcastindex");
    assert_eq!(hits[0].external_id, "920666");
    assert_eq!(hits[0].title, "In Our Time");
    assert_eq!(
        hits[0].feed_url.as_deref(),
        Some("https://podcasts.files.bbci.co.uk/b006qykl.rss")
    );
    assert_eq!(
        hits[1].synopsis.as_deref(),
        Some("Science news from around the world")
    );
}

// ---------------------------------------------------------------- choosing lists

#[test]
fn discover_sources_follow_country_and_configuration() {
    let http = reqwest::Client::new();
    let pi = Arc::new(PodcastIndexProvider::new(http.clone()));
    let reg = ProviderRegistry::default()
        .with(Arc::new(BbcProvider::new(http.clone())))
        .with(Arc::new(AppleProvider::new(http.clone())))
        .with(pi.clone())
        .with(Arc::new(RssProvider::new(http)));
    let ids = |enabled: &[String], prefs: SearchPrefs| -> Vec<&'static str> {
        reg.for_discover(enabled, &prefs).iter().map(|p| p.id()).collect()
    };
    // rss has no lists; Podcast Index is off until configured.
    assert_eq!(ids(&[], SearchPrefs::default()), ["bbc", "apple"]);
    assert_eq!(ids(&[], gb()), ["bbc", "apple"]);
    let us = SearchPrefs {
        country: Some("US".into()),
        language: None,
    };
    assert_eq!(ids(&[], us.clone()), ["apple"], "the BBC only for GB");
    pi.configure(&RadioSettingsDto {
        podcastindex_api_key: Some("k".into()),
        podcastindex_api_secret: Some("s".into()),
        ..RadioSettingsDto::default()
    });
    assert_eq!(ids(&[], us), ["apple", "podcastindex"]);
    assert_eq!(ids(&["podcastindex".into()], gb()), ["podcastindex"]);
}

// ---------------------------------------------------------------- service

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
async fn followed_shows_are_marked_by_id_alias_and_feed() {
    let s = MockServer::start().await;
    let feed = format!("{}/feed.xml", s.uri());
    // The first chart entry is the podcast the test follows by its Apple id.
    let chart = fixture("apple_top_chart.json").replace("1743030473", "1767854856");
    Mock::given(method("GET"))
        .and(path("/api/v2/us/podcasts/top/50/podcasts.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(chart))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/lookup"))
        .and(query_param("id", "1767854856"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            fixture("apple_lookup.json").replace("https://podcasts.files.bbci.co.uk/b006s5dp.rss", &feed),
        ))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FEED))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/podcasts/trending"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                fixture("podcastindex_trending.json")
                    .replace("https://podcasts.files.bbci.co.uk/b006qykl.rss", &feed),
            ),
        )
        .mount(&s)
        .await;
    let svc = service(&s).await;

    // No country set: Apple's US chart; Podcast Index only once configured.
    let lists = svc.discover_lists().await.unwrap();
    let rows: Vec<&str> = lists.rows.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(rows, ["apple-top"]);
    assert_eq!(lists.rows[0].provider_name, "Apple Podcasts");
    assert!(
        lists
            .categories
            .iter()
            .any(|c| c.id == "apple-genre-1303" && c.name == "Comedy")
    );
    svc.set_settings(RadioSettingsDto {
        podcastindex_api_key: Some("k".into()),
        podcastindex_api_secret: Some("s".into()),
        default_follow: RadioFollow::FromNowOn,
        ..RadioSettingsDto::default()
    })
    .await
    .unwrap();
    let rows: Vec<String> = svc
        .discover_lists()
        .await
        .unwrap()
        .rows
        .into_iter()
        .map(|l| l.id)
        .collect();
    assert_eq!(rows, ["apple-top", "podcastindex-trending"]);
    assert!(matches!(
        svc.discover("bbc-popular").await,
        Err(RadioError::NotFound(_))
    ));
    assert!(matches!(
        svc.discover("apple-nope").await,
        Err(RadioError::NotFound(_))
    ));

    let top = svc.discover("apple-top").await.unwrap();
    assert!(top.iter().all(|h| h.show_id.is_none()));

    // Opening the chart entry reads the feed (handed to rss) and saves nothing.
    let preview = svc.preview_show("apple", "1767854856").await.unwrap();
    assert_eq!(preview.show.provider, "rss");
    assert_eq!(preview.show.external_id, feed);
    assert!(!preview.latest_episodes.is_empty());
    assert_eq!(preview.show.show_id, None);
    assert_eq!(
        svc.list_shows(&RadioShowListQuery::default())
            .await
            .unwrap()
            .total,
        0
    );

    // Following it by the Apple id saves an rss show and remembers the Apple id.
    let show = svc
        .add_show(AddRadioShowRequest {
            provider: Some("apple".into()),
            external_id: Some("1767854856".into()),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(show.provider, "rss");
    assert_eq!(show.external_id, feed);
    assert_eq!(
        show.follow,
        RadioFollow::FromNowOn,
        "the settings' default follow"
    );

    let top = svc.discover("apple-top").await.unwrap();
    assert_eq!(top[0].show_id, Some(show.id), "marked through the alias");
    assert!(top[1..].iter().all(|h| h.show_id.is_none()));
    // Podcast Index lists the same feed under its own id: marked by the feed URL.
    let trending = svc.discover("podcastindex-trending").await.unwrap();
    assert_eq!(trending[0].show_id, Some(show.id));
    assert_eq!(trending[1].show_id, None);

    let followed = svc
        .followed_shows(&[
            ("apple".into(), "1767854856".into()),
            ("rss".into(), format!("{feed}/")),
            ("apple".into(), "1".into()),
        ])
        .await
        .unwrap();
    assert_eq!(followed.len(), 2);
    assert_eq!(
        svc.preview_show("apple", "1767854856")
            .await
            .unwrap()
            .show
            .show_id,
        Some(show.id)
    );
    // Asking again by the Apple id is a conflict, not a second show.
    assert!(matches!(
        svc.add_show(AddRadioShowRequest {
            provider: Some("apple".into()),
            external_id: Some("1767854856".into()),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await,
        Err(RadioError::Conflict(_))
    ));
}
