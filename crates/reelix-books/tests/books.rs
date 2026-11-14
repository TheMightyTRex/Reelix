//! Books service flows against a fake Open Library and temp directories.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDate;
use reelix_api_types::books::*;
use reelix_api_types::content::{ContentSearchKind, CreateContentRootFolderRequest};
use reelix_books::testing::{FakeBookMetadata, author, work};
use reelix_books::{
    AvailabilityStatus, BooksDeps, BooksError, BooksService, ContentDownload, ScanSummary, Wanted,
};
use reelix_core::bus::EventBus;
use reelix_core::{BookFormat, ContentKind};
use reelix_db::Db;
use reelix_import::files::TransferMode;
use reelix_library::{DynMetadata, LibraryService};
use reelix_metadata::{EditionMetadata, SeriesRef};
use reelix_parser::{QualityFamily, parse_book};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

struct Env {
    svc: Arc<BooksService>,
    meta: Arc<FakeBookMetadata>,
    dir: tempfile::TempDir,
    ebook_root: i64,
    audio_root: i64,
    ebook_profile: i64,
    audio_profile: i64,
}

impl Env {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
}

fn sanderson() -> reelix_metadata::AuthorMetadata {
    let mut a = author("OL1A", "Brandon Sanderson");
    a.bio = Some("Writes long books.".into());
    a
}

fn provider() -> FakeBookMetadata {
    let a = sanderson();
    let mut empire = work("OL10W", "The Final Empire", &a, Some(2006));
    empire.series = vec![SeriesRef {
        title: "Mistborn #1".into(),
        position: None,
    }];
    let well = work("OL11W", "The Well of Ascension", &a, Some(2007));
    let future = work("OL12W", "Unwritten Book", &a, Some(2099));
    FakeBookMetadata::new()
        .with_author(a)
        .with_author(author("OL2A", "Frank Herbert"))
        .with_work(empire)
        .with_work(well)
        .with_work(future)
        .with_work(work(
            "OL20W",
            "Dune",
            &author("OL2A", "Frank Herbert"),
            Some(1965),
        ))
        .with_editions(
            "OL11W",
            vec![EditionMetadata {
                id: "OL100M".into(),
                title: "The Well of Ascension".into(),
                isbn13: Some("9780765316882".into()),
                series: vec![SeriesRef {
                    title: "Mistborn, Book 2".into(),
                    position: None,
                }],
                ..Default::default()
            }],
        )
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    let meta = Arc::new(provider());
    let svc = Arc::new(BooksService::new(BooksDeps {
        db: db.clone(),
        events,
        http: reqwest::Client::new(),
        library,
        metadata: meta.clone(),
    }));
    svc.set_today(NaiveDate::from_ymd_opt(2024, 6, 1).unwrap());
    for d in ["ebooks", "audiobooks", "downloads"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    let e = svc
        .add_root_folder(CreateContentRootFolderRequest {
            path: dir.path().join("ebooks").to_string_lossy().into(),
            kind: ContentKind::Ebook,
        })
        .await
        .unwrap();
    let a = svc
        .add_root_folder(CreateContentRootFolderRequest {
            path: dir.path().join("audiobooks").to_string_lossy().into(),
            kind: ContentKind::Audiobook,
        })
        .await
        .unwrap();
    let ep = db
        .quality_profiles()
        .list_family(QualityFamily::Ebook)
        .await
        .unwrap();
    let ap = db
        .quality_profiles()
        .list_family(QualityFamily::Audiobook)
        .await
        .unwrap();
    Env {
        svc,
        meta,
        ebook_root: e.id,
        audio_root: a.id,
        ebook_profile: ep[0].id,
        audio_profile: ap[0].id,
        dir,
    }
}

fn add_req(env: &Env, olid: &str, monitor: AuthorMonitor) -> AddAuthorRequest {
    AddAuthorRequest {
        openlibrary_id: olid.into(),
        quality_profile_id: Some(env.ebook_profile),
        root_folder_id: Some(env.ebook_root),
        audio_quality_profile_id: Some(env.audio_profile),
        audio_root_folder_id: Some(env.audio_root),
        monitored: true,
        monitor,
        monitor_new_items: NewBookMonitor::All,
        selected_work_ids: vec![],
        tags: vec![3, 1, 3],
        search_on_add: false,
    }
}

async fn books_of(env: &Env, author_id: i64) -> Vec<BookDto> {
    env.svc
        .list_books(&BookListQuery {
            author_id: Some(author_id),
            ..Default::default()
        })
        .await
        .unwrap()
        .items
}

fn by_title<'a>(books: &'a [BookDto], t: &str) -> &'a BookDto {
    books.iter().find(|b| b.title == t).unwrap()
}

