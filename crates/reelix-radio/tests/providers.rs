//! The BBC, Apple Podcasts, Podcast Index and Internet Archive providers against saved (and
//! trimmed) responses served by a mock server; source selection; artwork and logo downloads;
//! provider credentials in the settings.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reelix_api_types::radio::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_radio::providers::{
    AppleProvider, ArchiveProvider, BbcProvider, PodcastIndexProvider, RssProvider, podcastindex,
};
use reelix_radio::*;
use wiremock::matchers::{header, header_exists, method, path, query_param};
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

fn full(max_lookups: u32) -> ShowFetch {
    ShowFetch {
        max_lookups,
        ..ShowFetch::default()
    }
}

// ---------------------------------------------------------------- BBC

async fn bbc_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/programmes/b006s5dp.json"))
        .respond_with(json("bbc_brand.json"))
        .expect(1) // cached after the first read
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/programmes/b006s5dp/children.json"))
        .and(query_param("limit", "50"))
        .respond_with(json("bbc_brand_children.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/programmes/m002qrx3/children.json"))
        .respond_with(json("bbc_s96_children.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/programmes/m002rd20.json"))
        .respond_with(json("bbc_episode.json"))
        .mount(&s)
        .await;
    s
}

fn bbc(s: &MockServer) -> BbcProvider {
    BbcProvider::new(reqwest::Client::new())
        .with_base_urls(&s.uri(), &s.uri())
        .with_interval(Duration::ZERO)
}

#[tokio::test]
async fn bbc_walks_brand_series_and_episodes() {
    let s = bbc_server().await;
    Mock::given(method("GET"))
        .and(path("/programmes/m002h9t4/children.json"))
        .respond_with(json("bbc_s95_children.json"))
        .mount(&s)
        .await;
    let p = bbc(&s);
    let d = p.show("b006s5dp", &full(1)).await.unwrap();
    assert_eq!(d.external_id, "b006s5dp");
    assert_eq!(d.title, "Just a Minute");
    assert!(d.complete);
    assert_eq!(
        d.artwork_url.as_deref(),
        Some("https://ichef.bbci.co.uk/images/ic/640x640/p0m1qz0x.jpg")
    );
    let st = d.station.as_ref().unwrap();
    assert_eq!(
        (st.external_id.as_str(), st.name.as_str()),
        ("bbc_radio_four", "BBC Radio 4")
    );
    assert!(
        st.logo_url
            .as_deref()
            .unwrap()
            .ends_with("/networks/bbc_radio_four/blocks-colour_default.svg")
    );
    let series: Vec<_> = d
        .series
        .iter()
        .map(|s| (s.external_id.as_str(), s.number, s.kind))
        .collect();
    assert_eq!(
        series,
        [
            ("m002qrx3", Some(96), RadioSeriesKind::Series),
            ("m002h9t4", Some(95), RadioSeriesKind::Series),
            ("specials", None, RadioSeriesKind::Specials),
        ]
    );
    assert_eq!(d.series[0].expected_episodes, Some(6));
    assert_eq!(d.episodes.len(), 5);
    let ep = d.episodes.iter().find(|e| e.external_id == "m002rd20").unwrap();
    assert_eq!(ep.series_external_id, "m002qrx3");
    assert_eq!(ep.number, Some(6));
    // One lookup allowed: the newest episode's length, from its canonical version.
    assert_eq!(ep.duration_secs, Some(1680));
    assert!(
        d.episodes
            .iter()
            .filter(|e| e.external_id != "m002rd20")
            .all(|e| e.duration_secs.is_none())
    );
    let special = d
        .episodes
        .iter()
        .find(|e| e.series_external_id == "specials")
        .unwrap();
    assert_eq!(special.external_id, "b0b1xq4z");

    // Cached: a second read makes no new request for the brand (expect(1) above).
    p.show("b006s5dp", &full(0)).await.unwrap();
}

