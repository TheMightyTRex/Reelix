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
    providers: Arc<ProviderService>,
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
        providers.clone(),
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
        providers,
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
    activity_env_with(None).await
}

/// [`activity_env`] plus another client (`(id, client)`, host `localhost`).
async fn activity_env_with(extra: Option<(i64, Arc<dyn DownloadClient>)>) -> ActivityEnv {
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
    let mut clients = vec![ClientHandle {
        id: CLIENT_ID,
        name: "Fake".into(),
        host: "localhost".into(),
        client: client.clone(),
    }];
    clients.extend(extra.map(|(id, client)| ClientHandle {
        id,
        name: client.info().name.clone(),
        host: "localhost".into(),
        client,
    }));
    let providers = StaticProviders {
        clients,
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
        soulseek: None,
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

// ---------------------------------------------------------------- Soulseek (slskd)

const SLSKD_SEARCH: &str = r"^/api/v0/searches/[0-9a-f-]{36}$";

/// slskd answering every search with `responses` (shaped like slskd 0.26's JSON) and
/// accepting every enqueue.
async fn slskd_server(responses: serde_json::Value) -> MockServer {
    let s = MockServer::start().await;
    let search = json!({"fileCount": 4, "id": "6f1c2b9e-3d4a-4e5f-9a7b-1c2d3e4f5a6b", "isComplete": true,
        "lockedFileCount": 0, "responseCount": 2, "responses": [], "searchText": "Radiohead OK Computer",
        "startedAt": "2026-11-05T14:02:11.1234567Z", "endedAt": "2026-11-05T14:02:29.9876543Z",
        "state": "Completed, TimedOut", "token": 48213});
    Mock::given(method("POST"))
        .and(path("/api/v0/searches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&search))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(SLSKD_SEARCH))
        .respond_with(ResponseTemplate::new(200).set_body_json(&search))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            r"^/api/v0/searches/[0-9a-f-]{36}/responses$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&responses))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .and(wiremock::matchers::path_regex(SLSKD_SEARCH))
        .respond_with(ResponseTemplate::new(204))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex(
            r"^/api/v0/transfers/downloads/[^/]+$",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "enqueued": [{"id": "0b6f7e1a-2c3d-4e5f-8a9b-0c1d2e3f4a5b", "state": "Queued, Remotely"}],
            "failed": []
        })))
        .mount(&s)
        .await;
    s
}

fn peer(username: &str, dir: &str, free: bool, queue: u32) -> serde_json::Value {
    let f = |n: &str, size: u64| {
        json!({"bitDepth": 16, "code": 1, "extension": "", "filename": format!("{dir}\\{n}"), "length": 284,
               "sampleRate": 44100, "size": size, "isLocked": false})
    };
    json!({"fileCount": 2, "files": [f("01 - Airbag.flac", 30 * MB), f("02 - Paranoid Android.flac", 40 * MB)],
           "hasFreeUploadSlot": free, "lockedFileCount": 0, "lockedFiles": [], "queueLength": queue,
           "token": 48213, "uploadSpeed": 1048576, "username": username})
}

#[tokio::test]
async fn soulseek_album_search_uses_its_own_delay_and_grabs_only_through_slskd() {
    let e = search_env().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "music"))
        .respond_with(xml(rss(&e.indexer.uri(), &[])))
        .mount(&e.indexer)
        .await;
    let dir = r"@@abcde\Music\Radiohead - OK Computer (1997)";
    let slskd = slskd_server(json!([peer("queued", dir, false, 9), peer("fast", dir, true, 0)])).await;
    let client = e
        .providers
        .create(
            ProviderKind::DownloadClient,
            &ProviderInput {
                implementation: "slskd".into(),
                name: "slskd".into(),
                enabled: true,
                priority: 1,
                tags: vec![],
                settings: json!({"host": "127.0.0.1", "port": slskd.address().port(), "api_key": "k",
                                 "downloads_dir": "/downloads"}),
            },
        )
        .await
        .unwrap();
    // The matching indexer came with the client.
    assert!(
        e.providers
            .indexers()
            .iter()
            .any(|(r, ix)| r.implementation == "slskd" && ix.protocol() == Protocol::Soulseek)
    );
    let req = AlbumSearchRequest {
        kind: ContentSearchKind::Item,
        ids: vec![e.content.album],
    };

    // Only the Soulseek delay applies to Soulseek releases.
    let mut dp = e.db.delay_profiles().list().await.unwrap().remove(0);
    dp.usenet_delay = 0;
    dp.soulseek_delay = 6000;
    dp.bypass_if_highest_quality = false;
    e.db.delay_profiles().update(dp.id, &dp).await.unwrap();
    let held = e.svc.run_album_search(&req, &|| false).await.unwrap();
    assert_eq!((held.grabbed, held.pending), (0, 1), "{held:?}");
    let p = pending::all(&e.db).await.unwrap();
    let rel = &p[0].release;
    assert_eq!(rel.protocol, Protocol::Soulseek);
    assert_eq!(rel.title, "Radiohead - OK Computer (1997) [FLAC]");
    assert_eq!(rel.quality.quality, Quality::Flac);
    // The free-slot peer ranks first; its file list travels with the pending release.
    let s = rel.soulseek.as_ref().unwrap();
    assert_eq!((s.username.as_str(), s.files.len()), ("fast", 2));
    assert_eq!(rel.indexer_flags, ["free_upload_slot"]);

    // Soulseek releases never go to the usenet client.
    let sab = e
        .providers
        .download_clients()
        .into_iter()
        .find(|(r, _)| r.implementation == "sabnzbd")
        .unwrap()
        .0;
    let err = e
        .svc
        .grab(rel, &Default::default(), Some(sab.id))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not handle Soulseek"), "{err}");

    let r = e.svc.grab_pending(p[0].id).await.unwrap();
    assert!(sab_categories(&e).await.is_empty());
    assert_eq!(r.download_client, "slskd");
    let posts: Vec<_> = slskd
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().starts_with("/api/v0/transfers/"))
        .collect();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].url.path(), "/api/v0/transfers/downloads/fast");
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body[0]["filename"], format!(r"{dir}\01 - Airbag.flac"));
    assert_eq!(body[1]["size"], 40 * MB);
    let row = tracked::get(&e.db, r.queue_id).await.unwrap().unwrap();
    assert_eq!(row.protocol, Protocol::Soulseek);
    assert_eq!(row.download_client_id, client.id);
    assert_eq!(row.download_id, format!("fast/{dir}"));
    assert_eq!(row.content, ContentRef::album(e.content.album));

    // A blocklisted peer folder is rejected; the same folder from another peer is not.
    tracked::delete(&e.db, r.queue_id).await.unwrap();
    blocklist::insert(
        &e.db,
        &blocklist::NewBlocklist {
            source_title: rel.title.clone(),
            protocol: Some(Protocol::Soulseek),
            torrent_info_hash: rel.release_key(),
            content: ContentRef::album(e.content.album),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    dp.soulseek_delay = 0;
    e.db.delay_profiles().update(dp.id, &dp).await.unwrap();
    let sum = e.svc.run_album_search(&req, &|| false).await.unwrap();
    assert_eq!(sum.grabbed, 1, "{sum:?}");
    let last = tracked::active(&e.db).await.unwrap();
    assert_eq!(last[0].download_id, format!("queued/{dir}"));
}

#[tokio::test]
async fn completed_soulseek_folder_is_imported_from_the_slskd_downloads_folder() {
    let slskd = MockServer::start().await;
    let dir = r"books\Frank Herbert\Dune";
    let file = |id: &str, name: &str, size: u64| {
        json!({"id": id, "username": "reader", "direction": "Download", "filename": format!("{dir}\\{name}"),
               "size": size, "state": "Completed, Succeeded", "requestedAt": "2026-11-05T14:05:00.0000000",
               "startedAt": "2026-11-05T14:05:01.0000000Z", "endedAt": "2026-11-05T14:05:09.0000000Z",
               "bytesTransferred": size, "averageSpeed": 524288, "attempts": 1, "removed": false,
               "bytesRemaining": 0, "percentComplete": 100})
    };
    let user = json!({"username": "reader", "directories": [{"directory": dir, "fileCount": 1,
        "files": [file("3f4a5b6c-7d8e-4f9a-8b1c-2d3e4f5a6b7c", "Dune.epub", 4096)]}]});
    Mock::given(method("GET"))
        .and(path("/api/v0/transfers/downloads"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([user.clone(), {
            "username": "someone", "directories": [{"directory": "music\\Mine", "fileCount": 1, "files": [{
                "id": "9a8b7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d", "username": "someone", "direction": "Download",
                "filename": "music\\Mine\\a.mp3", "size": 10, "state": "InProgress",
                "requestedAt": "2026-11-05T14:05:00.0000000", "bytesTransferred": 5, "averageSpeed": 1,
                "attempts": 1, "removed": false, "bytesRemaining": 5, "percentComplete": 50}]}]}])))
        .mount(&slskd)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v0/transfers/downloads/reader"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&user))
        .mount(&slskd)
        .await;
    Mock::given(method("DELETE"))
        .and(path(
            "/api/v0/transfers/downloads/reader/3f4a5b6c-7d8e-4f9a-8b1c-2d3e4f5a6b7c",
        ))
        .and(query_param("remove", "true"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&slskd)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("slskd");
    let client: Arc<dyn DownloadClient> = Arc::from(
        reelix_downloaders::build(
            "slskd",
            &json!({"name": "slskd", "host": "127.0.0.1", "port": slskd.address().port(), "api_key": "k",
                    "downloads_dir": downloads}),
            &reelix_downloaders::BuildContext::new(reqwest::Client::new()),
        )
        .unwrap(),
    );
    const SLSKD_ID: i64 = 8;
    let e = activity_env_with(Some((SLSKD_ID, client))).await;
    // slskd saved the files under its downloads folder by the last remote folder name.
    let folder = downloads.join("Dune");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Dune.epub"), vec![7u8; 4096]).unwrap();
    let title = "Frank Herbert - Dune [EPUB]";
    let mut rel = release(title, Quality::Epub);
    rel.protocol = Protocol::Soulseek;
    let id = tracked::insert(
        &e.db,
        &NewTracked {
            download_client_id: SLSKD_ID,
            download_id: format!("reader/{dir}"),
            protocol: Protocol::Soulseek,
            title: title.into(),
            movie_id: None,
            series_id: None,
            episode_ids: vec![],
            release: rel,
        },
    )
    .await
    .unwrap();
    tracked::set_content(&e.db, id, &ContentRef::book(e.content.dune, BookFormat::Ebook))
        .await
        .unwrap();

    e.svc.poll().await.unwrap();
    let row = tracked::get(&e.db, id).await.unwrap().unwrap();
    assert_eq!(row.state, TrackedState::Imported, "{:?}", row.status_messages);
    assert_eq!(
        row.output_path.as_deref(),
        Some(folder.to_string_lossy().as_ref())
    );
    let book = e.content.books.get_book(e.content.dune).await.unwrap();
    let f = book.ebook_file.expect("ebook file recorded");
    assert!(Path::new(&f.path).starts_with(&e.content.ebooks));
    // The slskd download the user started themselves was not adopted.
    assert_eq!(tracked::active(&e.db).await.unwrap().len(), 0);
}

#[tokio::test]
async fn failed_soulseek_folder_blocklists_only_that_peer() {
    let slskd = MockServer::start().await;
    let dir = "music/Radiohead/OK Computer";
    Mock::given(method("GET"))
        .and(path("/api/v0/transfers/downloads"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"username": "flaky", "directories": [{
            "directory": dir, "fileCount": 1, "files": [{"id": "4a5b6c7d-8e9f-4a0b-9c2d-3e4f5a6b7c8d",
            "username": "flaky", "direction": "Download", "filename": format!("{dir}/01 Airbag.flac"), "size": 5000000,
            "state": "Completed, Rejected", "requestedAt": "2026-11-05T14:06:00.0000000",
            "endedAt": "2026-11-05T14:06:30.0000000Z", "bytesTransferred": 0, "averageSpeed": 0,
            "exception": "Transfer rejected: File not shared.", "attempts": 1, "removed": false,
            "bytesRemaining": 5000000, "percentComplete": 0}]}]}])))
        .mount(&slskd)
        .await;
    let client: Arc<dyn DownloadClient> = Arc::from(
        reelix_downloaders::build(
            "slskd",
            &json!({"name": "slskd", "host": "127.0.0.1", "port": slskd.address().port(), "api_key": "k",
                    "downloads_dir": "/downloads", "remove_failed": false}),
            &reelix_downloaders::BuildContext::new(reqwest::Client::new()),
        )
        .unwrap(),
    );
    let e = activity_env_with(Some((8, client))).await;
    let title = "Radiohead - OK Computer [FLAC]";
    let mut rel = release(title, Quality::Flac);
    rel.protocol = Protocol::Soulseek;
    rel.soulseek = Some(reelix_core::soulseek::SoulseekRelease {
        username: "flaky".into(),
        directory: dir.into(),
        files: vec![],
        has_free_upload_slot: true,
        queue_length: 0,
        upload_speed: 0,
    });
    let id = tracked::insert(
        &e.db,
        &NewTracked {
            download_client_id: 8,
            download_id: format!("flaky/{dir}"),
            protocol: Protocol::Soulseek,
            title: title.into(),
            movie_id: None,
            series_id: None,
            episode_ids: vec![],
            release: rel,
        },
    )
    .await
    .unwrap();
    tracked::set_content(&e.db, id, &ContentRef::album(e.content.album))
        .await
        .unwrap();
    e.svc.poll().await.unwrap();
    assert_eq!(
        tracked::get(&e.db, id).await.unwrap().unwrap().state,
        TrackedState::Failed
    );
    let entries = blocklist::for_content(&e.db, &ContentRef::album(e.content.album))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].protocol, Some(Protocol::Soulseek));
    let key = format!("slsk:flaky/{dir}").to_ascii_lowercase();
    assert_eq!(entries[0].torrent_info_hash.as_deref(), Some(key.as_str()));
    assert!(blocklist::is_blocked(
        &entries,
        title,
        Some(&format!("slsk:flaky/{dir}"))
    ));
    assert!(!blocklist::is_blocked(
        &entries,
        title,
        Some(&format!("slsk:other/{dir}"))
    ));
    // The failure was recorded with the client's message.
    let hist = e
        .svc
        .history(&HistoryQuery {
            album_id: Some(e.content.album),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hist.items[0].event_type, "download_failed");
    assert_eq!(
        hist.items[0].download_id.as_deref(),
        Some(format!("flaky/{dir}").as_str())
    );
}
