//! Radio through the pipeline: wanted episodes searched in the radio category (Audio/Other)
//! of a wiremock Newznab indexer, scored with the audiobook profile, grabbed into the client's
//! book category; RSS releases matched to wanted episodes; completed downloads filed into the
//! podcast library.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::library::MediaManagementSettings;
use reelix_api_types::pipeline::HistoryQuery;
use reelix_api_types::providers::{IndexerSettingsDto, ProviderInput, ProviderKind};
use reelix_api_types::radio::{AddRadioShowRequest, RadioFollow, RadioSearchRequest, RadioSettingsDto};
use reelix_core::bus::EventBus;
use reelix_core::{ContentKind, Protocol};
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_downloaders::{
    AddOptions, ClientError, ClientInfo, ClientStatus, DownloadClient, DownloadId, DownloadItem,
    DownloadState, ReleasePayload,
};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_notify::NotificationEvent;
use reelix_parser::{Quality, QualityModel};
use reelix_pipeline::search::{SearchOptions, SearchService};
use reelix_pipeline::store::tracked::{self, NewTracked, TrackedState};
use reelix_pipeline::store::{ContentRef, history};
use reelix_pipeline::{ActivityService, ClientHandle, GrabbedRelease, StaticProviders};
use reelix_providers::{ProviderOptions, ProviderService};
use reelix_radio::{
    EpisodeData, ProviderError, ProviderRegistry, RadioDeps, RadioProvider, RadioService, SearchPrefs,
    SeriesData, ShowData, ShowFetch, ShowHit, StationData,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MB: u64 = 1024 * 1024;

fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 18, 30, 0).unwrap()
}

struct Fake;

#[async_trait]
impl RadioProvider for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    async fn search(&self, _q: &str, _p: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Ok(vec![])
    }
    async fn preview(&self, _l: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(None)
    }
    async fn show(&self, _id: &str, _f: &ShowFetch) -> Result<ShowData, ProviderError> {
        let ep = |id: &str, n: u32, title: &str, when: DateTime<Utc>| EpisodeData {
            external_id: id.into(),
            series_external_id: "s20".into(),
            number: Some(n),
            title: title.into(),
            first_broadcast: Some(when),
            duration_secs: Some(28 * 60),
            ..EpisodeData::default()
        };
        Ok(ShowData {
            external_id: "nq".into(),
            title: "The News Quiz".into(),
            station: Some(StationData {
                external_id: "r4".into(),
                name: "BBC Radio 4".into(),
                logo_url: None,
            }),
            series: vec![SeriesData {
                external_id: "s20".into(),
                number: Some(20),
                ..SeriesData::default()
            }],
            episodes: vec![
                ep("e3", 3, "Episode 3", at(2024, 3, 1)),
                // Not broadcast yet: never searched.
                ep("e4", 4, "Episode 4", at(2030, 1, 1)),
            ],
            complete: true,
            ..ShowData::default()
        })
    }
}

async fn radio(db: &Db, events: &EventBus, library: &Path) -> (Arc<RadioService>, i64) {
    let svc = Arc::new(RadioService::new(RadioDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(Arc::new(Fake)),
        library_sync: None,
    }));
    let show = svc
        .add_show(AddRadioShowRequest {
            provider: Some("fake".into()),
            external_id: Some("nq".into()),
            follow: Some(RadioFollow::All),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    std::fs::create_dir_all(library).unwrap();
    svc.set_settings(RadioSettingsDto {
        library_folder: Some(library.to_string_lossy().into_owned()),
        ..RadioSettingsDto::default()
    })
    .await
    .unwrap();
    let e3 = svc
        .show_episodes(show.id)
        .await
        .unwrap()
        .episodes
        .into_iter()
        .find(|e| e.external_id == "e3")
        .unwrap()
        .id;
    (svc, e3)
}

// ---------------------------------------------------------------- search → grab

const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <limits max="100" default="100" />
  <searching>
    <search available="yes" supportedParams="q" />
    <tv-search available="yes" supportedParams="q,season,ep" />
    <audio-search available="yes" supportedParams="q,artist,album" />
  </searching>
  <categories>
    <category id="3000" name="Audio"><subcat id="3030" name="Audiobook" /><subcat id="3050" name="Other" /></category>
  </categories>
</caps>"#;

const NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p@example.invalid" date="1777440000" subject="x [1/1] - &quot;x.rar&quot; yEnc (1/1)">
    <groups><group>alt.binaries.example</group></groups>
    <segments><segment bytes="768000" number="1">a@example.invalid</segment></segments>
  </file>
</nzb>"#;

fn rss(base: &str, items: &[(&str, &str, u64, u32)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><title>Idx</title>"#,
    );
    let date = (Utc::now() - chrono::Duration::days(2)).to_rfc2822();
    for (title, guid, size, cat) in items {
        s.push_str(&format!(
            r#"<item><title>{title}</title><guid isPermaLink="false">{guid}</guid><link>{base}/getnzb/{guid}.nzb</link>
<pubDate>{date}</pubDate><enclosure url="{base}/getnzb/{guid}.nzb" length="{size}" type="application/x-nzb" />
<newznab:attr name="size" value="{size}" /><newznab:attr name="category" value="{cat}" /></item>"#
        ));
    }
    s.push_str("</channel></rss>");
    s
}

fn xml(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/rss+xml")
}

#[derive(Default)]
struct NzoIds(std::sync::atomic::AtomicUsize);

impl wiremock::Respond for NzoIds {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        ResponseTemplate::new(200)
            .set_body_json(json!({"status": true, "nzo_ids": [format!("SABnzbd_nzo_{n}")]}))
    }
}