#[tokio::test]
async fn bbc_skips_settled_series_and_known_lengths() {
    let s = bbc_server().await;
    Mock::given(method("GET"))
        .and(path("/programmes/m002h9t4/children.json"))
        .respond_with(json("bbc_s95_children.json"))
        .expect(0)
        .mount(&s)
        .await;
    let fetch = ShowFetch {
        skip_series: HashSet::from(["m002h9t4".to_owned()]),
        known_episodes: HashSet::from(["m002rd20".to_owned()]),
        max_lookups: 30,
        quick: false,
    };
    let d = bbc(&s).show("b006s5dp", &fetch).await.unwrap();
    assert!(d.series.iter().all(|x| x.external_id != "m002h9t4"));
    assert!(d.episodes.iter().all(|e| e.series_external_id != "m002h9t4"));
    // The known episode is not looked up again; other lookups 404 and are skipped.
    assert!(d.episodes.iter().all(|e| e.duration_secs.is_none()));
    assert!(d.complete);
}

#[tokio::test]
async fn bbc_preview_resolves_an_episode_link_to_its_brand() {
    let s = bbc_server().await;
    let d = bbc(&s)
        .preview("https://www.bbc.co.uk/sounds/play/m002rd20")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.external_id, "b006s5dp");
    // A quick read: the newest series (and the specials) only, marked partial.
    assert!(!d.complete);
    assert!(d.episodes.iter().any(|e| e.series_external_id == "m002qrx3"));
    assert!(d.episodes.iter().all(|e| e.series_external_id != "m002h9t4"));
    assert!(
        bbc(&s)
            .preview("https://example.com/feed.xml")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn bbc_rate_limit_keeps_what_was_read() {
    let s = bbc_server().await;
    Mock::given(method("GET"))
        .and(path("/programmes/m002h9t4/children.json"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .expect(1)
        .mount(&s)
        .await;
    let p = bbc(&s);
    let d = p.show("b006s5dp", &full(30)).await.unwrap();
    assert!(!d.complete, "a partial read marks nothing removed");
    assert!(d.episodes.iter().any(|e| e.series_external_id == "m002qrx3"));
    // Until Retry-After passes, requests fail without reaching the BBC.
    let e = p.show("m002qrx2", &full(0)).await.unwrap_err();
    assert!(matches!(e, ProviderError::RateLimited { retry_after: Some(d) } if d > Duration::from_secs(100)));
}

#[tokio::test]
async fn bbc_changed_data_fails_plainly() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/programmes/b006s5dp.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"something":"else"}"#))
        .mount(&s)
        .await;
    let e = bbc(&s).show("b006s5dp", &full(0)).await.unwrap_err();
    assert!(e.to_string().contains("has changed"), "{e}");
    assert!(matches!(
        bbc(&s).show("not a pid", &full(0)).await,
        Err(ProviderError::NotFound(_))
    ));
}

#[tokio::test]
async fn bbc_sounds_search() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/programmes/search/container"))
        .and(query_param("q", "just a minute"))
        .respond_with(json("bbc_search.json"))
        .mount(&s)
        .await;
    let hits = bbc(&s)
        .search("just a minute", &SearchPrefs::default())
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].provider, "bbc");
    assert_eq!(hits[0].external_id, "b006s5dp");
    assert_eq!(hits[0].title, "Just a Minute");
    assert_eq!(hits[0].station_name.as_deref(), Some("Radio 4"));
    assert_eq!(
        hits[0].artwork_url.as_deref(),
        Some("https://ichef.bbci.co.uk/images/ic/640x640/p0m1qz0x.jpg")
    );
}

// ---------------------------------------------------------------- Apple

const FEED: &str = include_str!("fixtures/plain.xml");

