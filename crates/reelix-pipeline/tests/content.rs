//! Books and albums through the pipeline: search → decision → grab (with the client's book /
//! music category) against a wiremock Newznab indexer and SABnzbd, completed book downloads
//! handed to the books importer, failed album downloads blocklisted and searched again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use reelix_api_types::books::{
    AddAuthorRequest, AuthorMonitor, BookDto, BookListQuery, BookSearchRequest, NewBookMonitor,
};
use reelix_api_types::content::{ContentSearchKind, CreateContentRootFolderRequest};
use reelix_api_types::library::MediaManagementSettings;
use reelix_api_types::music::AlbumSearchRequest;
use reelix_api_types::pipeline::{GrabRequest, HistoryQuery, ReleaseQuery};
use reelix_api_types::providers::{IndexerSettingsDto, ProviderInput, ProviderKind};
use reelix_books::testing::{FakeBookMetadata, author, work};
use reelix_books::{BooksDeps, BooksService};
use reelix_core::bus::EventBus;
use reelix_core::{BookFormat, ContentKind, Protocol};
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_downloaders::{
    AddOptions, ClientError, ClientInfo, ClientStatus, DownloadClient, DownloadId, DownloadItem,
    DownloadState, ReleasePayload, UsenetFailure,
};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_music::provider::{
    ArtistMetadata, ArtistSearchHit, CoverArt, MetaError, MusicMetadataProvider, ReleaseGroupMetadata,
    ReleaseGroupSearchHit, ReleaseMetadata,
};
use reelix_music::{MusicDeps, MusicService};
use reelix_notify::NotificationEvent;
use reelix_parser::{Quality, QualityFamily, QualityModel};
use reelix_pipeline::search::{SearchOptions, SearchService};
use reelix_pipeline::store::tracked::{self, NewTracked, TrackedState};
use reelix_pipeline::store::{ContentRef, blocklist, history, pending};
use reelix_pipeline::{ActivityService, ClientHandle, GrabbedRelease, StaticProviders};
use reelix_providers::{ProviderOptions, ProviderService};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------- shared fixtures

/// No MusicBrainz: albums are inserted directly.
struct NoMusicBrainz;

