//! Radio service flows against a fake provider, and the podcast feed provider against saved
//! feeds served by a mock server.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use reelix_api_types::content::CreateContentRootFolderRequest;
use reelix_api_types::radio::*;
use reelix_core::ContentKind;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_radio::providers::rss::{self, RssProvider};
use reelix_radio::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------- fixtures

const SEASONS: &str = include_str!("fixtures/seasons.xml");
const PLAIN: &str = include_str!("fixtures/plain.xml");

fn day(m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, m, d, 12, 0, 0).unwrap()
}

fn ep(id: &str, series: &str, number: Option<u32>, at: Option<DateTime<Utc>>) -> EpisodeData {
    EpisodeData {
        external_id: id.into(),
        series_external_id: series.into(),
        number,
        title: format!("Episode {id}"),
        first_broadcast: at,
        duration_secs: Some(1800),
        ..EpisodeData::default()
    }
}

fn series(id: &str, number: Option<u32>, expected: Option<u32>) -> SeriesData {
    SeriesData {
        external_id: id.into(),
        number,
        expected_episodes: expected,
        ..SeriesData::default()
    }
}

/// A provider whose show data the test changes between refreshes, recording each fetch.
#[derive(Default)]
struct FakeProvider {
    data: Mutex<ShowData>,
    fetches: Mutex<Vec<ShowFetch>>,
}

impl FakeProvider {
    fn set(&self, f: impl FnOnce(&mut ShowData)) {
        f(&mut self.data.lock().unwrap());
    }
}

#[async_trait::async_trait]
impl RadioProvider for FakeProvider {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake broadcaster"
    }
    fn supports_search(&self) -> bool {
        true
    }
    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let d = self.data.lock().unwrap().clone();
        Ok(if d.title.to_lowercase().contains(&query.to_lowercase()) {
            vec![d.hit("fake")]
        } else {
            vec![]
        })
    }
    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(link
            .starts_with("https://fake.example/")
            .then(|| self.data.lock().unwrap().clone()))
    }
    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        self.fetches.lock().unwrap().push(fetch.clone());
        let mut d = self.data.lock().unwrap().clone();
        if d.external_id != external_id {
            return Err(ProviderError::NotFound(external_id.into()));
        }
        // Like a real provider, leave skipped series out.
        d.series.retain(|s| !fetch.skip_series.contains(&s.external_id));
        d.episodes
            .retain(|e| !fetch.skip_series.contains(&e.series_external_id));
        Ok(d)
    }
}

fn show_data() -> ShowData {
    ShowData {
        external_id: "p001".into(),
        title: "The Archers".into(),
        synopsis: Some("Contemporary drama in a rural setting".into()),
        station: Some(StationData {
            external_id: "radio4".into(),
            name: "Radio 4".into(),
            logo_url: Some("https://fake.example/r4.svg".into()),
        }),
        series: vec![series("s1", Some(1), Some(2)), series("s2", None, None)],
        episodes: vec![
            ep("e1", "s1", Some(1), Some(day(1, 1))),
            ep("e2", "s1", Some(2), Some(day(1, 8))),
            // No numbers: numbered in broadcast order (e4 before e3).
            ep("e3", "s2", None, Some(day(3, 10))),
            ep("e4", "s2", None, Some(day(3, 3))),
        ],
        complete: true,
        ..ShowData::default()
    }
}

struct Ctx {
    svc: RadioService,
    fake: Arc<FakeProvider>,
}

async fn ctx() -> Ctx {
    let db = Db::in_memory().await.unwrap();
    let fake = Arc::new(FakeProvider::default());
    fake.set(|d| *d = show_data());
    let http = reqwest::Client::new();
    let svc = RadioService::new(RadioDeps {
        db,
        events: EventBus::default(),
        http: http.clone(),
        providers: ProviderRegistry::default()
            .with(fake.clone())
            .with(Arc::new(RssProvider::new(http))),
        library_sync: None,
    });
    svc.set_now(day(4, 1));
    Ctx { svc, fake }
}