#[tokio::test]
async fn apple_search_and_hand_off_to_the_feed() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("media", "podcast"))
        .and(query_param("term", "just a minute"))
        .and(query_param("country", "GB"))
        .respond_with(json("apple_search.json"))
        .mount(&s)
        .await;
    let feed = format!("{}/feed.xml", s.uri());
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
    let p = AppleProvider::new(reqwest::Client::new())
        .with_base_url(&s.uri())
        .with_interval(Duration::ZERO);
    let prefs = SearchPrefs {
        country: Some("GB".into()),
        language: None,
    };
    let hits = p.search("just a minute", &prefs).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].provider, "rss", "hits are followed as feeds");
    assert_eq!(
        hits[0].external_id,
        "https://podcasts.files.bbci.co.uk/b006s5dp.rss"
    );
    assert_eq!(hits[0].author.as_deref(), Some("BBC Radio 4"));
    assert_eq!(hits[0].episode_count, Some(55));
    assert_eq!(
        hits[0].web_url.as_deref(),
        Some("https://podcasts.apple.com/gb/podcast/just-a-minute/id1767854856")
    );

    let d = p
        .preview("https://podcasts.apple.com/gb/podcast/just-a-minute/id1767854856?uo=4")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.provider.as_deref(), Some("rss"));
    assert_eq!(d.external_id, feed);
    assert!(!d.episodes.is_empty());
    assert!(p.preview("https://example.com/x").await.unwrap().is_none());
}

// ---------------------------------------------------------------- Podcast Index

fn signed(key: &'static str, secret: &'static str) -> impl Fn(&Request) -> bool {
    move |r: &Request| {
        let h = |n: &str| {
            r.headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let date = h("x-auth-date");
        h("x-auth-key") == key
            && !date.is_empty()
            && h("authorization") == podcastindex::signature(key, secret, &date)
    }
}

#[tokio::test]
async fn podcastindex_needs_credentials_and_signs_requests() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/byterm"))
        .and(query_param("q", "just a minute"))
        .and(header("x-auth-key", "KEY"))
        .and(header_exists("x-auth-date"))
        .and(signed("KEY", "SECRET"))
        .respond_with(json("podcastindex_search.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/podcasts/byfeedid"))
        .and(query_param("id", "7303624"))
        .and(signed("KEY", "SECRET"))
        .respond_with(json("podcastindex_podcast.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/episodes/byfeedid"))
        .and(query_param("id", "7303624"))
        .and(signed("KEY", "SECRET"))
        .respond_with(json("podcastindex_episodes.json"))
        .mount(&s)
        .await;
    let p = PodcastIndexProvider::new(reqwest::Client::new())
        .with_base_url(&s.uri())
        .with_interval(Duration::ZERO);
    assert!(!p.is_configured());
    let e = p.search("x", &SearchPrefs::default()).await.unwrap_err();
    assert!(matches!(e, ProviderError::Unsupported(_)), "{e}");

    p.configure(&RadioSettingsDto {
        podcastindex_api_key: Some("KEY".into()),
        podcastindex_api_secret: Some("SECRET".into()),
        ..RadioSettingsDto::default()
    });
    assert!(p.is_configured());
    let prefs = SearchPrefs {
        country: None,
        language: Some("en".into()),
    };
    let hits = p.search("just a minute", &prefs).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].external_id, "7303624", "the settings' language first");
    assert_eq!(hits[0].provider, "podcastindex");
    assert_eq!(
        hits[0].feed_url.as_deref(),
        Some("https://podcasts.files.bbci.co.uk/b006s5dp.rss")
    );
    assert_eq!(hits[1].synopsis.as_deref(), Some("Eine Minute ohne Zögern."));

    let d = p
        .preview("https://podcastindex.org/podcast/7303624")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.title, "Just a Minute");
    let series: Vec<_> = d
        .series
        .iter()
        .map(|s| (s.external_id.as_str(), s.number))
        .collect();
    assert_eq!(series, [("season:96", Some(96)), ("specials", None)]);
    assert_eq!(d.episodes.len(), 3);
    assert_eq!(d.episodes[0].external_id, "urn:bbc:podcast:m002rd20");
    assert_eq!(d.episodes[0].number, Some(6));
    assert_eq!(d.episodes[0].duration_secs, Some(1720));
    assert_eq!(d.episodes[0].synopsis.as_deref(), Some("Sue Perkins hosts."));
    assert_eq!(
        d.episodes[2].external_id, "41785090003",
        "no guid: the episode id"
    );
    assert_eq!(d.episodes[2].series_external_id, "specials");

    // Cleared credentials turn it off again.
    p.configure(&RadioSettingsDto::default());
    assert!(!p.is_configured());
}