#[async_trait]
impl MusicMetadataProvider for NoMusicBrainz {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn search_artists(&self, _q: &str) -> Result<Vec<ArtistSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn search_release_groups(
        &self,
        _q: &str,
        _artist: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn artist(&self, _mbid: &str) -> Result<ArtistMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn release_group(&self, _mbid: &str) -> Result<ReleaseGroupMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn releases(&self, _group: &str) -> Result<Vec<ReleaseMetadata>, MetaError> {
        Ok(vec![])
    }
    async fn release(&self, _mbid: &str) -> Result<ReleaseMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn cover_art(&self, _group: &str) -> Result<Option<CoverArt>, MetaError> {
        Ok(None)
    }
}

struct Content {
    books: Arc<BooksService>,
    music: Arc<MusicService>,
    dune: i64,
    album: i64,
    ebooks: PathBuf,
}

/// Books + music services with Frank Herbert's "Dune" (ebook format wanted) and Radiohead's
/// "OK Computer" (3 tracks, ~16 min).
async fn content(db: &Db, events: &EventBus, library: &Arc<LibraryService>, dir: &Path) -> Content {
    let herbert = author("OL2A", "Frank Herbert");
    let meta = FakeBookMetadata::new()
        .with_author(herbert.clone())
        .with_work(work("OL20W", "Dune", &herbert, Some(1965)))
        .with_work(work("OL21W", "Dune Messiah", &herbert, Some(1969)));
    let books = Arc::new(BooksService::new(BooksDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(meta),
    }));
    books.set_today(NaiveDate::from_ymd_opt(2024, 6, 1).unwrap());
    let ebooks = dir.join("ebooks");
    let music_root = dir.join("music");
    for d in [&ebooks, &music_root] {
        std::fs::create_dir_all(d).unwrap();
    }
    let root = books
        .add_root_folder(CreateContentRootFolderRequest {
            path: ebooks.to_string_lossy().into(),
            kind: ContentKind::Ebook,
        })
        .await
        .unwrap();
    let ebook_profile = db
        .quality_profiles()
        .list_family(QualityFamily::Ebook)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.is_allowed(Quality::Epub))
        .unwrap()
        .id;
    let a = books
        .add_author(AddAuthorRequest {
            openlibrary_id: "OL2A".into(),
            quality_profile_id: Some(ebook_profile),
            root_folder_id: Some(root.id),
            audio_quality_profile_id: None,
            audio_root_folder_id: None,
            monitored: true,
            monitor: AuthorMonitor::All,
            monitor_new_items: NewBookMonitor::All,
            selected_work_ids: vec![],
            tags: vec![],
            search_on_add: false,
        })
        .await
        .unwrap();
    let list: Vec<BookDto> = books
        .list_books(&BookListQuery {
            author_id: Some(a.id),
            ..Default::default()
        })
        .await
        .unwrap()
        .items;
    let dune = list.iter().find(|b| b.title == "Dune").unwrap().id;

    let music = Arc::new(MusicService::new(MusicDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(NoMusicBrainz),
    }));
    let music_profile = db
        .quality_profiles()
        .list_family(QualityFamily::Music)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.is_allowed(Quality::Flac))
        .unwrap()
        .id;
    let root_id: i64 = sqlx::query_scalar(
        "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES (?, 'music', 0, 0) RETURNING id",
    )
    .bind(music_root.to_string_lossy().into_owned())
    .fetch_one(db.pool())
    .await
    .unwrap();
    let artist: i64 = sqlx::query_scalar(
        "INSERT INTO artists (musicbrainz_id, name, sort_name, clean_name, quality_profile_id, metadata_profile_id, \
         root_folder_id, path, created_at, updated_at) VALUES ('mb-radiohead', 'Radiohead', 'Radiohead', 'radiohead', \
         ?, 1, ?, ?, 0, 0) RETURNING id",
    )
    .bind(music_profile)
    .bind(root_id)
    .bind(music_root.join("Radiohead").to_string_lossy().into_owned())
    .fetch_one(db.pool())
    .await
    .unwrap();
    let album: i64 = sqlx::query_scalar(
        "INSERT INTO albums (artist_id, musicbrainz_release_group_id, title, clean_title, release_date, monitored, \
         created_at, updated_at) VALUES (?, 'mb-okc', 'OK Computer', 'okcomputer', '1997-05-21', 1, 0, 0) RETURNING id",
    )
    .bind(artist)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let release: i64 = sqlx::query_scalar(
        "INSERT INTO album_releases (album_id, musicbrainz_release_id, title, status, track_count, release_date, \
         created_at, updated_at) VALUES (?, 'mb-okc-gb', 'OK Computer', 'official', 3, '1997-05-21', 0, 0) RETURNING id",
    )
    .bind(album)
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE albums SET selected_release_id = ? WHERE id = ?")
        .bind(release)
        .bind(album)
        .execute(db.pool())
        .await
        .unwrap();
    for (n, (t, secs)) in [
        ("Airbag", 284),
        ("Paranoid Android", 383),
        ("Subterranean Homesick Alien", 267),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "INSERT INTO tracks (album_id, release_id, track_number, title, duration_ms, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 0, 0)",
        )
        .bind(album)
        .bind(release)
        .bind(n as i64 + 1)
        .bind(t)
        .bind(secs * 1000)
        .execute(db.pool())
        .await
        .unwrap();
    }
    Content {
        books,
        music,
        dune,
        album,
        ebooks,
    }
}

// ---------------------------------------------------------------- search → grab

const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <limits max="100" default="100" />
  <searching>
    <search available="yes" supportedParams="q" />
    <tv-search available="yes" supportedParams="q,season,ep" />
    <movie-search available="yes" supportedParams="q,imdbid" />
    <book-search available="yes" supportedParams="q,author,title" />
    <audio-search available="yes" supportedParams="q,artist,album" />
  </searching>
  <categories>
    <category id="2000" name="Movies" />
    <category id="3000" name="Audio"><subcat id="3030" name="Audiobook" /><subcat id="3040" name="Lossless" /></category>
    <category id="7000" name="Books"><subcat id="7020" name="EBook" /></category>
  </categories>
</caps>"#;

const NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p@example.invalid" date="1777440000" subject="x [1/1] - &quot;x.rar&quot; yEnc (1/1)">
    <groups><group>alt.binaries.example</group></groups>
    <segments><segment bytes="768000" number="1">a@example.invalid</segment></segments>
  </file>
</nzb>"#;

const MB: u64 = 1024 * 1024;

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