struct SearchEnv {
    db: Db,
    svc: SearchService,
    episode: i64,
    indexer: MockServer,
    sab: MockServer,
    _dir: tempfile::TempDir,
}

async fn search_env() -> SearchEnv {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let mut opts = ProviderOptions::new(dir.path());
    opts.search_timeout = std::time::Duration::from_secs(5);
    let providers = ProviderService::new(
        db.clone(),
        SecretBox::from_key([3; 32]),
        events.clone(),
        reqwest::Client::new(),
        opts,
    )
    .await
    .unwrap();
    providers
        .set_indexer_settings(IndexerSettingsDto {
            allow_private_addresses: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let indexer = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(xml(CAPS.into()))
        .mount(&indexer)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("^/getnzb/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(NZB, "application/x-nzb"))
        .mount(&indexer)
        .await;
    let base = indexer.uri();
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "search"))
        .respond_with(xml(rss(
            &base,
            &[
                ("The.News.Quiz.S20E03.MP3-GRP", "mp3", 25 * MB, 3050),
                ("The.News.Quiz.S20E03.AAC-GRP", "aac", 25 * MB, 3050),
                ("The.News.Quiz.S20E02.AAC-GRP", "e02", 25 * MB, 3050),
                ("The Now Show S20E03 AAC", "other", 25 * MB, 3050),
            ],
        )))
        .mount(&indexer)
        .await;
    let sab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("/api$"))
        .and(query_param("mode", "addfile"))
        .respond_with(NzoIds::default())
        .mount(&sab)
        .await;
    providers
        .create(
            ProviderKind::Indexer,
            &ProviderInput {
                implementation: "newznab".into(),
                name: "Idx".into(),
                enabled: true,
                priority: 25,
                tags: vec![],
                settings: json!({"base_url": indexer.uri(), "api_key": "k"}),
            },
        )
        .await
        .unwrap();
    providers
        .create(
            ProviderKind::DownloadClient,
            &ProviderInput {
                implementation: "sabnzbd".into(),
                name: "SAB".into(),
                enabled: true,
                priority: 1,
                tags: vec![],
                settings: json!({
                    "host": "127.0.0.1", "port": sab.address().port(), "url_base": "", "api_key": "sabkey",
                    "book_category": "books", "music_category": "music",
                }),
            },
        )
        .await
        .unwrap();
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    let (radio, episode) = radio(&db, &events, &dir.path().join("podcasts")).await;
    let svc = SearchService::new(
        db.clone(),
        events,
        providers,
        library,
        SearchOptions {
            throttle: std::time::Duration::ZERO,
            ..Default::default()
        },
    );
    svc.set_radio(radio);
    SearchEnv {
        db,
        svc,
        episode,
        indexer,
        sab,
        _dir: dir,
    }
}

/// `cat` query values of every SABnzbd `addfile` call.
async fn sab_categories(e: &SearchEnv) -> Vec<String> {
    e.sab
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.query_pairs().any(|(k, v)| k == "mode" && v == "addfile"))
        .map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "cat")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test]