// ---------------------------------------------------------------- Internet Archive

const ITEM: &str = "shf58jdhqzopvquyh9ijaob0prcp2diycy4zxqfz";

#[tokio::test]
async fn archive_search_and_items() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/advancedsearch.php"))
        .and(query_param(
            "q",
            "(dragnet) AND mediatype:audio AND (collection:oldtimeradio OR subject:radio)",
        ))
        .and(query_param("output", "json"))
        .respond_with(json("archive_search.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/metadata/{ITEM}")))
        .respond_with(json("archive_metadata.json"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/metadata/missing"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&s)
        .await;
    let p = ArchiveProvider::new(reqwest::Client::new())
        .with_base_url(&s.uri())
        .with_interval(Duration::ZERO);
    let hits = p.search("dragnet", &SearchPrefs::default()).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].provider, "archive");
    assert_eq!(hits[0].external_id, ITEM);
    assert_eq!(
        hits[0].web_url.as_deref(),
        Some(format!("https://archive.org/details/{ITEM}").as_str())
    );

    let d = p
        .preview(&format!("https://archive.org/details/{ITEM}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.title, "Jeff Regan - 2 Episodes From 1948");
    assert_eq!(d.author.as_deref(), Some("Boxcars711 Old Time Radio"));
    // The FLAC and the MP3 made from it are one episode (the MP3 is kept).
    assert_eq!(d.episodes.len(), 2);
    let flac = d.episodes.iter().find(|e| e.number == Some(2)).unwrap();
    assert_eq!(flac.external_id, "Jeff_Regan_49-10-15_The_Lady_With_The_Lute.mp3");
    assert_eq!(flac.title, "The Lady With The Lute");
    assert_eq!(flac.duration_secs, Some(1781));
    assert_eq!(
        flac.enclosure_url.as_deref(),
        Some(
            format!(
                "{}/download/{ITEM}/Jeff_Regan_49-10-15_The_Lady_With_The_Lute.mp3",
                s.uri()
            )
            .as_str()
        )
    );
    let first = d.episodes.iter().find(|e| e.number == Some(1)).unwrap();
    assert_eq!(first.duration_secs, Some(3613));
    assert!(matches!(
        p.show("missing", &full(0)).await,
        Err(ProviderError::NotFound(_))
    ));
}

// ---------------------------------------------------------------- choosing sources

#[test]
fn search_sources_follow_country_and_language() {
    let http = reqwest::Client::new();
    let reg = ProviderRegistry::default()
        .with(Arc::new(AppleProvider::new(http.clone())))
        .with(Arc::new(BbcProvider::new(http.clone())))
        .with(Arc::new(PodcastIndexProvider::new(http.clone())))
        .with(Arc::new(ArchiveProvider::new(http.clone())))
        .with(Arc::new(RssProvider::new(http)));
    let ids = |prefs: SearchPrefs| -> Vec<&'static str> {
        reg.for_search(&[], &prefs).iter().map(|p| p.id()).collect()
    };
    // Podcast Index is unconfigured, rss cannot search.
    assert_eq!(ids(SearchPrefs::default()), ["bbc", "apple", "archive"]);
    let gb = SearchPrefs {
        country: Some("GB".into()),
        language: Some("en".into()),
    };
    assert_eq!(ids(gb), ["bbc", "apple", "archive"]);
    let us = SearchPrefs {
        country: Some("US".into()),
        language: None,
    };
    assert_eq!(ids(us), ["apple", "archive"]);
    let de = SearchPrefs {
        country: None,
        language: Some("de".into()),
    };
    assert_eq!(ids(de), ["apple", "archive"]);
    assert_eq!(
        reg.for_search(&["archive".to_owned()], &SearchPrefs::default())
            .iter()
            .map(|p| p.id())
            .collect::<Vec<_>>(),
        ["archive"]
    );
    let dtos = reg.dtos(&[]);
    let pi = dtos.iter().find(|d| d.id == "podcastindex").unwrap();
    assert!(pi.supports_search && !pi.enabled, "off until configured");
}

