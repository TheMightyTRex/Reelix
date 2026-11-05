//! Library sync and automatic matching of podcast library files to episodes
//! (`RadioService::sync_library_files` / `match_library`).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use reelix_api_types::radio::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_radio::*;

fn day(m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, m, d, 12, 0, 0).unwrap()
}

/// A provider serving fixed shows by external id.
struct Fake {
    id: &'static str,
    shows: HashMap<String, ShowData>,
}

#[async_trait::async_trait]
impl RadioProvider for Fake {
    fn id(&self) -> &'static str {
        self.id
    }
    fn name(&self) -> &'static str {
        self.id
    }
    async fn search(&self, _q: &str, _p: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Ok(vec![])
    }
    async fn preview(&self, _link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(None)
    }
    async fn show(&self, external_id: &str, _f: &ShowFetch) -> Result<ShowData, ProviderError> {
        self.shows
            .get(external_id)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(external_id.into()))
    }
}

fn ep(id: &str, series: &str, number: Option<u32>, title: &str) -> EpisodeData {
    EpisodeData {
        external_id: id.into(),
        series_external_id: series.into(),
        number,
        title: title.into(),
        first_broadcast: Some(day(1, 1 + number.unwrap_or(0))),
        ..EpisodeData::default()
    }
}

fn show(external_id: &str, title: &str, series_number: u32, episodes: Vec<EpisodeData>) -> ShowData {
    ShowData {
        external_id: external_id.into(),
        title: title.into(),
        series: vec![SeriesData {
            external_id: "s".into(),
            number: Some(series_number),
            ..SeriesData::default()
        }],
        episodes,
        complete: true,
        ..ShowData::default()
    }
}

fn archers() -> ShowData {
    let mut e1 = ep("a1", "s", Some(1), "Ambridge One");
    e1.guid = Some("urn:archers:1".into());
    let mut e2 = ep("a2", "s", Some(2), "Ambridge Two");
    e2.enclosure_url = Some("https://media.example/archers/2.mp3".into());
    show(
        "archers",
        "The Archers",
        20,
        vec![
            e1,
            e2,
            ep("a3", "s", Some(3), "Ambridge Three"),
            ep("a4", "s", Some(4), "Ambridge Four"),
            ep("a5", "s", Some(5), "Ambridge Five"),
            ep("a6", "s", Some(6), "Ambridge Six"),
            ep("a7", "s", Some(7), "The Harvest Supper"),
            ep("a8", "s", Some(8), "Christmas Special"),
        ],
    )
}

fn quiz() -> ShowData {
    show(
        "quiz",
        "The News Quiz",
        1,
        vec![
            ep("q1", "s", Some(1), "Episode One"),
            ep("q2", "s", Some(2), "Christmas Special"),
        ],
    )
}

fn bbc_show() -> ShowData {
    show(
        "b006qykl",
        "In Our Time",
        1,
        vec![
            ep("b0abc123", "s", Some(1), "Consciousness"),
            ep("p0xyz789", "s", Some(2), "The Silk Road"),
        ],
    )
}

struct Ctx {
    svc: RadioService,
}