async fn add(c: &Ctx, follow: RadioFollow) -> RadioShowDto {
    c.svc
        .add_show(AddRadioShowRequest {
            provider: Some("fake".into()),
            external_id: Some("p001".into()),
            follow: Some(follow),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap()
}

fn by_ext<'a>(eps: &'a [RadioEpisodeDto], id: &str) -> &'a RadioEpisodeDto {
    eps.iter().find(|e| e.external_id == id).unwrap()
}

// ---------------------------------------------------------------- service

#[tokio::test]
async fn add_show_saves_structure_station_and_numbers() {
    let c = ctx().await;
    let show = add(&c, RadioFollow::All).await;
    assert_eq!(show.title, "The Archers");
    assert_eq!(show.sort_title, "Archers");
    assert_eq!(show.station_name.as_deref(), Some("Radio 4"));
    assert_eq!(show.statistics.series_count, 2);
    assert_eq!(show.statistics.episode_count, 4);
    assert_eq!(show.statistics.wanted_count, 4);

    let eps = c.svc.show_episodes(show.id).await.unwrap();
    let s2 = eps.series.iter().find(|s| s.external_id == "s2").unwrap();
    assert_eq!(s2.number, Some(2), "unnumbered series takes the next free number");
    assert_eq!(s2.number_source, RadioNumberSource::BroadcastOrder);
    let e4 = by_ext(&eps.episodes, "e4");
    assert_eq!(
        (e4.number, e4.number_source),
        (Some(1), RadioNumberSource::BroadcastOrder)
    );
    assert_eq!(by_ext(&eps.episodes, "e3").number, Some(2));
    assert_eq!(
        by_ext(&eps.episodes, "e1").number_source,
        RadioNumberSource::Source
    );
    // Ordered by series then number.
    let order: Vec<&str> = eps.episodes.iter().map(|e| e.external_id.as_str()).collect();
    assert_eq!(order, ["e1", "e2", "e4", "e3"]);

    let stations = c.svc.stations().await.unwrap();
    assert_eq!(stations.len(), 1);
    assert_eq!(stations[0].show_count, 1);

    // The same show again is a conflict; search marks it as added.
    let again = c
        .svc
        .add_show(AddRadioShowRequest {
            link: Some("https://fake.example/p001".into()),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await;
    assert!(matches!(again, Err(RadioError::Conflict(_))), "{again:?}");
    let hits = c.svc.search("archers", None).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].show_id, Some(show.id));

    // Filters.
    let page = c
        .svc
        .list_shows(&RadioShowListQuery {
            followed: Some(false),
            ..RadioShowListQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(page.total, 0);
    let page = c
        .svc
        .list_shows(&RadioShowListQuery {
            station_id: Some(stations[0].id),
            page: Some(1),
            page_size: Some(10),
            ..RadioShowListQuery::default()
        })
        .await
        .unwrap();
    assert_eq!((page.total, page.items.len()), (1, 1));
}

#[tokio::test]
async fn follow_choices_decide_new_episode_statuses() {
    // none: everything is Not requested, also what a refresh adds.
    let c = ctx().await;
    let show = add(&c, RadioFollow::None).await;
    assert_eq!(show.statistics.wanted_count, 0);
    c.fake
        .set(|d| d.episodes.push(ep("e5", "s2", None, Some(day(3, 20)))));
    assert_eq!(c.svc.refresh_show(show.id).await.unwrap(), 1);
    let eps = c.svc.show_episodes(show.id).await.unwrap().episodes;
    assert!(eps.iter().all(|e| e.status == RadioEpisodeStatus::NotRequested));

    // from_now_on: only episodes broadcast after following; future ones are Not released.
    let c = ctx().await;
    let show = add(&c, RadioFollow::FromNowOn).await;
    assert_eq!(
        show.statistics.wanted_count, 0,
        "everything listed predates following"
    );
    c.svc.set_now(day(4, 10));
    c.fake.set(|d| {
        d.episodes.push(ep("new", "s2", None, Some(day(4, 5))));
        d.episodes.push(ep("old", "s2", None, Some(day(3, 25))));
        d.episodes.push(ep("soon", "s2", None, Some(day(5, 1))));
        d.episodes.push(ep("undated", "s2", None, None));
    });
    assert_eq!(c.svc.refresh_show(show.id).await.unwrap(), 4);
    let eps = c.svc.show_episodes(show.id).await.unwrap().episodes;
    assert_eq!(by_ext(&eps, "new").status, RadioEpisodeStatus::Wanted);
    assert_eq!(by_ext(&eps, "old").status, RadioEpisodeStatus::NotRequested);
    assert_eq!(by_ext(&eps, "soon").status, RadioEpisodeStatus::NotReleased);
    assert_eq!(by_ext(&eps, "undated").status, RadioEpisodeStatus::Wanted);
    assert_eq!(by_ext(&eps, "e1").status, RadioEpisodeStatus::NotRequested);
    // Undated episodes number last in broadcast order.
    assert_eq!(by_ext(&eps, "undated").number, Some(6));

    // all, then switching to none and back re-applies the choice.
    let c = ctx().await;
    let show = add(&c, RadioFollow::All).await;
    let s = c
        .svc
        .update_show(
            show.id,
            RadioShowEdit {
                follow: Some(RadioFollow::None),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.statistics.wanted_count, 0);
    let s = c
        .svc
        .update_show(
            show.id,
            RadioShowEdit {
                follow: Some(RadioFollow::All),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.statistics.wanted_count, 4);
    // An unmonitored show wants nothing.
    let s = c
        .svc
        .update_show(
            show.id,
            RadioShowEdit {
                monitored: Some(false),
                tags: Some(vec![3]),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((s.statistics.wanted_count, s.tags.clone()), (0, vec![3]));
}

#[tokio::test]
async fn dropped_items_are_kept_and_come_back() {
    let c = ctx().await;
    let show = add(&c, RadioFollow::All).await;
    c.fake.set(|d| {
        d.episodes.retain(|e| e.external_id != "e3");
        d.title = "The Archers Omnibus".into();
        d.synopsis = None;
    });
    c.svc.refresh_show(show.id).await.unwrap();
    let eps = c.svc.show_episodes(show.id).await.unwrap();
    assert_eq!(eps.episodes.len(), 4, "nothing is deleted");
    let e3 = by_ext(&eps.episodes, "e3");
    assert!(e3.source_removed_at.is_some());
    assert_eq!(e3.title, "Episode e3");
    let show = c.svc.get_show(show.id).await.unwrap();
    assert_eq!(show.title, "The Archers Omnibus");
    assert!(show.synopsis.is_some(), "values the source stops giving are kept");
    assert!(show.refreshed_at.is_some());

    // A partial read marks nothing; the item coming back clears the mark.
    c.fake.set(|d| {
        d.episodes.clear();
        d.complete = false;
    });
    c.svc.refresh_show(show.id).await.unwrap();
    let eps = c.svc.show_episodes(show.id).await.unwrap().episodes;
    assert!(by_ext(&eps, "e1").source_removed_at.is_none());
    c.fake.set(|d| *d = show_data());
    c.svc.refresh_show(show.id).await.unwrap();
    let eps = c.svc.show_episodes(show.id).await.unwrap().episodes;
    assert!(eps.iter().all(|e| e.source_removed_at.is_none()));

    // A failing source leaves everything as saved.
    c.fake.set(|d| d.external_id = "other".into());
    assert!(c.svc.refresh_show(show.id).await.is_err());
    assert_eq!(c.svc.show_episodes(show.id).await.unwrap().episodes.len(), 4);
}

#[tokio::test]
async fn settled_series_are_skipped() {
    let c = ctx().await;
    let show = add(&c, RadioFollow::All).await;
    // Series 1 has both expected episodes and its last aired in January (> 60 days before 1 April).
    let s = c.svc.show_episodes(show.id).await.unwrap().series;
    assert!(
        s.iter()
            .find(|x| x.external_id == "s1")
            .unwrap()
            .settled_at
            .is_some()
    );
    assert!(
        s.iter()
            .find(|x| x.external_id == "s2")
            .unwrap()
            .settled_at
            .is_none()
    );

    c.svc.refresh_show(show.id).await.unwrap();
    let fetch = c.fake.fetches.lock().unwrap().last().cloned().unwrap();
    assert!(fetch.skip_series.contains("s1"));
    assert!(!fetch.skip_series.contains("s2"));
    assert_eq!(fetch.max_lookups, 30);
    assert!(fetch.known_episodes.contains("e1"));
    // The skipped series and its episodes are not marked removed.
    let eps = c.svc.show_episodes(show.id).await.unwrap();
    assert!(eps.series.iter().all(|s| s.source_removed_at.is_none()));
    assert!(by_ext(&eps.episodes, "e1").source_removed_at.is_none());
}

#[tokio::test]
async fn manual_matches_and_disappearing_files() {
    let c = ctx().await;
    let show = add(&c, RadioFollow::All).await;
    let eps = c.svc.show_episodes(show.id).await.unwrap().episodes;
    let (e1, e2) = (by_ext(&eps, "e1").id, by_ext(&eps, "e2").id);
    let seen = day(4, 1);
    let ids = c
        .svc
        .upsert_episode_files(
            &[
                NewEpisodeFile {
                    abs_item_id: "li_1".into(),
                    abs_episode_id: "ep_1".into(),
                    item_title: Some("The Archers".into()),
                    title: Some("Episode 1".into()),
                    ..NewEpisodeFile::default()
                },
                NewEpisodeFile {
                    abs_item_id: "li_2".into(),
                    abs_episode_id: "ep_9".into(),
                    item_title: Some("Something else".into()),
                    ..NewEpisodeFile::default()
                },
            ],
            seen,
        )
        .await
        .unwrap();
    let review = c.svc.library_review().await.unwrap();
    assert_eq!(review.unmatched_files.len(), 2);
    // "The Archers" is covered by the show of the same title.
    assert_eq!(review.unmatched_shows.len(), 1);

    let e = c.svc.set_episode_file(e1, ids[0]).await.unwrap();
    assert_eq!(e.status, RadioEpisodeStatus::InLibrary);
    assert!(e.manual_match);
    assert_eq!(e.file.as_ref().unwrap().abs_episode_id, "ep_1");
    // The hand-set link is kept: automatic matching may not move it.
    assert!(!c.svc.link_file_auto(e2, ids[0]).await.unwrap());
    assert!(matches!(
        c.svc.set_episode_file(e2, ids[0]).await,
        Err(RadioError::Conflict(_))
    ));
    let review = c.svc.library_review().await.unwrap();
    assert_eq!(review.unmatched_files.len(), 1);
    assert_eq!(review.unmatched_shows[0].abs_item_id, "li_2");

    // "Not this file" keeps it unmatched; automatic matching leaves it alone.
    let e = c.svc.clear_episode_file(e1, false).await.unwrap();
    assert_eq!(
        (e.status, e.manual_match, e.file_id),
        (RadioEpisodeStatus::Wanted, true, None)
    );
    assert!(!c.svc.link_file_auto(e1, ids[0]).await.unwrap());
    let e = c.svc.clear_episode_file(e1, true).await.unwrap();
    assert!(!e.manual_match);
    assert!(c.svc.link_file_auto(e1, ids[0]).await.unwrap());
    assert_eq!(
        c.svc.get_episode(e1).await.unwrap().status,
        RadioEpisodeStatus::InLibrary
    );

    // The next sync no longer sees the file: the episode is Wanted again.
    c.svc
        .upsert_episode_files(
            &[NewEpisodeFile {
                abs_item_id: "li_2".into(),
                abs_episode_id: "ep_9".into(),
                ..NewEpisodeFile::default()
            }],
            day(4, 2),
        )
        .await
        .unwrap();
    assert_eq!(c.svc.remove_unseen_files(day(4, 2)).await.unwrap(), 1);
    let e = c.svc.get_episode(e1).await.unwrap();
    assert_eq!((e.status, e.file_id), (RadioEpisodeStatus::Wanted, None));
    c.svc.mark_library_synced(day(4, 2)).await.unwrap();
    assert_eq!(c.svc.library_review().await.unwrap().synced_at, Some(day(4, 2)));

    // Deleting the show keeps the library files.
    c.svc.delete_show(show.id).await.unwrap();
    assert!(matches!(
        c.svc.get_show(show.id).await,
        Err(RadioError::NotFound(_))
    ));
    assert_eq!(c.svc.episode_files().await.unwrap().len(), 1);
}

#[tokio::test]
async fn wanted_list_settings_and_root_folders() {
    let c = ctx().await;
    let show = add(&c, RadioFollow::All).await;
    let wanted = c.svc.wanted_episodes(None).await.unwrap();
    assert_eq!(wanted.len(), 4);
    assert_eq!(wanted[0].external_id, "e1");
    assert_eq!((wanted[0].series_number, wanted[0].number), (1, Some(1)));
    assert_eq!(wanted[0].show_id, show.id);

    // Settings: the folder must exist and be writable; codes are normalised.
    let dir = tempfile::tempdir().unwrap();
    let bad = c
        .svc
        .set_settings(RadioSettingsDto {
            library_folder: Some(dir.path().join("missing").to_string_lossy().into_owned()),
            ..RadioSettingsDto::default()
        })
        .await;
    assert!(matches!(bad, Err(RadioError::Validation(m)) if m.contains("mounted")));
    assert!(
        c.svc
            .set_settings(RadioSettingsDto {
                enabled_providers: vec!["nope".into()],
                ..RadioSettingsDto::default()
            })
            .await
            .is_err()
    );
    let s = c
        .svc
        .set_settings(RadioSettingsDto {
            library_folder: Some(dir.path().to_string_lossy().into_owned()),
            abs_library_folder: Some("/podcasts".into()),
            country: Some("gb".into()),
            language: Some("EN".into()),
            enabled_providers: vec!["rss".into(), "rss".into()],
            default_follow: RadioFollow::FromNowOn,
            max_lookups_per_refresh: 0,
            ..RadioSettingsDto::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (s.country.as_deref(), s.language.as_deref()),
        (Some("GB"), Some("en"))
    );
    assert_eq!(s.enabled_providers, ["rss"]);
    assert_eq!(s.max_lookups_per_refresh, 30);
    assert_eq!(c.svc.settings().await.unwrap(), s);
    // The fake provider is now turned off.
    assert!(matches!(
        c.svc.search("archers", Some("fake")).await,
        Err(RadioError::Validation(_))
    ));
    assert!(matches!(
        c.svc.search("archers", None).await,
        Err(RadioError::Validation(_))
    ));

    // Root folders of kind radio.
    let root = c
        .svc
        .add_root_folder(CreateContentRootFolderRequest {
            path: dir.path().to_string_lossy().into_owned(),
            kind: ContentKind::Radio,
        })
        .await
        .unwrap();
    assert_eq!(root.kind, ContentKind::Radio);
    assert!(
        c.svc
            .add_root_folder(CreateContentRootFolderRequest {
                path: "/x".into(),
                kind: ContentKind::Music,
            })
            .await
            .is_err()
    );
    c.svc
        .update_show(
            show.id,
            RadioShowEdit {
                root_folder_id: Some(Some(root.id)),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        c.svc.delete_root_folder(root.id).await,
        Err(RadioError::Conflict(_))
    ));
    c.svc
        .update_show(
            show.id,
            RadioShowEdit {
                root_folder_id: Some(None),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    c.svc.delete_root_folder(root.id).await.unwrap();
    assert!(c.svc.root_folders().await.unwrap().is_empty());
}

// ---------------------------------------------------------------- podcast feeds

#[test]
fn feed_with_seasons_specials_and_entities() {
    let d = rss::parse_feed(SEASONS, "https://feeds.example.com/nq.xml")
        .unwrap()
        .unwrap();
    assert_eq!(d.title, "The News & Quiz");
    assert_eq!(
        d.synopsis.as_deref(),
        Some("Topical comedy panel show.\nEvery Friday.")
    );
    assert_eq!(d.artwork_url.as_deref(), Some("https://example.com/art.jpg"));
    assert_eq!(d.author.as_deref(), Some("Example Radio"));
    assert_eq!(d.web_url.as_deref(), Some("https://example.com/newsquiz"));
    assert_eq!(d.feed_url.as_deref(), Some("https://feeds.example.com/nq.xml"));
    assert_eq!(d.external_id, "https://feeds.example.com/nq.xml");
    assert_eq!(d.episodes.len(), 6);
    let ids: Vec<(&str, RadioSeriesKind, Option<u32>)> = d
        .series
        .iter()
        .map(|s| (s.external_id.as_str(), s.kind, s.number))
        .collect();
    assert_eq!(
        ids,
        [
            ("season:1", RadioSeriesKind::Series, Some(1)),
            ("season:2", RadioSeriesKind::Series, Some(2)),
            ("specials", RadioSeriesKind::Specials, None),
        ]
    );
    let e = |id: &str| d.episodes.iter().find(|e| e.external_id == id).unwrap();
    let s2e2 = e("nq-s2e2");
    assert_eq!(
        (s2e2.series_external_id.as_str(), s2e2.number),
        ("season:2", Some(2))
    );
    assert_eq!(s2e2.duration_secs, Some(28 * 60 + 5));
    assert_eq!(
        s2e2.synopsis.as_deref(),
        Some("Guests & jokes — the week’s news.")
    );
    assert_eq!(
        s2e2.enclosure_url.as_deref(),
        Some("https://cdn.example.com/nq-s2e2.mp3")
    );
    assert_eq!(s2e2.guid.as_deref(), Some("nq-s2e2"));
    assert_eq!(e("nq-s2e1").duration_secs, Some(1685));
    assert_eq!(
        e("nq-s2e1").first_broadcast,
        Some(Utc.with_ymd_and_hms(2024, 1, 5, 18, 30, 0).unwrap())
    );
    assert_eq!(e("nq-xmas").series_external_id, "specials");
    assert_eq!(e("nq-xmas").duration_secs, Some(2700));
    assert_eq!(e("nq-trailer").series_external_id, "specials");
    // No guid: the enclosure address is the id.
    let s1e2 = e("https://cdn.example.com/nq-s1e2.mp3");
    assert_eq!((s1e2.number, s1e2.guid.as_ref()), (None, None));
    assert_eq!(d.first_broadcast, e("nq-trailer").first_broadcast);
}

#[test]
fn feeds_reject_deep_nesting_and_ignore_other_documents() {
    let deep = format!(
        "<rss><channel>{}{}</channel></rss>",
        "<x>".repeat(500),
        "</x>".repeat(500)
    );
    assert!(matches!(
        rss::parse_feed(&deep, "https://x/feed"),
        Err(ProviderError::Parse(m)) if m.contains("nested")
    ));
    assert_eq!(
        rss::parse_feed("<html><body>hi</body></html>", "https://x").unwrap(),
        None
    );
    assert_eq!(
        rss::parse_feed("<feed xmlns=\"http://www.w3.org/2005/Atom\"></feed>", "https://x").unwrap(),
        None
    );
    assert_eq!(rss::parse_feed("{\"json\": true}", "https://x").unwrap(), None);
    assert!(rss::parse_feed("<rss><channel><title>x</rss>", "https://x").is_err());
}

#[tokio::test]
async fn feed_shows_number_in_broadcast_order_and_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/walk.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PLAIN))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/show"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<!doctype html><html><head><link rel="alternate" type="application/rss+xml" href="/walk.xml"></head><body><p>hi<br></body></html>"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>no feed</body></html>"))
        .mount(&server)
        .await;
    let c = ctx().await;

    // A page that advertises its feed previews as the feed; a plain page is not recognised.
    let p = c.svc.preview(&format!("{}/show", server.uri())).await.unwrap();
    assert_eq!(p.show.provider, "rss");
    assert_eq!(p.show.external_id, format!("{}/walk.xml", server.uri()));
    assert_eq!(p.episode_count, 3);
    assert_eq!(p.latest_episodes[0].title, "Walk three");
    assert!(matches!(
        c.svc.preview(&format!("{}/page", server.uri())).await,
        Err(RadioError::Validation(_))
    ));
    assert!(
        c.svc
            .list_shows(&RadioShowListQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );

    let show = c
        .svc
        .add_show(AddRadioShowRequest {
            link: Some(format!("{}/walk.xml", server.uri())),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(show.provider, "rss");
    let eps = c.svc.show_episodes(show.id).await.unwrap();
    assert_eq!(eps.series.len(), 1);
    assert_eq!(
        (eps.series[0].number, eps.series[0].number_source),
        (Some(1), RadioNumberSource::BroadcastOrder)
    );
    let numbered: Vec<(&str, Option<u32>)> = eps
        .episodes
        .iter()
        .map(|e| (e.external_id.as_str(), e.number))
        .collect();
    assert_eq!(
        numbered,
        [("walk-1", Some(1)), ("walk-2", Some(2)), ("walk-3", Some(3))]
    );

    // The hourly feed sync reads feed shows only.
    let r = c.svc.refresh_shows(None, true, false).await.unwrap();
    assert_eq!((r.refreshed, r.failed, r.episodes_added), (1, 0, 0));
    let r = c.svc.refresh_shows(None, false, true).await.unwrap();
    assert_eq!(r.refreshed, 0);
}

#[tokio::test]
async fn feed_fetch_limits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(rss::MAX_FEED_BYTES + 1)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gone.xml"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let p = RssProvider::new(reqwest::Client::new());
    let big = p.preview(&format!("{}/big.xml", server.uri())).await;
    assert!(
        matches!(big, Err(ProviderError::Parse(ref m)) if m.contains("larger")),
        "{big:?}"
    );
    let gone = p
        .show(&format!("{}/gone.xml", server.uri()), &ShowFetch::default())
        .await;
    assert!(matches!(gone, Err(ProviderError::NotFound(_))), "{gone:?}");
    assert_eq!(p.preview("not a link").await.unwrap(), None);
    assert!(!p.supports_search());
    assert!(p.search("x", &SearchPrefs::default()).await.is_err());
}

#[tokio::test]
async fn restricted_reads_use_only_the_public_client() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gone.xml"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let url = format!("{}/gone.xml", server.uri());
    // Without a public client, links read on behalf of users are refused outright.
    let p = RssProvider::new(reqwest::Client::new());
    let refused = rss::restricted(p.show(&url, &ShowFetch::default())).await;
    assert!(
        matches!(refused, Err(ProviderError::Network(ref m)) if m.contains("cannot be read here")),
        "{refused:?}"
    );
    // Outside the scope the shared client is used as before.
    let normal = p.show(&url, &ShowFetch::default()).await;
    assert!(matches!(normal, Err(ProviderError::NotFound(_))), "{normal:?}");
    // With one, restricted reads go through it.
    let p = RssProvider::new(reqwest::Client::new()).with_public_client(reqwest::Client::new());
    let routed = rss::restricted(p.show(&url, &ShowFetch::default())).await;
    assert!(matches!(routed, Err(ProviderError::NotFound(_))), "{routed:?}");
}