// ---------------------------------------------------------------- artwork and logos

/// A provider whose artwork and logo URLs the test changes.
struct Pictures {
    artwork: Mutex<String>,
    logo: String,
}

#[async_trait::async_trait]
impl RadioProvider for Pictures {
    fn id(&self) -> &'static str {
        "pics"
    }
    fn name(&self) -> &'static str {
        "Pictures"
    }
    async fn preview(&self, _link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(None)
    }
    async fn show(&self, _id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        Ok(ShowData {
            external_id: "s1".into(),
            title: "Pictured".into(),
            artwork_url: Some(self.artwork.lock().unwrap().clone()),
            station: Some(StationData {
                external_id: "st".into(),
                name: "Station".into(),
                logo_url: Some(self.logo.clone()),
            }),
            complete: true,
            ..ShowData::default()
        })
    }
}

fn jpeg(len: usize) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
    v.resize(len, 7);
    v
}

#[tokio::test]
async fn artwork_and_logos_are_downloaded_on_refresh() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/art.jpg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(jpeg(1000)),
        )
        .expect(1) // not again while the URL stays the same
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/logo.svg"))
        .respond_with(
            ResponseTemplate::new(200)
                // A wrong type is not trusted: the bytes decide.
                .insert_header("content-type", "text/plain")
                .set_body_string(r#"<svg onload="alert(1)"><script>alert(2)</script><circle r="4"/></svg>"#),
        )
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/big.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes({
            let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
            v.resize(2 * 1024 * 1024 + 1, 0);
            v
        }))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/page.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_string("<html></html>"),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/art2.webp"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFF\x10\0\0\0WEBPVP8 rest".to_vec()))
        .mount(&s)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let pics = Arc::new(Pictures {
        artwork: Mutex::new(format!("{}/art.jpg", s.uri())),
        logo: format!("{}/logo.svg", s.uri()),
    });
    let svc = RadioService::new(RadioDeps {
        db: Db::in_memory().await.unwrap(),
        events: EventBus::default(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(pics.clone()),
        library_sync: None,
    });
    svc.set_data_dir(dir.path().to_path_buf());
    let show = svc
        .add_show(AddRadioShowRequest {
            provider: Some("pics".into()),
            external_id: Some("s1".into()),
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    assert!(show.has_artwork);
    let id = show.id;
    let rel = svc.show_artwork_path(id).await.unwrap().unwrap();
    assert_eq!(rel, format!("radio/artwork/{id}.jpg"));
    assert!(dir.path().join(&rel).is_file());
    let img = svc.show_artwork(id).await.unwrap().unwrap();
    assert_eq!(img.content_type, "image/jpeg");
    assert_eq!(img.bytes.len(), 1000);
    assert!(img.etag.starts_with('"') && img.etag.ends_with('"'));

    let station = show.station_id.unwrap();
    let logo = svc.station_logo(station).await.unwrap().unwrap();
    assert_eq!(logo.content_type, "image/svg+xml");
    let text = String::from_utf8(logo.bytes).unwrap();
    assert!(
        text.contains("<circle r=\"4\"/>") && !text.contains("script") && !text.contains("onload"),
        "{text}"
    );
    assert!(svc.get_station(station).await.unwrap().has_logo);

    // Same URLs: nothing downloaded again (expect(1) above).
    svc.refresh_show(id).await.unwrap();

    // Too large, or not an image: the saved artwork stays.
    for bad in ["big.png", "page.html", "missing.jpg"] {
        *pics.artwork.lock().unwrap() = format!("{}/{bad}", s.uri());
        svc.refresh_show(id).await.unwrap();
        assert_eq!(svc.show_artwork_path(id).await.unwrap().unwrap(), rel, "{bad}");
        assert_eq!(svc.show_artwork(id).await.unwrap().unwrap().bytes.len(), 1000);
    }

    // A new URL replaces the file (and the old type's file).
    *pics.artwork.lock().unwrap() = format!("{}/art2.webp", s.uri());
    svc.refresh_show(id).await.unwrap();
    assert_eq!(
        svc.show_artwork_path(id).await.unwrap().unwrap(),
        format!("radio/artwork/{id}.webp")
    );
    assert_eq!(
        svc.show_artwork(id).await.unwrap().unwrap().content_type,
        "image/webp"
    );
    assert!(!dir.path().join(&rel).exists());
}

#[tokio::test]
async fn no_data_folder_means_no_downloads() {
    let svc = RadioService::new(RadioDeps {
        db: Db::in_memory().await.unwrap(),
        events: EventBus::default(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(Arc::new(Pictures {
            artwork: Mutex::new("http://127.0.0.1:9/never.jpg".into()),
            logo: "http://127.0.0.1:9/never.svg".into(),
        })),
        library_sync: None,
    });
    let show = svc
        .add_show(AddRadioShowRequest {
            provider: Some("pics".into()),
            external_id: Some("s1".into()),
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    assert!(!show.has_artwork);
    assert!(svc.show_artwork(show.id).await.unwrap().is_none());
}

// ---------------------------------------------------------------- credentials

#[tokio::test]
async fn podcastindex_secret_is_encrypted_and_masked() {
    let db = Db::in_memory().await.unwrap();
    let http = reqwest::Client::new();
    let svc = RadioService::new(RadioDeps {
        db: db.clone(),
        events: EventBus::default(),
        http: http.clone(),
        providers: ProviderRegistry::default()
            .with(Arc::new(PodcastIndexProvider::new(http.clone())))
            .with(Arc::new(RssProvider::new(http))),
        library_sync: None,
    });
    let with_secret = RadioSettingsDto {
        podcastindex_api_key: Some(" KEY ".into()),
        podcastindex_api_secret: Some("SECRET".into()),
        ..RadioSettingsDto::default()
    };
    // Without a secret key attached, credentials are refused rather than stored in clear.
    assert!(svc.set_settings(with_secret.clone()).await.is_err());

    svc.set_secrets(SecretBox::from_key([9; 32]));
    let saved = svc.set_settings(with_secret).await.unwrap();
    assert_eq!(saved.podcastindex_api_key.as_deref(), Some("KEY"));
    assert_eq!(saved.podcastindex_api_secret.as_deref(), Some("********"));
    let raw: serde_json::Value = db.settings().get(keys::SETTINGS).await.unwrap().unwrap();
    let stored = raw["podcastindex_api_secret"].as_str().unwrap();
    assert!(
        stored.starts_with("enc:") && !stored.contains("SECRET"),
        "{stored}"
    );
    assert_eq!(
        svc.settings().await.unwrap().podcastindex_api_secret.as_deref(),
        Some("SECRET")
    );
    assert_eq!(
        svc.settings_masked()
            .await
            .unwrap()
            .podcastindex_api_secret
            .as_deref(),
        Some("********")
    );
    let pi = |dtos: Vec<RadioProviderDto>| dtos.into_iter().find(|d| d.id == "podcastindex").unwrap();
    assert!(pi(svc.provider_list().await.unwrap()).enabled);

    // Sending the mask back keeps the secret.
    let mut again = svc.settings_masked().await.unwrap();
    again.max_lookups_per_refresh = 10;
    svc.set_settings(again).await.unwrap();
    assert_eq!(
        svc.settings().await.unwrap().podcastindex_api_secret.as_deref(),
        Some("SECRET")
    );

    // Clearing it turns the source off.
    let mut cleared = svc.settings_masked().await.unwrap();
    cleared.podcastindex_api_secret = None;
    svc.set_settings(cleared).await.unwrap();
    assert!(!pi(svc.provider_list().await.unwrap()).enabled);
}