async fn radio_search_scores_and_grabs_wanted_episodes_only() {
    let e = search_env().await;

    // Interactive: every release with its decision.
    let rows = e.svc.interactive_radio(e.episode).await.unwrap();
    assert_eq!(rows.len(), 4, "{rows:#?}");
    assert_eq!(rows[0].guid, "aac", "{rows:#?}");
    assert!(rows[0].accepted, "{:?}", rows[0].rejections);
    assert_eq!(rows[0].quality.quality, Quality::AudiobookM4b);
    let mp3 = rows.iter().find(|r| r.guid == "mp3").unwrap();
    assert!(mp3.accepted && mp3.quality.quality == Quality::AudiobookMp3);
    for wrong in ["e02", "other"] {
        let r = rows.iter().find(|r| r.guid == wrong).unwrap();
        assert!(r.rejections.iter().any(|x| x.spec == "mapping"), "{r:?}");
    }
    // Searched in Audio/Other, by show + SxxEyy (and the date).
    let searches: Vec<(String, String)> = e
        .indexer
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.query_pairs().any(|(k, v)| k == "t" && v == "search"))
        .map(|r| {
            let get = |key: &str| {
                r.url
                    .query_pairs()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default()
            };
            (get("q"), get("cat"))
        })
        .collect();
    assert!(
        searches.contains(&("The News Quiz S20E03".into(), "3050".into())),
        "{searches:?}"
    );

    // Automatic (every wanted episode): only S20E03 is searched; the AAC release is grabbed
    // into the client's book category.
    let req = RadioSearchRequest {
        kind: ContentSearchKind::Missing,
        ids: vec![],
    };
    let sum = e.svc.run_radio_search(&req, &|| false).await.unwrap();
    assert_eq!((sum.searched, sum.grabbed), (1, 1), "{sum:?}");
    assert_eq!(sab_categories(&e).await, vec!["books".to_owned()]);
    let queue = tracked::active(&e.db).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].title, "The.News.Quiz.S20E03.AAC-GRP");
    assert_eq!(queue[0].content, ContentRef::radio_episode(e.episode));
    let (h, _) = history::list(&e.db, &Default::default(), 0, 10).await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(
        (h[0].event.as_str(), h[0].content.radio_episode_id),
        ("grabbed", Some(e.episode))
    );

    // Again: the queued download blocks a second grab.
    let again = e.svc.run_radio_search(&req, &|| false).await.unwrap();
    assert_eq!(again.grabbed, 0);
}

#[tokio::test]
async fn rss_sync_matches_radio_releases_to_wanted_episodes() {
    let e = search_env().await;
    let sum = e.svc.rss_sync(true).await.unwrap().unwrap();
    assert_eq!(sum.grabbed, 1, "{sum:?}");
    let queue = tracked::active(&e.db).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].content, ContentRef::radio_episode(e.episode));
    assert_eq!(queue[0].title, "The.News.Quiz.S20E03.AAC-GRP");
}

// ---------------------------------------------------------------- completed download → filing

struct FakeClient {
    info: ClientInfo,
    items: Mutex<Vec<DownloadItem>>,
}

#[async_trait]
impl DownloadClient for FakeClient {
    fn protocol(&self) -> Protocol {
        Protocol::Usenet
    }
    fn info(&self) -> &ClientInfo {
        &self.info
    }
    async fn add(&self, _r: &ReleasePayload, _o: &AddOptions) -> Result<DownloadId, ClientError> {
        Err(ClientError::Unsupported("add"))
    }
    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        Ok(self.items.lock().unwrap().clone())
    }
    async fn remove(&self, id: &DownloadId, _delete: bool) -> Result<(), ClientError> {
        self.items.lock().unwrap().retain(|i| i.download_id != *id);
        Ok(())
    }
    async fn test(&self) -> Result<(), ClientError> {
        Ok(())
    }
    async fn status(&self) -> Result<ClientStatus, ClientError> {
        Ok(ClientStatus::default())
    }
}