/// SABnzbd `addfile` reply with a fresh `nzo_id` per call.
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
    content: Content,
    indexer: MockServer,
    sab: MockServer,
    indexer_id: i64,
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
    let sab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("/api$"))
        .and(query_param("mode", "addfile"))
        .respond_with(NzoIds::default())
        .mount(&sab)
        .await;
    let idx = providers
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
    let content = content(&db, &events, &library, dir.path()).await;
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
    svc.set_content(content.books.clone(), content.music.clone());
    SearchEnv {
        db,
        svc,
        content,
        indexer,
        sab,
        indexer_id: idx.id,
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
async fn book_search_grabs_the_ebook_into_the_book_category() {
    let e = search_env().await;
    let base = e.indexer.uri();
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "book"))
        .respond_with(xml(rss(
            &base,
            &[
                (
                    "Frank Herbert - Dune Messiah (1969) [EPUB]",
                    "messiah",
                    3 * MB,
                    7020,
                ),
                ("Frank Herbert - Dune (1965) [MP3]", "audio", 300 * MB, 3030),
                ("Frank Herbert - Dune (1965) [EPUB]", "dune-epub", 3 * MB, 7020),
            ],
        )))
        .mount(&e.indexer)
        .await;

    // Interactive: every release with its decision, mapped to the book format.
    let rows = e
        .svc
        .interactive(&ReleaseQuery {
            book_id: Some(e.content.dune),
            book_format: Some(BookFormat::Ebook),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    let best = &rows[0];
    assert_eq!(best.guid, "dune-epub", "{rows:#?}");
    assert!(best.accepted, "{:?}", best.rejections);
    assert_eq!(best.quality.quality, Quality::Epub);
    assert_eq!(best.book_id, Some(e.content.dune));
    assert_eq!(best.book_format, Some(BookFormat::Ebook));
    let messiah = rows.iter().find(|r| r.guid == "messiah").unwrap();
    assert!(
        messiah.rejections.iter().any(|r| r.spec == "mapping"),
        "{messiah:?}"
    );
    let audio = rows.iter().find(|r| r.guid == "audio").unwrap();
    assert!(!audio.accepted);

    // Automatic search (job `book_search`): grabs the EPUB with the book category.
    let sum = e
        .svc
        .run_book_search(
            &BookSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![e.content.dune],
                format: None,
            },
            &|| false,
        )
        .await
        .unwrap();
    assert_eq!((sum.searched, sum.grabbed), (1, 1));
    assert_eq!(sab_categories(&e).await, vec!["books".to_owned()]);
    let queue = tracked::active(&e.db).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].title, "Frank Herbert - Dune (1965) [EPUB]");
    assert_eq!(
        queue[0].content,
        ContentRef::book(e.content.dune, BookFormat::Ebook)
    );
    assert_eq!(queue[0].movie_id, None);
    let (h, _) = history::list(&e.db, &Default::default(), 0, 10).await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].event.as_str(), "grabbed");
    assert_eq!(h[0].content.book_id, Some(e.content.dune));
    assert!(h[0].author_id.is_some());

    // Searching again: the queued release blocks a second grab.
    let again = e
        .svc
        .run_book_search(
            &BookSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![e.content.dune],
                format: Some(BookFormat::Ebook),
            },
            &|| false,
        )
        .await
        .unwrap();
    assert_eq!(again.grabbed, 0);

    // Interactive grab of a cached result keeps the book reference.
    let r = e
        .svc
        .grab_request(&GrabRequest {
            guid: "messiah".into(),
            indexer_id: e.indexer_id,
            force: true,
            movie_id: None,
            episode_ids: None,
            download_client_id: None,
            download_url: None,
            title: None,
        })
        .await
        .unwrap();
    let row = tracked::get(&e.db, r.queue_id).await.unwrap().unwrap();
    assert_eq!(row.content.book_id, Some(e.content.dune));
}