async fn ctx() -> Ctx {
    let fake = Fake {
        id: "fake",
        shows: [("archers", archers()), ("quiz", quiz())]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    };
    let bbc = Fake {
        id: "bbc",
        shows: HashMap::from([("b006qykl".to_owned(), bbc_show())]),
    };
    let svc = RadioService::new(RadioDeps {
        db: Db::in_memory().await.unwrap(),
        events: EventBus::default(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default()
            .with(Arc::new(bbc))
            .with(Arc::new(fake)),
        library_sync: None,
    });
    svc.set_now(day(4, 1));
    Ctx { svc }
}

impl Ctx {
    async fn add(&self, provider: &str, external_id: &str, follow: RadioFollow) -> RadioShowDto {
        self.svc
            .add_show(AddRadioShowRequest {
                provider: Some(provider.into()),
                external_id: Some(external_id.into()),
                follow: Some(follow),
                monitored: true,
                ..AddRadioShowRequest::default()
            })
            .await
            .unwrap()
    }

    async fn episodes(&self, show_id: i64) -> HashMap<String, RadioEpisodeDto> {
        self.svc
            .show_episodes(show_id)
            .await
            .unwrap()
            .episodes
            .into_iter()
            .map(|e| (e.external_id.clone(), e))
            .collect()
    }

    /// `external id → abs_episode_id` of the matched file.
    async fn links(&self, show_id: i64) -> HashMap<String, String> {
        let files: HashMap<i64, String> = self
            .svc
            .episode_files()
            .await
            .unwrap()
            .into_iter()
            .map(|f| (f.id, f.abs_episode_id))
            .collect();
        self.episodes(show_id)
            .await
            .into_iter()
            .filter_map(|(k, e)| Some((k, files[&e.file_id?].clone())))
            .collect()
    }

    async fn file_id(&self, abs_episode_id: &str) -> i64 {
        self.svc
            .episode_files()
            .await
            .unwrap()
            .into_iter()
            .find(|f| f.abs_episode_id == abs_episode_id)
            .unwrap()
            .id
    }
}

fn file(item: &str, item_title: &str, id: &str) -> NewEpisodeFile {
    NewEpisodeFile {
        abs_item_id: item.into(),
        abs_episode_id: id.into(),
        item_title: Some(item_title.into()),
        title: Some(format!("Untitled {id}")),
        ..NewEpisodeFile::default()
    }
}

fn with(mut f: NewEpisodeFile, edit: impl FnOnce(&mut NewEpisodeFile)) -> NewEpisodeFile {
    edit(&mut f);
    f
}

fn links(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[tokio::test]
async fn rule_1_source_ids_in_guid_enclosure_or_file_name() {
    let c = ctx().await;
    let archers = c.add("fake", "archers", RadioFollow::All).await;
    let iot = c.add("bbc", "b006qykl", RadioFollow::All).await;
    // Rule 1 does not need the item title to match the show.
    let files = vec![
        with(file("li_x", "Unrelated", "f_guid"), |f| {
            f.guid = Some("urn:archers:1".into())
        }),
        with(file("li_x", "Unrelated", "f_encl"), |f| {
            f.enclosure_url = Some(" https://media.example/archers/2.mp3".into())
        }),
        // BBC programme pids in the file name and in the enclosure address.
        with(file("li_iot", "IOT", "f_pid1"), |f| {
            f.path = Some("/podcasts/IOT/In Our Time - B0ABC123 - Consciousness.mp3".into())
        }),
        with(file("li_iot", "IOT", "f_pid2"), |f| {
            f.enclosure_url = Some("https://open.live.bbc.co.uk/p0xyz789.mp3".into())
        }),
        // A longer token containing the pid is not the pid.
        with(file("li_iot", "IOT", "f_near"), |f| {
            f.path = Some("/podcasts/IOT/xb0abc1234.mp3".into())
        }),
    ];
    let s = c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(
        s,
        LibrarySyncSummary {
            files_seen: 5,
            files_removed: 0,
            episodes_matched: 4
        }
    );
    assert_eq!(
        c.links(archers.id).await,
        links(&[("a1", "f_guid"), ("a2", "f_encl")])
    );
    assert_eq!(
        c.links(iot.id).await,
        links(&[("b0abc123", "f_pid1"), ("p0xyz789", "f_pid2")])
    );
    let eps = c.episodes(archers.id).await;
    assert_eq!(eps["a1"].status, RadioEpisodeStatus::InLibrary);
    assert!(!eps["a1"].manual_match);
    assert_eq!(eps["a3"].status, RadioEpisodeStatus::Wanted);
    // A second sync with the same files changes nothing.
    let s = c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(s.episodes_matched, 0);
    assert_eq!(c.links(iot.id).await.len(), 2);
    assert!(c.svc.library_review().await.unwrap().synced_at.is_some());
}

#[tokio::test]
async fn rule_2_series_and_episode_numbers_within_the_show() {
    let c = ctx().await;
    let archers = c.add("fake", "archers", RadioFollow::All).await;
    // Accents, punctuation, case and a leading "The" do not matter.
    let item = "Thé ÁRCHERS!";
    let files = vec![
        with(file("li_a", item, "f_t"), |f| {
            f.title = Some("Ambridge S20E03 rerun".into())
        }),
        with(file("li_a", item, "f_abs"), |f| {
            f.season = Some(20);
            f.episode = Some(4);
        }),
        with(file("li_a", item, "f_name"), |f| {
            f.path = Some("/podcasts/The Archers/The Archers - s20e05 - x.mp3".into())
        }),
        // Episode 6 twice: ambiguous, nothing matched.
        with(file("li_a", item, "f_dup1"), |f| f.title = Some("S20E06".into())),
        with(file("li_a", item, "f_dup2"), |f| {
            f.season = Some(20);
            f.episode = Some(6);
        }),
        // Another podcast with the right number: not this show.
        with(file("li_other", "Gardeners' Question Time", "f_other"), |f| {
            f.title = Some("S20E01".into())
        }),
        // No season from Audiobookshelf counts as series 1 (here: no match in series 20).
        with(file("li_a", item, "f_noseason"), |f| f.episode = Some(1)),
    ];
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(
        c.links(archers.id).await,
        links(&[("a3", "f_t"), ("a4", "f_abs"), ("a5", "f_name")])
    );
    // The show is matched to one item: it gets that item.
    assert_eq!(
        c.svc.get_show(archers.id).await.unwrap().abs_item_id.as_deref(),
        Some("li_a")
    );
    let review = c.svc.library_review().await.unwrap();
    assert_eq!(
        review
            .unmatched_shows
            .iter()
            .map(|s| s.abs_item_id.as_str())
            .collect::<Vec<_>>(),
        ["li_other"]
    );
    let mut unmatched: Vec<String> = review
        .unmatched_files
        .into_iter()
        .map(|f| f.abs_episode_id)
        .collect();
    unmatched.sort();
    assert_eq!(unmatched, ["f_dup1", "f_dup2", "f_noseason", "f_other"]);
}

#[tokio::test]
async fn rule_2_uses_the_mapped_item_and_series_1_for_seasonless_files() {
    let c = ctx().await;
    let quiz = c.add("fake", "quiz", RadioFollow::All).await;
    // Mapped by hand to an item with another title; same-titled items are then ignored.
    c.svc
        .update_show(
            quiz.id,
            RadioShowEdit {
                abs_item_id: Some(Some("li_mapped".into())),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    let files = vec![
        with(file("li_mapped", "BBC Comedy Quiz", "f_q1"), |f| {
            f.episode = Some(1)
        }),
        with(file("li_same_title", "The News Quiz", "f_q2"), |f| {
            f.title = Some("S01E02".into())
        }),
    ];
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(c.links(quiz.id).await, links(&[("q1", "f_q1")]));
}

#[tokio::test]
async fn rule_3_unique_titles_only() {
    let c = ctx().await;
    let archers = c.add("fake", "archers", RadioFollow::All).await;
    let quiz = c.add("fake", "quiz", RadioFollow::All).await;
    let files = vec![
        // Item title unrelated: only the episode title can match it.
        with(file("li_z", "Misc", "f_harvest"), |f| {
            f.title = Some("the harvest-supper".into())
        }),
        // "Christmas Special" is an episode of both shows: not matched.
        with(file("li_z", "Misc", "f_xmas"), |f| {
            f.title = Some("Christmas Special".into())
        }),
        // "Episode One" twice in the library: not matched.
        with(file("li_z", "Misc", "f_one_a"), |f| {
            f.title = Some("Episode One".into())
        }),
        with(file("li_y", "Misc 2", "f_one_b"), |f| {
            f.title = Some("Episode One".into())
        }),
    ];
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(c.links(archers.id).await, links(&[("a7", "f_harvest")]));
    assert!(c.links(quiz.id).await.is_empty());
    // Two items were matched to nothing / one show by title only: no abs_item_id from a
    // title-only match of several items.
    assert_eq!(c.svc.get_show(quiz.id).await.unwrap().abs_item_id, None);
}

#[tokio::test]
async fn rules_apply_in_order_and_give_one_file_per_episode() {
    let c = ctx().await;
    let archers = c.add("fake", "archers", RadioFollow::All).await;
    let item = "The Archers";
    let files = vec![
        // Matches a1 by guid; its title also says S20E02 and "Ambridge Three".
        with(file("li_a", item, "f1"), |f| {
            f.guid = Some("urn:archers:1".into());
            f.title = Some("S20E02 Ambridge Three".into());
        }),
        // Also carries a1's guid: a1 already has a file, so this one is left over.
        with(file("li_a", item, "f1_copy"), |f| {
            f.guid = Some("urn:archers:1".into());
            f.title = Some("copy".into());
        }),
        // S20E02 is now unique among the files left: a2.
        with(file("li_a", item, "f2"), |f| f.title = Some("S20E02".into())),
    ];
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(c.links(archers.id).await, links(&[("a1", "f1"), ("a2", "f2")]));
}

#[tokio::test]
async fn hand_set_links_and_not_this_file_are_kept() {
    let c = ctx().await;
    let archers = c.add("fake", "archers", RadioFollow::All).await;
    let item = "The Archers";
    let files = vec![
        with(file("li_a", item, "f1"), |f| {
            f.guid = Some("urn:archers:1".into())
        }),
        with(file("li_a", item, "f3"), |f| f.title = Some("S20E03".into())),
        with(file("li_a", item, "f_other"), |f| f.title = Some("bonus".into())),
    ];
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(c.links(archers.id).await, links(&[("a1", "f1"), ("a3", "f3")]));
    let eps = c.episodes(archers.id).await;

    // a3 is linked by hand to another file; f1 (a1's by guid) is linked by hand to a4.
    let other = c.file_id("f_other").await;
    c.svc.set_episode_file(eps["a3"].id, other).await.unwrap();
    let f1 = c.file_id("f1").await;
    c.svc.set_episode_file(eps["a4"].id, f1).await.unwrap();
    c.svc.sync_library_files(&files).await.unwrap();
    // f3 is free again but a3 keeps its hand-set file; f1 is not offered back to a1.
    assert_eq!(
        c.links(archers.id).await,
        links(&[("a3", "f_other"), ("a4", "f1")])
    );

    // "Not this file" on a4: never matched automatically again…
    c.svc.clear_episode_file(eps["a4"].id, false).await.unwrap();
    c.svc.sync_library_files(&files).await.unwrap();
    let now = c.links(archers.id).await;
    assert_eq!(now.get("a4"), None);
    // …while the freed f1 goes back to a1 by its guid.
    assert_eq!(now.get("a1").map(String::as_str), Some("f1"));
    let e4 = &c.episodes(archers.id).await["a4"];
    assert_eq!((e4.manual_match, e4.status), (true, RadioEpisodeStatus::Wanted));

    // Handing a1 back to automatic matching after "Not this file".
    c.svc.clear_episode_file(eps["a1"].id, false).await.unwrap();
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(c.links(archers.id).await.get("a1"), None);
    c.svc.clear_episode_file(eps["a1"].id, true).await.unwrap();
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(
        c.links(archers.id).await.get("a1").map(String::as_str),
        Some("f1")
    );
}

#[tokio::test]
async fn disappearing_files_return_episodes_to_wanted_or_not_requested() {
    let c = ctx().await;
    let archers = c.add("fake", "archers", RadioFollow::All).await;
    let quiz = c.add("fake", "quiz", RadioFollow::None).await;
    let files = vec![
        with(file("li_a", "The Archers", "f1"), |f| {
            f.guid = Some("urn:archers:1".into())
        }),
        with(file("li_q", "The News Quiz", "fq"), |f| {
            f.title = Some("S01E01".into())
        }),
    ];
    c.svc.sync_library_files(&files).await.unwrap();
    assert_eq!(
        c.episodes(archers.id).await["a1"].status,
        RadioEpisodeStatus::InLibrary
    );
    assert_eq!(
        c.episodes(quiz.id).await["q1"].status,
        RadioEpisodeStatus::InLibrary
    );

    c.svc.set_now(day(4, 2));
    let s = c.svc.sync_library_files(&[]).await.unwrap();
    assert_eq!((s.files_seen, s.files_removed), (0, 2));
    let a1 = &c.episodes(archers.id).await["a1"];
    assert_eq!((a1.status, a1.file_id), (RadioEpisodeStatus::Wanted, None));
    let q1 = &c.episodes(quiz.id).await["q1"];
    assert_eq!((q1.status, q1.file_id), (RadioEpisodeStatus::NotRequested, None));
    assert!(c.svc.episode_files().await.unwrap().is_empty());
    assert_eq!(c.svc.followed_show_count().await.unwrap(), 1);
}

#[test]
fn source_ids_per_provider() {
    assert_eq!(source_ids("bbc", "b0abc123"), ["b0abc123"]);
    assert!(source_ids("rss", "https://feed.example/1").is_empty());
    assert_eq!(clean_title("Thé Árchers!"), clean_title("the archers"));
    assert_eq!(clean_title("The Archers"), clean_title("Archers"));
}