fn release(title: &str) -> GrabbedRelease {
    GrabbedRelease {
        guid: format!("guid-{title}"),
        title: title.into(),
        indexer_id: Some(1),
        indexer: "Idx".into(),
        protocol: Protocol::Usenet,
        size: Some(1000),
        publish_date: None,
        download_url: Some("http://x/nzb".into()),
        magnet_url: None,
        info_url: None,
        info_hash: None,
        seeders: None,
        quality: QualityModel::new(Quality::AudiobookMp3),
        languages: vec![],
        release_group: None,
        custom_formats: vec![],
        format_score: 0,
        indexer_flags: vec![],
        seed_ratio: None,
        seed_time_minutes: None,
        soulseek: None,
    }
}

#[tokio::test]
async fn completed_radio_download_is_filed_into_the_podcast_library() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let jobs = JobSystem::builder(db.clone(), events.clone()).build();
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    library.set_queue(jobs.queue());
    library
        .set_media_management(MediaManagementSettings {
            analyze_video_files: false,
            minimum_free_space_mb: 0,
            ..Default::default()
        })
        .await
        .unwrap();
    let podcasts = dir.path().join("podcasts");
    let (radio, episode) = radio(&db, &events, &podcasts).await;
    let client = Arc::new(FakeClient {
        info: ClientInfo {
            implementation: "fake".into(),
            name: "Fake".into(),
            priority: 1,
            remove_completed: false,
            remove_failed: false,
            tags: vec![],
        },
        items: Mutex::default(),
    });
    let providers = StaticProviders {
        clients: vec![ClientHandle {
            id: 7,
            name: "Fake".into(),
            host: "localhost".into(),
            client: client.clone(),
        }],
        roots: vec![downloads.clone()],
        ..Default::default()
    };
    let sent = providers.sent.clone();
    let svc = Arc::new(ActivityService::new(
        db.clone(),
        events,
        library,
        Arc::new(providers),
    ));
    svc.set_queue(jobs.queue());
    svc.set_radio(radio.clone());

    let title = "The.News.Quiz.S20E03.MP3-GRP";
    let folder = downloads.join(title);
    std::fs::create_dir_all(&folder).unwrap();
    // Not a real MP3: tags cannot be written, the file is still filed.
    std::fs::write(folder.join("the.news.quiz.s20e03.mp3"), vec![7u8; 4096]).unwrap();
    let id = tracked::insert(
        &db,
        &NewTracked {
            download_client_id: 7,
            download_id: "nzo_r".into(),
            protocol: Protocol::Usenet,
            title: title.into(),
            movie_id: None,
            series_id: None,
            episode_ids: vec![],
            release: release(title),
        },
    )
    .await
    .unwrap();
    tracked::set_content(&db, id, &ContentRef::radio_episode(episode))
        .await
        .unwrap();
    let mut item = DownloadItem::new("nzo_r", title, DownloadState::Completed);
    item.total_bytes = 4096;
    item.output_path = Some(folder.to_string_lossy().into_owned());
    item.can_move_files = true;
    item.can_be_removed = true;
    client.items.lock().unwrap().push(item);

    svc.poll().await.unwrap();
    let row = tracked::get(&db, id).await.unwrap().unwrap();
    assert_eq!(row.state, TrackedState::Imported, "{:?}", row.status_messages);
    let dest: PathBuf = podcasts
        .join("The News Quiz")
        .join("The News Quiz - S20E03 - Episode 3.mp3");
    assert!(dest.is_file());
    // A finished Usenet download is moved (nothing seeds it).
    assert!(!folder.join("the.news.quiz.s20e03.mp3").exists());
    assert_eq!(radio.filed_path(episode).await.unwrap(), Some(dest));

    let hist = svc
        .history(&HistoryQuery {
            radio_episode_id: Some(episode),
            ..Default::default()
        })
        .await
        .unwrap();
    let imported = hist.items.iter().find(|h| h.event_type == "imported").unwrap();
    let media = imported.media.clone().unwrap();
    assert_eq!(
        (media.kind.as_str(), media.id, media.title.as_str()),
        ("radio_episode", episode, "S20E03 - Episode 3")
    );
    assert_eq!(media.subtitle.as_deref(), Some("The News Quiz"));
    let sent = sent.lock().unwrap().clone();
    assert!(
        sent.iter()
            .any(|n| matches!(n, NotificationEvent::Import { media, .. }
            if media.content == Some(ContentKind::Radio)
                && media.title == "The News Quiz - S20E03 - Episode 3")),
        "{sent:?}"
    );
}