#[tokio::test]
async fn album_search_grabs_into_the_music_category_and_honours_delay_profiles() {
    let e = search_env().await;
    let base = e.indexer.uri();
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "music"))
        .respond_with(xml(rss(
            &base,
            &[
                ("Radiohead - Kid A (2000) [FLAC]", "kida", 120 * MB, 3040),
                (
                    "Radiohead - OK Computer (1997) [FLAC]",
                    "okc-flac",
                    110 * MB,
                    3040,
                ),
                (
                    "Radiohead - Discography 1993-2016 [FLAC]",
                    "disco",
                    9000 * MB,
                    3040,
                ),
            ],
        )))
        .mount(&e.indexer)
        .await;
    let req = AlbumSearchRequest {
        kind: ContentSearchKind::Item,
        ids: vec![e.content.album],
    };

    // A usenet delay holds the album release as pending (with the album reference).
    let mut dp = e.db.delay_profiles().list().await.unwrap().remove(0);
    dp.usenet_delay = 6000;
    dp.bypass_if_highest_quality = false;
    e.db.delay_profiles().update(dp.id, &dp).await.unwrap();
    let held = e.svc.run_album_search(&req, &|| false).await.unwrap();
    assert_eq!((held.grabbed, held.pending), (0, 1), "{held:?}");
    let p = pending::all(&e.db).await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].content, ContentRef::album(e.content.album));
    let listed = e.svc.list_pending().await.unwrap();
    let media = listed[0].media.clone().unwrap();
    assert_eq!(
        (media.kind.as_str(), media.title.as_str()),
        ("album", "OK Computer")
    );
    assert_eq!(media.subtitle.as_deref(), Some("Radiohead"));

    // Grabbing the pending release now sends it with the music category.
    let r = e.svc.grab_pending(p[0].id).await.unwrap();
    assert_eq!(sab_categories(&e).await, vec!["music".to_owned()]);
    let row = tracked::get(&e.db, r.queue_id).await.unwrap().unwrap();
    assert_eq!(row.title, "Radiohead - OK Computer (1997) [FLAC]");
    assert_eq!(row.content, ContentRef::album(e.content.album));
    assert!(pending::all(&e.db).await.unwrap().is_empty());

    // Without the delay a fresh search is blocked by the queued download.
    dp.usenet_delay = 0;
    e.db.delay_profiles().update(dp.id, &dp).await.unwrap();
    tracked::delete(&e.db, r.queue_id).await.unwrap();
    let sum = e.svc.run_album_search(&req, &|| false).await.unwrap();
    assert_eq!((sum.searched, sum.grabbed), (1, 1));
    assert_eq!(sab_categories(&e).await.len(), 2);
}

#[tokio::test]
async fn rss_sync_maps_book_and_music_releases_by_category() {
    let e = search_env().await;
    let base = e.indexer.uri();
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "search"))
        .respond_with(xml(rss(
            &base,
            &[
                ("Frank Herbert - Dune (1965) [EPUB]", "rss-dune", 3 * MB, 7020),
                ("Radiohead - OK Computer (1997) [FLAC]", "rss-okc", 110 * MB, 3040),
                ("Unknown Artist - Nothing [FLAC]", "rss-x", 110 * MB, 3040),
            ],
        )))
        .mount(&e.indexer)
        .await;
    let sum = e.svc.rss_sync(true).await.unwrap().unwrap();
    assert_eq!(sum.grabbed, 2, "{sum:?}");
    let mut cats = sab_categories(&e).await;
    cats.sort();
    assert_eq!(cats, vec!["books".to_owned(), "music".to_owned()]);
    let queue = tracked::active(&e.db).await.unwrap();
    assert!(
        queue
            .iter()
            .any(|t| t.content == ContentRef::book(e.content.dune, BookFormat::Ebook))
    );
    assert!(
        queue
            .iter()
            .any(|t| t.content == ContentRef::album(e.content.album))
    );
}

// ---------------------------------------------------------------- tracking → import

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

struct ActivityEnv {
    db: Db,
    svc: Arc<ActivityService>,
    client: Arc<FakeClient>,
    sent: Arc<Mutex<Vec<NotificationEvent>>>,
    content: Content,
    downloads: PathBuf,
    _dir: tempfile::TempDir,
}

const CLIENT_ID: i64 = 7;

async fn activity_env() -> ActivityEnv {
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
    let content = content(&db, &events, &library, dir.path()).await;
    content.books.set_queue(jobs.queue());
    content.music.set_queue(jobs.queue());
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
            id: CLIENT_ID,
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
    svc.set_content(content.books.clone(), content.music.clone());
    ActivityEnv {
        db,
        svc,
        client,
        sent,
        content,
        downloads,
        _dir: dir,
    }
}

fn release(title: &str, quality: Quality) -> GrabbedRelease {
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
        quality: QualityModel::new(quality),
        languages: vec![],
        release_group: None,
        custom_formats: vec![],
        format_score: 0,
        indexer_flags: vec![],
        seed_ratio: None,
        seed_time_minutes: None,
    }
}

async fn track(e: &ActivityEnv, id: &str, title: &str, quality: Quality, content: ContentRef) -> i64 {
    let row = tracked::insert(
        &e.db,
        &NewTracked {
            download_client_id: CLIENT_ID,
            download_id: id.into(),
            protocol: Protocol::Usenet,
            title: title.into(),
            movie_id: None,
            series_id: None,
            episode_ids: vec![],
            release: release(title, quality),
        },
    )
    .await
    .unwrap();
    tracked::set_content(&e.db, row, &content).await.unwrap();
    row
}

fn item(id: &str, title: &str, state: DownloadState, output: Option<&Path>) -> DownloadItem {
    let mut i = DownloadItem::new(id, title, state);
    i.total_bytes = 1000;
    i.output_path = output.map(|p| p.to_string_lossy().into_owned());
    if state == DownloadState::Failed {
        i.message = Some("CRC errors".into());
        i.failure = Some(UsenetFailure::Other);
    }
    i.can_move_files = true;
    i.can_be_removed = true;
    i
}