/// A minimal EPUB 2 with the given title and ISBN identifier.
fn epub(title: &str, isbn: Option<&str>) -> Vec<u8> {
    let ident = isbn
        .map(|i| format!(r#"<dc:identifier opf:scheme="ISBN">{i}</dc:identifier>"#))
        .unwrap_or_default();
    let opf = format!(
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf"><dc:title>{title}</dc:title><dc:identifier id="uid">urn:uuid:42</dc:identifier>{ident}</metadata><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#
    );
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    w.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )
    .unwrap();
    w.write_all(b"application/epub+zip").unwrap();
    w.start_file("META-INF/container.xml", SimpleFileOptions::default())
        .unwrap();
    w.write_all(br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
    w.start_file("content.opf", SimpleFileOptions::default()).unwrap();
    w.write_all(opf.as_bytes()).unwrap();
    w.start_file("c.xhtml", SimpleFileOptions::default()).unwrap();
    w.write_all(b"<html/>").unwrap();
    w.finish().unwrap().into_inner()
}

fn opf_of(path: &Path) -> String {
    let mut z = ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut s = String::new();
    z.by_name("content.opf").unwrap().read_to_string(&mut s).unwrap();
    s
}

/// Silent MPEG-1 Layer III frames (128 kbps, 44.1 kHz).
fn mp3(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..frames {
        out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
        out.extend(std::iter::repeat_n(0u8, 413));
    }
    out
}

fn write(p: &Path, data: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

#[tokio::test]
async fn add_author_monitors_works_and_series() {
    let env = env().await;
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    assert_eq!(a.name, "Brandon Sanderson");
    assert_eq!(a.sort_name, "Sanderson, Brandon");
    assert_eq!(a.tags, vec![1, 3]);
    assert_eq!(a.statistics.book_count, 3);
    assert!(Path::new(a.path.as_deref().unwrap()).ends_with("ebooks/Brandon Sanderson"));
    assert!(Path::new(a.audio_path.as_deref().unwrap()).ends_with("audiobooks/Brandon Sanderson"));

    let books = books_of(&env, a.id).await;
    assert_eq!(books.len(), 3);
    assert!(books.iter().all(|b| b.monitored_ebook && b.monitored_audiobook));
    let empire = by_title(&books, "The Final Empire");
    assert_eq!(empire.series[0].title, "Mistborn");
    assert_eq!(empire.series[0].position.as_deref(), Some("1"));
    assert_eq!(empire.credits[0].author_id, Some(a.id));
    let well = by_title(&books, "The Well of Ascension");
    assert_eq!(well.isbn13.as_deref(), Some("9780765316882"));
    assert_eq!(well.series[0].position.as_deref(), Some("2"));
    let series = env.svc.book_series().await.unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].books.len(), 2);
    assert_eq!(series[0].books[0].1, empire.id);
    let eds = env.svc.editions(well.id).await.unwrap();
    assert_eq!(eds[0].openlibrary_edition_id, "OL100M");

    // Duplicates and validation.
    assert!(matches!(
        env.svc
            .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
            .await,
        Err(BooksError::Conflict(_))
    ));
    let mut bad = add_req(&env, "OL2A", AuthorMonitor::All);
    bad.quality_profile_id = None;
    bad.audio_quality_profile_id = None;
    assert!(matches!(
        env.svc.add_author(bad).await,
        Err(BooksError::Validation(_))
    ));
    let mut wrong = add_req(&env, "OL2A", AuthorMonitor::All);
    wrong.quality_profile_id = Some(env.audio_profile);
    assert!(matches!(
        env.svc.add_author(wrong).await,
        Err(BooksError::Validation(_))
    ));
    let mut wrong_root = add_req(&env, "OL2A", AuthorMonitor::All);
    wrong_root.root_folder_id = Some(env.audio_root);
    assert!(matches!(
        env.svc.add_author(wrong_root).await,
        Err(BooksError::Validation(_))
    ));

    // Lookup flags.
    let hits = env.svc.lookup_authors("sanderson").await.unwrap();
    assert_eq!(hits[0].author_id, Some(a.id));
    let hits = env.svc.lookup_authors("olid:OL2A").await.unwrap();
    assert_eq!(hits[0].name, "Frank Herbert");
    assert_eq!(hits[0].author_id, None);
    let bl = env.svc.lookup_books("Dune").await.unwrap();
    assert_eq!(bl[0].author_openlibrary_id.as_deref(), Some("OL2A"));
    let bl = env.svc.lookup_books("isbn:9780765316882").await.unwrap();
    assert_eq!(bl[0].book_id, Some(well.id));

    let list = env
        .svc
        .list_authors(&AuthorListQuery {
            search: Some("brandon".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(list.total, 1);
}

#[tokio::test]
async fn monitor_options_and_single_format() {
    let env = env().await;
    let mut req = add_req(&env, "OL1A", AuthorMonitor::Future);
    req.audio_quality_profile_id = None;
    req.audio_root_folder_id = None;
    let a = env.svc.add_author(req).await.unwrap();
    assert!(a.audio_path.is_none());
    let books = books_of(&env, a.id).await;
    assert!(by_title(&books, "Unwritten Book").monitored_ebook);
    assert!(!by_title(&books, "Unwritten Book").monitored_audiobook);
    assert!(!by_title(&books, "The Final Empire").monitored_ebook);
    env.svc.delete_author(a.id, false).await.unwrap();
    assert!(env.svc.book_series().await.unwrap().is_empty());

    let mut req = add_req(&env, "OL1A", AuthorMonitor::Selected);
    req.selected_work_ids = vec!["OL11W".into()];
    let a = env.svc.add_author(req).await.unwrap();
    let books = books_of(&env, a.id).await;
    let monitored: Vec<_> = books
        .iter()
        .filter(|b| b.monitored_ebook)
        .map(|b| b.title.as_str())
        .collect();
    assert_eq!(monitored, ["The Well of Ascension"]);

    // Toggle one format; disabling the ebook format unmonitors every ebook.
    let well = by_title(&books, "The Well of Ascension").id;
    let r = env
        .svc
        .monitor_books(BookMonitorRequest {
            book_ids: vec![well],
            format: Some(BookFormat::Audiobook),
            monitored: false,
        })
        .await
        .unwrap();
    assert!(r[0].monitored_ebook && !r[0].monitored_audiobook);
    let edited = env
        .svc
        .update_author(
            a.id,
            serde_json::from_str(r#"{"quality_profile_id":null,"monitored":false}"#).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(edited.quality_profile_id, None);
    assert!(!edited.monitored);
    assert!(!env.svc.get_book(well).await.unwrap().monitored_ebook);
    assert!(matches!(
        env.svc
            .update_author(
                a.id,
                serde_json::from_str(r#"{"audio_quality_profile_id":null}"#).unwrap()
            )
            .await,
        Err(BooksError::Validation(_))
    ));
}

#[tokio::test]
async fn add_book_creates_author_with_one_book() {
    let env = env().await;
    let b = env
        .svc
        .add_book(AddBookRequest {
            openlibrary_work_id: "OL20W".into(),
            formats: vec![BookFormat::Audiobook],
            quality_profile_id: Some(env.ebook_profile),
            root_folder_id: Some(env.ebook_root),
            audio_quality_profile_id: Some(env.audio_profile),
            audio_root_folder_id: Some(env.audio_root),
            tags: vec![],
            search_on_add: false,
            added_by_request_id: Some(7),
        })
        .await
        .unwrap();
    assert_eq!(b.title, "Dune");
    assert_eq!(b.author_name, "Frank Herbert");
    assert!(b.monitored_audiobook && !b.monitored_ebook);
    assert_eq!(b.added_by_request_id, Some(7));
    let a = env.svc.author_by_openlibrary("OL2A").await.unwrap().unwrap();
    assert_eq!(a.quality_profile_id, None);
    assert_eq!(a.audio_quality_profile_id, Some(env.audio_profile));
    let avail = env
        .svc
        .availability(&["OL20W".into(), "OL99W".into()])
        .await
        .unwrap();
    assert_eq!(avail["OL20W"].status, AvailabilityStatus::Monitored);
    assert_eq!(avail["OL20W"].library_id, Some(b.id));
    assert!(!avail.contains_key("OL99W"));
    assert!(matches!(
        env.svc
            .add_book(AddBookRequest {
                openlibrary_work_id: "OL20W".into(),
                formats: vec![],
                quality_profile_id: None,
                root_folder_id: None,
                audio_quality_profile_id: None,
                audio_root_folder_id: None,
                tags: vec![],
                search_on_add: false,
                added_by_request_id: None,
            })
            .await,
        Err(BooksError::Conflict(_))
    ));
}

#[tokio::test]
async fn scan_matches_files_and_folders() {
    let env = env().await;
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let ebooks = PathBuf::from(a.path.clone().unwrap());
    let audio = PathBuf::from(a.audio_path.clone().unwrap());
    // ISBN match despite an unhelpful name; name match for the second.
    write(&ebooks.join("random.epub"), &epub("x", Some("978-0-7653-1688-2")));
    write(&ebooks.join("The Final Empire/The Final Empire.mobi"), b"mobi");
    write(
        &ebooks.join("The Final Empire/The Final Empire.epub"),
        &epub("The Final Empire", None),
    );
    write(&ebooks.join("notes.txt"), b"x");
    write(&audio.join("The Final Empire (2006)/02.mp3"), &mp3(5));
    write(&audio.join("The Final Empire (2006)/01.mp3"), &mp3(5));
    write(&audio.join("Unrelated Thing.m4b"), b"m4b");

    let s = env.svc.scan_authors(&[a.id]).await.unwrap();
    assert_eq!(
        s,
        ScanSummary {
            added: 3,
            updated: 0,
            removed: 0
        }
    );
    let books = books_of(&env, a.id).await;
    let well = by_title(&books, "The Well of Ascension");
    assert_eq!(well.ebook_file.as_ref().unwrap().relative_path, "random.epub");
    let empire = by_title(&books, "The Final Empire");
    // EPUB ranks above MOBI in the default profile.
    let ef = empire.ebook_file.as_ref().unwrap();
    assert_eq!(ef.relative_path, "The Final Empire/The Final Empire.epub");
    assert_eq!(ef.quality_name, "EPUB");
    let af = empire.audiobook_file.as_ref().unwrap();
    assert_eq!(af.relative_path, "The Final Empire (2006)");
    assert_eq!(af.parts.len(), 2);
    assert!(af.parts[0].relative_path.ends_with("01.mp3"));
    assert_eq!(af.path, audio.join("The Final Empire (2006)").to_string_lossy());
    let a2 = env.svc.get_author(a.id).await.unwrap();
    assert_eq!(a2.statistics.ebook_file_count, 2);
    assert_eq!(a2.statistics.audiobook_file_count, 1);

    // Rescan is a no-op; removing a file drops its record.
    assert_eq!(
        env.svc.scan_authors(&[a.id]).await.unwrap(),
        ScanSummary::default()
    );
    std::fs::remove_file(ebooks.join("random.epub")).unwrap();
    let s = env.svc.scan_authors(&[]).await.unwrap();
    assert_eq!(s.removed, 1);

    // Wanted: the well has no ebook any more; the unreleased book is not wanted yet.
    let w = env.svc.wanted(&BookWantedQuery::default()).await.unwrap();
    let titles: Vec<_> = w.items.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["The Well of Ascension"]);
    let targets = env
        .svc
        .search_api()
        .targets(&[], None, Wanted::Missing)
        .await
        .unwrap();
    assert_eq!(targets.len(), 4, "well ×2, unwritten ×2");
    let t = env
        .svc
        .search_api()
        .target(empire.id, BookFormat::Audiobook)
        .await
        .unwrap();
    assert_eq!(t.quality_profile_id, env.audio_profile);
    assert!(t.existing.is_some());
    assert_eq!(t.tags, vec![1, 3]);

    let avail = env
        .svc
        .availability(&["OL10W".into(), "OL11W".into()])
        .await
        .unwrap();
    assert_eq!(avail["OL10W"].status, AvailabilityStatus::Available);
    assert_eq!(avail["OL11W"].status, AvailabilityStatus::Monitored);

    // Delete a multi-part file: parts and the folder go.
    env.svc.delete_book_file(af.id).await.unwrap();
    assert!(!audio.join("The Final Empire (2006)").exists());
}

#[tokio::test]
async fn match_release_by_isbn_and_title() {
    let env = env().await;
    env.svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let api = env.svc.search_api();
    let mut p = parse_book("whatever [epub]", None);
    p.isbn = Some("9780765316882".into());
    let (id, f) = api.match_release(&p).await.unwrap().unwrap();
    assert_eq!(env.svc.get_book(id).await.unwrap().title, "The Well of Ascension");
    assert_eq!(f, BookFormat::Ebook);

    let mut p = parse_book("Brandon Sanderson - The Final Empire (m4b)", None);
    p.author = Some("Brandon Sanderson".into());
    p.title = "The Final Empire".into();
    let (id, f) = api.match_release(&p).await.unwrap().unwrap();
    assert_eq!(env.svc.get_book(id).await.unwrap().title, "The Final Empire");
    assert_eq!(f, BookFormat::Audiobook);

    // Without a parsed author the raw title must contain author and title.
    let p = parse_book("Brandon.Sanderson-The.Well.of.Ascension.2007.RETAIL.EPUB", None);
    let (id, _) = api.match_release(&p).await.unwrap().unwrap();
    assert_eq!(env.svc.get_book(id).await.unwrap().title, "The Well of Ascension");
    assert!(
        api.match_release(&parse_book("Frank Herbert - Dune", None))
            .await
            .unwrap()
            .is_none()
    );
}

fn download(path: PathBuf, mode: TransferMode) -> ContentDownload {
    ContentDownload {
        source: path,
        release_title: Some("Brandon Sanderson - The Final Empire [EPUB]".into()),
        mode,
        download_client: Some("qbit".into()),
        download_id: Some("abc".into()),
    }
}

#[cfg(unix)]
fn inode(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).unwrap().ino()
}

#[tokio::test]
async fn import_ebook_writes_metadata_into_a_copy() {
    let env = env().await;
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let empire = by_title(&books_of(&env, a.id).await, "The Final Empire").id;
    let src = env.path("downloads/tfe/The Final Empire.epub");
    let original = epub("Seeding Title", None);
    write(&src, &original);
    write(&env.path("downloads/tfe/readme.nfo"), b"x");

    let out = env
        .svc
        .import_api()
        .import_download(
            &download(env.path("downloads/tfe"), TransferMode::HardlinkOrCopy),
            empire,
            BookFormat::Ebook,
        )
        .await
        .unwrap();
    assert!(out.rejections.is_empty(), "{:?}", out.rejections);
    assert_eq!(out.imported.len(), 1);
    let f = &out.imported[0];
    assert!(!f.is_upgrade);
    assert_eq!(f.quality.quality.name(), "EPUB");
    let dest = PathBuf::from(a.path.unwrap()).join("The Final Empire - Mistborn 1.epub");
    assert_eq!(f.destination, dest);
    assert!(dest.exists());
    // The seeding source is untouched and the library file is a separate copy.
    assert_eq!(std::fs::read(&src).unwrap(), original);
    #[cfg(unix)]
    assert_ne!(inode(&src), inode(&dest));
    let opf = opf_of(&dest);
    assert!(opf.contains("<dc:title>The Final Empire</dc:title>"), "{opf}");
    assert!(opf.contains("Brandon Sanderson"));
    assert!(opf.contains(r#"content="Mistborn""#));
    // No stage left behind.
    let leftovers: Vec<_> = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("reelix-partial"))
        .collect();
    assert!(leftovers.is_empty());

    let b = env.svc.get_book(empire).await.unwrap();
    let rec = b.ebook_file.unwrap();
    assert_eq!(
        rec.scene_name.as_deref(),
        Some("Brandon Sanderson - The Final Empire [EPUB]")
    );
    assert_eq!(rec.relative_path, "The Final Empire - Mistborn 1.epub");

    // Same quality again: not an upgrade.
    let again = env
        .svc
        .import_api()
        .import_download(
            &download(src.clone(), TransferMode::Copy),
            empire,
            BookFormat::Ebook,
        )
        .await
        .unwrap();
    assert!(again.imported.is_empty());
    assert!(
        again.rejections[0].contains("not an upgrade"),
        "{:?}",
        again.rejections
    );

    // Nothing usable in a download.
    let none = env
        .svc
        .import_api()
        .import_download(
            &download(env.path("downloads/tfe/readme.nfo"), TransferMode::Copy),
            empire,
            BookFormat::Ebook,
        )
        .await
        .unwrap();
    assert!(none.rejections[0].contains("no ebook files"));
    assert_eq!(
        env.svc
            .import_api()
            .destination_root(empire, BookFormat::Audiobook)
            .await
            .unwrap(),
        env.path("audiobooks/Brandon Sanderson")
    );
}

#[tokio::test]
async fn import_multi_file_audiobook_atomically() {
    let env = env().await;
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let well = by_title(&books_of(&env, a.id).await, "The Well of Ascension").id;
    let dl = env.path("downloads/woa");
    write(&dl.join("CD2/Track 01.mp3"), &mp3(30));
    write(&dl.join("CD1/Track 01.mp3"), &mp3(30));
    write(&dl.join("CD1/Track 02.mp3"), &mp3(30));
    write(&dl.join("cover.jpg"), &[0xFF, 0xD8, 0xFF, 0, 0, 0, 0, 0]);

    let mut d = download(dl.clone(), TransferMode::Move);
    d.release_title = Some("Brandon Sanderson - The Well of Ascension (Unabridged) MP3".into());
    let out = env
        .svc
        .import_api()
        .import_download(&d, well, BookFormat::Audiobook)
        .await
        .unwrap();
    assert!(out.rejections.is_empty(), "{:?}", out.rejections);
    assert_eq!(out.imported.len(), 3);
    let folder = PathBuf::from(a.audio_path.unwrap()).join("The Well of Ascension (2007)");
    let names: Vec<String> = out
        .imported
        .iter()
        .map(|f| {
            f.destination
                .strip_prefix(&folder)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        [
            "The Well of Ascension - Part 01.mp3",
            "The Well of Ascension - Part 02.mp3",
            "The Well of Ascension - Part 03.mp3"
        ]
    );
    assert!(out.imported[0].source.ends_with("CD1/Track 01.mp3"));
    // Tags on every part.
    for (i, f) in out.imported.iter().enumerate() {
        use lofty::prelude::*;
        let tf = lofty::read_from_path(&f.destination).unwrap();
        let tag = tf.primary_tag().unwrap();
        assert_eq!(tag.album().as_deref(), Some("The Well of Ascension"));
        assert_eq!(tag.track(), Some(i as u32 + 1));
    }
    // Move mode removed the sources after the commit.
    assert!(!dl.join("CD1/Track 01.mp3").exists());
    let b = env.svc.get_book(well).await.unwrap();
    let rec = b.audiobook_file.unwrap();
    assert_eq!(rec.relative_path, "The Well of Ascension (2007)");
    assert_eq!(rec.parts.len(), 3);
    assert_eq!(rec.size, 3 * 30 * 417);
}

#[cfg(unix)]
#[tokio::test]
async fn failed_import_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt;
    let env = env().await;
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let well = by_title(&books_of(&env, a.id).await, "The Well of Ascension").id;
    let dl = env.path("downloads/broken");
    write(&dl.join("01.mp3"), &mp3(3));
    write(&dl.join("02.mp3"), &mp3(3));
    let locked = dl.join("02.mp3");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&locked).is_ok() {
        return; // running as root: permissions are not enforced
    }
    let res = env
        .svc
        .import_api()
        .import_download(
            &download(dl.clone(), TransferMode::Copy),
            well,
            BookFormat::Audiobook,
        )
        .await;
    assert!(res.is_err());
    let audio = PathBuf::from(a.audio_path.unwrap());
    let entries: Vec<_> = std::fs::read_dir(&audio)
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(entries.is_empty(), "{entries:?}");
    assert!(env.svc.get_book(well).await.unwrap().audiobook_file.is_none());
    assert!(dl.join("01.mp3").exists());
}

#[tokio::test]
async fn rename_preview_and_execute() {
    let env = env().await;
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let ebooks = PathBuf::from(a.path.clone().unwrap());
    write(&ebooks.join("The Well of Ascension/well.pdf"), b"%PDF");
    env.svc.scan_authors(&[a.id]).await.unwrap();
    let p = env.svc.rename_preview(a.id).await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].existing_path, "The Well of Ascension/well.pdf");
    assert_eq!(p[0].new_path, "The Well of Ascension - Mistborn 2.pdf");
    assert_eq!(env.svc.execute_rename(a.id).await.unwrap(), 1);
    assert!(ebooks.join("The Well of Ascension - Mistborn 2.pdf").exists());
    assert!(!ebooks.join("The Well of Ascension").exists());
    assert!(env.svc.rename_preview(a.id).await.unwrap().is_empty());
    let well = by_title(&books_of(&env, a.id).await, "The Well of Ascension").clone();
    assert_eq!(
        well.ebook_file.unwrap().relative_path,
        "The Well of Ascension - Mistborn 2.pdf"
    );
}

#[tokio::test]
async fn refresh_adds_new_works_per_monitor_new_items() {
    let env = env().await;
    let mut req = add_req(&env, "OL1A", AuthorMonitor::None);
    req.monitor_new_items = NewBookMonitor::None;
    let a = env.svc.add_author(req).await.unwrap();
    env.meta
        .put_work(work("OL13W", "The Hero of Ages", &sanderson(), Some(2008)));
    assert_eq!(env.svc.refresh_authors(Some(vec![a.id]), true).await.unwrap(), 0);
    let books = books_of(&env, a.id).await;
    assert_eq!(books.len(), 4);
    assert!(!by_title(&books, "The Hero of Ages").monitored_ebook);
    env.svc
        .update_author(
            a.id,
            serde_json::from_str(r#"{"monitor_new_items":"all"}"#).unwrap(),
        )
        .await
        .unwrap();
    env.meta
        .put_work(work("OL14W", "Elantris", &sanderson(), Some(2005)));
    // A scheduled refresh skips recently refreshed authors; a manual one does not.
    env.svc.refresh_authors(None, false).await.unwrap();
    assert_eq!(books_of(&env, a.id).await.len(), 4);
    env.svc.refresh_authors(None, true).await.unwrap();
    let books = books_of(&env, a.id).await;
    assert!(by_title(&books, "Elantris").monitored_ebook);
    assert!(
        env.meta
            .calls()
            .iter()
            .filter(|c| c.starts_with("author:"))
            .count()
            >= 3
    );
}

#[tokio::test]
async fn library_import_and_root_folders() {
    let env = env().await;
    write(
        &env.path("ebooks/Brandon Sanderson/The Final Empire.epub"),
        &epub("The Final Empire", None),
    );
    let cands = env.svc.import_scan(env.ebook_root, true).await.unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].guessed_author.as_deref(), Some("Brandon Sanderson"));
    assert_eq!(cands[0].suggestions[0].openlibrary_id, "OL1A");
    assert_eq!(cands[0].lookup_error, None);
    // Fast listing without Open Library, then matching in a batch.
    let quick = env.svc.import_scan(env.ebook_root, false).await.unwrap();
    assert!(quick[0].suggestions.is_empty());
    let matched = env
        .svc
        .import_match(env.ebook_root, vec![quick[0].path.clone()])
        .await
        .unwrap();
    assert_eq!(matched[0].suggestions[0].openlibrary_id, "OL1A");
    assert_eq!(matched[0].folder, "Brandon Sanderson");
    assert_eq!(matched[0].guessed_author.as_deref(), Some("Brandon Sanderson"));
    let outside = env.path("audiobooks/x").to_string_lossy().into_owned();
    assert!(env.svc.import_match(env.ebook_root, vec![outside]).await.is_err());
    // At most 25 folders per match request.
    let too_many = vec![quick[0].path.clone(); 26];
    assert!(env.svc.import_match(env.ebook_root, too_many).await.is_err());
    let r = env
        .svc
        .library_import(BookLibraryImportRequest {
            root_folder_id: env.ebook_root,
            items: vec![BookImportItem {
                path: cands[0].path.clone(),
                openlibrary_id: "OL1A".into(),
                quality_profile_id: Some(env.ebook_profile),
                audio_quality_profile_id: None,
                monitor: AuthorMonitor::Missing,
                tags: vec![],
            }],
        })
        .await
        .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let books = books_of(&env, r.added[0]).await;
    let empire = by_title(&books, "The Final Empire");
    assert!(empire.ebook_file.is_some());
    assert!(
        !empire.monitored_ebook,
        "missing: books with files are unmonitored"
    );
    assert!(by_title(&books, "The Well of Ascension").monitored_ebook);
    assert!(
        env.svc
            .import_scan(env.ebook_root, true)
            .await
            .unwrap()
            .is_empty()
    );

    // Root folders: in use → conflict; kind checks.
    assert!(matches!(
        env.svc.delete_root_folder(env.ebook_root).await,
        Err(BooksError::Conflict(_))
    ));
    assert!(matches!(
        env.svc
            .add_root_folder(CreateContentRootFolderRequest {
                path: env.path("downloads").to_string_lossy().into(),
                kind: ContentKind::Music,
            })
            .await,
        Err(BooksError::Validation(_))
    ));
    let roots = env.svc.root_folders().await.unwrap();
    assert_eq!(roots.len(), 2);
    env.svc.delete_root_folder(env.audio_root).await.unwrap();

    // Settings validation.
    let mut s = env.svc.settings().await.unwrap();
    assert_eq!(s, BookSettingsDto::default());
    s.naming.ebook_file = "{Book Title} {Bogus}".into();
    assert!(matches!(
        env.svc.set_settings(s.clone()).await,
        Err(BooksError::Validation(_))
    ));
    s.naming.ebook_file = "{Author Name} - {Book Title}".into();
    assert_eq!(
        env.svc.set_settings(s.clone()).await.unwrap().naming.ebook_file,
        s.naming.ebook_file
    );

    // Searches need ids for item/parent searches.
    assert!(matches!(
        env.svc
            .request_search(BookSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![],
                format: None
            })
            .await,
        Err(BooksError::Validation(_))
    ));
}

/// RFX-86: Audible gives the recording, Audnexus fills the narrator Audible left out, the
/// answer is stored and reused, the book gets its ASIN, and a failed refresh keeps it.
#[tokio::test]
async fn audiobook_details_from_audible_and_audnexus() {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let env = env().await;
    let audible = MockServer::start().await;
    let nexus = MockServer::start().await;
    env.svc
        .set_audiobook_sources_options(reelix_metadata::AudiobookSourcesOptions {
            audible_url: Some(audible.uri()),
            audnexus_url: nexus.uri(),
            retry: reelix_metadata::http::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            per_second: 100,
            ..Default::default()
        });
    let a = env
        .svc
        .add_author(add_req(&env, "OL1A", AuthorMonitor::All))
        .await
        .unwrap();
    let books = books_of(&env, a.id).await;
    let empire = by_title(&books, "The Final Empire");
    assert!(empire.monitored_audiobook);
    assert!(empire.asin.is_none());

    Mock::given(method("GET"))
        .and(path("/1.0/catalog/products"))
        .and(query_param("title", "The Final Empire"))
        .and(query_param("author", "Brandon Sanderson"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"products": [
            {"asin": "B002UZMLXM", "title": "The Final Empire", "format_type": "abridged",
             "authors": [{"name": "Brandon Sanderson"}], "narrators": [{"name": "Somebody Else"}]},
            {"asin": "B002UZJGYY", "title": "The Final Empire: Mistborn, Book 1", "format_type": "unabridged",
             "authors": [{"name": "Brandon Sanderson"}], "runtime_length_min": 1490,
             "release_date": "2008-08-15", "language": "english", "publisher_name": "Recorded Books",
             "series": [{"title": "Mistborn", "sequence": "1"}]},
            {"asin": "B000000001", "title": "Elantris", "authors": [{"name": "Brandon Sanderson"}]}
        ]})))
        .expect(1)
        .mount(&audible)
        .await;
    Mock::given(method("GET"))
        .and(path("/books/B002UZJGYY"))
        .and(query_param("region", "us"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "asin": "B002UZJGYY", "title": "The Final Empire: Mistborn, Book 1",
            "authors": [{"name": "Brandon Sanderson"}], "narrators": [{"name": "Michael Kramer"}],
            "runtimeLengthMin": 1490
        })))
        .expect(1)
        .mount(&nexus)
        .await;

    let d = env.svc.audiobook_details(empire.id, false).await.unwrap();
    assert!(d.found);
    assert_eq!(
        d.asin.as_deref(),
        Some("B002UZJGYY"),
        "the unabridged recording wins"
    );
    assert_eq!(d.narrators, vec!["Michael Kramer".to_string()]);
    assert_eq!(d.runtime_minutes, Some(1490));
    assert_eq!(d.abridged, Some(false));
    assert_eq!(d.recording_date.as_deref(), Some("2008-08-15"));
    assert_eq!(d.sources, vec!["audible".to_string(), "audnexus".to_string()]);
    assert_eq!(d.series[0].title, "Mistborn");
    let b = env.svc.get_book(empire.id).await.unwrap();
    assert_eq!(b.asin.as_deref(), Some("B002UZJGYY"));

    // Stored: asked again, nobody is called (the mocks expect one call each).
    let again = env.svc.audiobook_details(empire.id, false).await.unwrap();
    assert_eq!(again.narrators, d.narrators);
    audible.verify().await;
    nexus.verify().await;

    // A forced refresh that fails keeps the stored details.
    audible.reset().await;
    nexus.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&audible)
        .await;
    let kept = env.svc.audiobook_details(empire.id, true).await.unwrap();
    assert_eq!(kept.asin.as_deref(), Some("B002UZJGYY"));
    assert_eq!(kept.narrators, d.narrators);

    // A book no source knows is stored as not found; with nothing stored a failure is an error.
    let well = by_title(&books, "The Well of Ascension");
    assert!(env.svc.audiobook_details(well.id, false).await.is_err());
    audible.reset().await;
    Mock::given(method("GET"))
        .and(path("/1.0/catalog/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"products": []})))
        .mount(&audible)
        .await;
    let none = env.svc.audiobook_details(well.id, false).await.unwrap();
    assert!(!none.found);
    assert!(none.narrators.is_empty());

    // The refresh job looks up only monitored audiobooks that are missing or stale.
    let n = env.svc.refresh_audiobooks_of(&[a.id]).await.unwrap();
    let monitored = books
        .iter()
        .filter(|b| b.monitored_audiobook && b.id != empire.id && b.id != well.id)
        .count();
    assert_eq!(n, monitored);
}