#[tokio::test]
async fn completed_book_download_is_imported_by_the_books_importer() {
    let e = activity_env().await;
    let title = "Frank Herbert - Dune (1965) [EPUB]";
    let folder = e.downloads.join(title);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Dune.epub"), vec![7u8; 4096]).unwrap();
    let id = track(
        &e,
        "nzo_b",
        title,
        Quality::Epub,
        ContentRef::book(e.content.dune, BookFormat::Ebook),
    )
    .await;
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("nzo_b", title, DownloadState::Completed, Some(&folder)));

    e.svc.poll().await.unwrap();
    let row = tracked::get(&e.db, id).await.unwrap().unwrap();
    assert_eq!(row.state, TrackedState::Imported, "{:?}", row.status_messages);
    let book = e.content.books.get_book(e.content.dune).await.unwrap();
    let file = book.ebook_file.expect("ebook file recorded");
    assert!(Path::new(&file.path).exists());
    assert!(Path::new(&file.path).starts_with(&e.content.ebooks));
    assert_eq!(file.quality.quality, Quality::Epub);

    let hist = e
        .svc
        .history(&HistoryQuery {
            book_id: Some(e.content.dune),
            ..Default::default()
        })
        .await
        .unwrap();
    let imported = hist.items.iter().find(|h| h.event_type == "imported").unwrap();
    let media = imported.media.clone().unwrap();
    assert_eq!((media.kind.as_str(), media.id), ("book", e.content.dune));
    assert_eq!(media.book_format, Some(BookFormat::Ebook));
    assert_eq!(media.subtitle.as_deref(), Some("Frank Herbert"));
    let sent = e.sent.lock().unwrap().clone();
    assert!(
        sent.iter()
            .any(|n| matches!(n, NotificationEvent::Import { media, .. }
            if media.content == Some(ContentKind::Ebook) && media.title == "Frank Herbert - Dune")),
        "{sent:?}"
    );
}

#[tokio::test]
async fn book_download_without_book_files_waits_with_the_rejection() {
    let e = activity_env().await;
    let title = "Frank Herbert - Dune (1965) [EPUB]";
    let folder = e.downloads.join("empty");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("readme.nfo"), b"x").unwrap();
    let id = track(
        &e,
        "nzo_e",
        title,
        Quality::Epub,
        ContentRef::book(e.content.dune, BookFormat::Ebook),
    )
    .await;
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("nzo_e", title, DownloadState::Completed, Some(&folder)));
    e.svc.poll().await.unwrap();
    let row = tracked::get(&e.db, id).await.unwrap().unwrap();
    assert_eq!(row.state, TrackedState::ImportPending);
    assert!(
        row.status_messages.iter().any(|m| m.contains("no ebook files")),
        "{:?}",
        row.status_messages
    );
    let queue = e.svc.queue(None, None).await.unwrap();
    let q = &queue.items[0];
    assert_eq!(q.media.as_ref().map(|m| m.kind.as_str()), Some("book"));
    assert!(!q.can_manual_import);
}

#[tokio::test]
async fn failed_album_download_blocklists_and_searches_again() {
    let e = activity_env().await;
    let title = "Radiohead - OK Computer (1997) [FLAC]";
    let id = track(
        &e,
        "nzo_a",
        title,
        Quality::Flac,
        ContentRef::album(e.content.album),
    )
    .await;
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("nzo_a", title, DownloadState::Failed, None));

    e.svc.poll().await.unwrap();
    assert_eq!(
        tracked::get(&e.db, id).await.unwrap().unwrap().state,
        TrackedState::Failed
    );
    let entries = blocklist::for_content(&e.db, &ContentRef::album(e.content.album))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source_title, title);
    let jobs: Vec<(String, String)> = sqlx::query_as("SELECT kind, payload FROM jobs")
        .fetch_all(e.db.pool())
        .await
        .unwrap();
    let search = jobs
        .iter()
        .find(|(k, _)| k == "album_search")
        .expect("album search queued");
    let req: AlbumSearchRequest = serde_json::from_str(&search.1).unwrap();
    assert_eq!(req.ids, vec![e.content.album]);
    let hist = e
        .svc
        .history(&HistoryQuery {
            album_id: Some(e.content.album),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hist.items[0].event_type, "download_failed");
    let media = hist.items[0].media.clone().unwrap();
    assert_eq!(
        (media.kind.as_str(), media.subtitle.as_deref()),
        ("album", Some("Radiohead"))
    );
}
