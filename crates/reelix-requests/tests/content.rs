//! Book and album requests (M8/M9) against an in-memory DB and the in-memory fakes.

use std::collections::BTreeMap;
use std::sync::Arc;

use reelix_api_types::requests::*;
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, BookFormat, ContentKind, Permissions, User};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_notify::NotificationEvent;
use reelix_requests::fakes::{self, FakeDirectory, FakeLibrary, FakeMetadata, FakeNotifications};
use reelix_requests::{ArtistHit, Availability, Error, RequestDeps, RequestService};

const DUNE: &str = "OL893415W";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";

struct T {
    svc: RequestService,
    lib: FakeLibrary,
    notes: FakeNotifications,
    admin: User,
    /// ROLE_USER + REQUEST_BOOK + REQUEST_MUSIC (no auto-approve).
    user: User,
}

async fn user(db: &Db, name: &str, p: Permissions) -> User {
    db.users()
        .create(NewUser {
            username: name.into(),
            email: Some(format!("{name}@example.com")),
            display_name: None,
            password_hash: Some("x".into()),
            auth_source: AuthSource::Local,
            permissions: p,
        })
        .await
        .unwrap()
}

async fn setup() -> T {
    let db = Db::in_memory().await.unwrap();
    let lib = FakeLibrary::default()
        .with_content(
            ContentRequestKind::Book,
            fakes::book(DUNE, "Dune", "Frank Herbert"),
        )
        .with_content(
            ContentRequestKind::Book,
            fakes::book("OL1W", "Dune Messiah", "Frank Herbert"),
        )
        .with_content(
            ContentRequestKind::Music,
            fakes::album(OK_COMPUTER, "OK Computer", "Radiohead"),
        );
    lib.state().artist_hits.push(ArtistHit {
        musicbrainz_id: "a74b1b7f".into(),
        name: "Radiohead".into(),
        library_id: Some(7),
        ..Default::default()
    });
    let notes = FakeNotifications::default();
    let svc = RequestService::new(RequestDeps {
        db: db.clone(),
        events: EventBus::default(),
        secrets: SecretBox::from_key([3; 32]),
        library: Arc::new(lib.clone()),
        media_servers: Arc::new(FakeDirectory::new("http://127.0.0.1:9")),
        metadata: Arc::new(FakeMetadata::default()),
        notifications: Arc::new(notes.clone()),
        http: None,
    });
    let admin = user(&db, "admin", Permissions::ROLE_ADMIN).await;
    let u = user(
        &db,
        "alice",
        Permissions::ROLE_USER | Permissions::REQUEST_BOOK | Permissions::REQUEST_MUSIC,
    )
    .await;
    T {
        svc,
        lib,
        notes,
        admin,
        user: u,
    }
}

fn book_req(id: &str, formats: Vec<BookFormat>) -> CreateContentRequestDto {
    CreateContentRequestDto {
        kind: ContentRequestKind::Book,
        foreign_id: id.into(),
        formats,
        profile_id: None,
        root_folder_id: None,
        tags: vec![],
        user_id: None,
    }
}

fn album_req(id: &str) -> CreateContentRequestDto {
    CreateContentRequestDto {
        kind: ContentRequestKind::Music,
        ..book_req(id, vec![])
    }
}

fn avail(status: AvailabilityStatus, id: i64) -> Availability {
    Availability {
        status,
        library_id: Some(id),
        seasons: BTreeMap::new(),
    }
}

#[tokio::test]
async fn book_request_auto_approve_add_and_sync_to_available() {
    let t = setup().await;
    let mut s = t.svc.request_settings().await.unwrap();
    s.auto_approve_book = true;
    s.book_ebook.profile_id = Some(11);
    s.book_ebook.root_folder_id = Some(21);
    s.book_ebook.tags = vec![5];
    s.book_audiobook.profile_id = Some(12);
    t.svc.set_request_settings(s).await.unwrap();

    let r = t
        .svc
        .create_content_request(&t.user, book_req(DUNE, vec![BookFormat::Ebook]))
        .await
        .unwrap();
    assert_eq!(r.status, RequestStatus::Approved);
    assert_eq!(r.kind, ContentRequestKind::Book);
    assert_eq!(r.title, "Dune");
    assert_eq!(r.subtitle.as_deref(), Some("Frank Herbert"));
    assert_eq!(r.formats, vec![BookFormat::Ebook]);
    let book_id = r.book_id.unwrap();
    assert_eq!(r.author_id, Some(1000 + book_id));
    {
        let st = t.lib.state();
        let add = &st.added_books[0];
        assert_eq!(add.work_id, DUNE);
        assert_eq!(add.formats, vec![BookFormat::Ebook]);
        assert_eq!(add.ebook_profile_id, Some(11));
        assert_eq!(add.ebook_root_folder_id, Some(21));
        assert_eq!(add.audiobook_profile_id, Some(12));
        assert_eq!(add.tags, vec![5]);
        assert_eq!(add.request_id, r.id);
        assert_eq!(
            st.content_searches,
            vec![(ContentRequestKind::Book, vec![book_id], vec![BookFormat::Ebook])]
        );
    }
    let n = t.notes.take();
    match &n[0].0 {
        NotificationEvent::RequestApproved { media, .. } => {
            assert_eq!(media.content, Some(ContentKind::Ebook));
            assert_eq!(media.title, "Dune");
            assert_eq!(media.id, Some(book_id));
        }
        other => panic!("unexpected {other:?}"),
    }

    // A second ebook request is a duplicate; the audiobook is still requestable.
    assert!(matches!(
        t.svc
            .create_content_request(&t.user, book_req(DUNE, vec![BookFormat::Ebook]))
            .await,
        Err(Error::Conflict(_))
    ));
    let detail = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Book, DUNE)
        .await
        .unwrap();
    assert!(detail.request_ability.can_request);
    assert_eq!(detail.request_ability.formats, vec![BookFormat::Audiobook]);
    assert_eq!(detail.requests.len(), 1);
    assert_eq!(detail.overview.as_deref(), Some("Dune overview"));

    // Download in progress → processing; file imported → available (+ notification).
    t.lib.set_content(
        ContentRequestKind::Book,
        DUNE,
        avail(AvailabilityStatus::Processing, book_id),
    );
    assert_eq!(t.svc.sync_request_statuses().await.unwrap(), 1);
    let r = t.svc.get_content_request(&t.user, r.id).await.unwrap();
    assert_eq!(r.status, RequestStatus::Processing);
    t.lib.set_content(
        ContentRequestKind::Book,
        DUNE,
        avail(AvailabilityStatus::Available, book_id),
    );
    assert_eq!(t.svc.sync_content_statuses().await.unwrap(), 1);
    let r = t.svc.get_content_request(&t.user, r.id).await.unwrap();
    assert_eq!(r.status, RequestStatus::Available);
    assert!(r.available_at.is_some());
    assert!(matches!(
        t.notes.take().last().unwrap().0,
        NotificationEvent::RequestAvailable { .. }
    ));
    // Now it is available: nothing left to request.
    assert!(matches!(
        t.svc
            .create_content_request(&t.user, book_req(DUNE, vec![BookFormat::Audiobook]))
            .await,
        Err(Error::Conflict(_))
    ));

    // Search results carry availability and the request.
    let page = t.svc.search_books(&t.user, "dune").await.unwrap();
    assert_eq!(page.results.len(), 2);
    let dune = page.results.iter().find(|i| i.foreign_id == DUNE).unwrap();
    assert_eq!(dune.availability, AvailabilityStatus::Available);
    assert_eq!(dune.library_id, Some(book_id));
    assert_eq!(dune.request.as_ref().unwrap().status, RequestStatus::Available);
}

#[tokio::test]
async fn album_request_pending_approve_and_sync() {
    let t = setup().await;
    let r = t
        .svc
        .create_content_request(&t.user, album_req(OK_COMPUTER))
        .await
        .unwrap();
    assert_eq!(r.status, RequestStatus::Pending);
    assert_eq!(r.subtitle.as_deref(), Some("Radiohead"));
    assert!(r.formats.is_empty());
    match &t.notes.take()[0].0 {
        NotificationEvent::RequestPending { media, .. } => {
            assert_eq!(media.content, Some(ContentKind::Music))
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        t.svc.approve_content_request(&t.user, r.id).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        t.svc
            .create_content_request(&t.user, album_req(OK_COMPUTER))
            .await,
        Err(Error::Conflict(_))
    ));
    // Movie/TV listings do not see book/album requests; the media listing does.
    let movies = t
        .svc
        .list_requests(&t.admin, &RequestListQuery::default())
        .await
        .unwrap();
    assert_eq!(movies.total, 0);
    let media = t
        .svc
        .list_content_requests(
            &t.admin,
            &RequestListQuery {
                kind: Some("music".into()),
                status: Some("pending".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(media.total, 1);
    let counts = t.svc.request_counts(&t.admin).await.unwrap();
    assert_eq!((counts.total, counts.pending, counts.music), (1, 1, 1));

    let mut s = t.svc.request_settings().await.unwrap();
    s.music.profile_id = Some(31);
    s.music_metadata_profile_id = Some(2);
    s.search_on_approve = false;
    t.svc.set_request_settings(s).await.unwrap();
    let a = t.svc.approve_content_request(&t.admin, r.id).await.unwrap();
    assert_eq!(a.status, RequestStatus::Approved);
    let album_id = a.album_id.unwrap();
    assert_eq!(a.artist_id, Some(2000 + album_id));
    {
        let st = t.lib.state();
        assert_eq!(st.added_albums[0].quality_profile_id, Some(31));
        assert_eq!(st.added_albums[0].metadata_profile_id, Some(2));
        assert!(st.content_searches.is_empty());
    }
    t.lib.set_content(
        ContentRequestKind::Music,
        OK_COMPUTER,
        avail(AvailabilityStatus::PartiallyAvailable, album_id),
    );
    t.svc.sync_request_statuses().await.unwrap();
    assert_eq!(
        t.svc.get_content_request(&t.user, r.id).await.unwrap().status,
        RequestStatus::PartiallyAvailable
    );
    t.lib.set_content(
        ContentRequestKind::Music,
        OK_COMPUTER,
        avail(AvailabilityStatus::Available, album_id),
    );
    t.svc.sync_request_statuses().await.unwrap();
    assert_eq!(
        t.svc.get_content_request(&t.user, r.id).await.unwrap().status,
        RequestStatus::Available
    );

    let music = t.svc.search_music(&t.user, "radiohead").await.unwrap();
    assert_eq!(music.artists.len(), 1);
    assert_eq!(music.artists[0].library_id, Some(7));
    let music = t.svc.search_music(&t.user, "ok computer").await.unwrap();
    assert_eq!(music.albums[0].availability, AvailabilityStatus::Available);
}

#[tokio::test]
async fn failed_add_retry_decline_and_delete() {
    let t = setup().await;
    t.lib.state().fail_adds = Some("no music root folder exists".into());
    let r = t
        .svc
        .create_content_request(&t.admin, album_req(OK_COMPUTER))
        .await
        .unwrap();
    assert_eq!(r.status, RequestStatus::Failed);
    assert_eq!(r.failure_reason.as_deref(), Some("no music root folder exists"));
    t.lib.state().fail_adds = None;
    let r = t.svc.retry_content_request(&t.admin, r.id).await.unwrap();
    assert_eq!(r.status, RequestStatus::Approved);

    let b = t
        .svc
        .create_content_request(&t.user, book_req(DUNE, vec![]))
        .await
        .unwrap();
    assert_eq!(b.formats, BookFormat::ALL.to_vec());
    // Owner edits formats while pending.
    let b = t
        .svc
        .update_content_request(
            &t.user,
            b.id,
            UpdateContentRequestDto {
                formats: Some(vec![BookFormat::Audiobook]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(b.formats, vec![BookFormat::Audiobook]);
    let d = t
        .svc
        .decline_content_request(&t.admin, b.id, Some("not now".into()))
        .await
        .unwrap();
    assert_eq!(d.status, RequestStatus::Declined);
    assert_eq!(d.decline_reason.as_deref(), Some("not now"));
    t.svc.delete_content_request(&t.user, b.id).await.unwrap();
    assert!(matches!(
        t.svc.get_content_request(&t.admin, b.id).await,
        Err(Error::NotFound(_))
    ));
    // Book requests cannot be read through the movie/TV endpoints.
    assert!(matches!(
        t.svc.get_request(&t.admin, r.id).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn permissions_and_quotas() {
    let t = setup().await;
    let db = t.svc.db().clone();
    let plain = user(&db, "bob", Permissions::ROLE_USER).await;
    assert!(matches!(
        t.svc.create_content_request(&plain, book_req(DUNE, vec![])).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        t.svc.create_content_request(&plain, album_req(OK_COMPUTER)).await,
        Err(Error::Forbidden(_))
    ));
    let detail = t
        .svc
        .content_detail(&plain, ContentRequestKind::Music, OK_COMPUTER)
        .await
        .unwrap();
    assert!(!detail.request_ability.can_request);
    // Advanced options need REQUEST_ADVANCED.
    let mut adv = book_req(DUNE, vec![BookFormat::Ebook]);
    adv.profile_id = Some(3);
    assert!(matches!(
        t.svc.create_content_request(&t.user, adv).await,
        Err(Error::Forbidden(_))
    ));
    // Unknown ids are 404.
    assert!(matches!(
        t.svc
            .create_content_request(&t.user, book_req("OL404W", vec![]))
            .await,
        Err(Error::NotFound(_))
    ));

    // Global book quota of 1 per week.
    let mut s = t.svc.request_settings().await.unwrap();
    s.book_quota_limit = Some(1);
    t.svc.set_request_settings(s).await.unwrap();
    t.svc
        .create_content_request(&t.user, book_req(DUNE, vec![]))
        .await
        .unwrap();
    let q = t.svc.quota(&t.user, t.user.id).await.unwrap();
    assert_eq!((q.book.used, q.book.limit, q.book.restricted), (1, Some(1), true));
    assert_eq!(q.music.used, 0);
    match t
        .svc
        .create_content_request(&t.user, book_req("OL1W", vec![]))
        .await
    {
        Err(Error::Forbidden(m)) => assert!(m.contains("quota"), "{m}"),
        other => panic!("expected quota error, got {other:?}"),
    }
    let detail = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Book, "OL1W")
        .await
        .unwrap();
    assert!(!detail.request_ability.can_request);
    assert!(detail.request_ability.reason.unwrap().contains("quota"));
    // Albums have their own quota; managers are exempt.
    t.svc
        .create_content_request(&t.user, album_req(OK_COMPUTER))
        .await
        .unwrap();
    t.svc
        .create_content_request(&t.admin, book_req("OL1W", vec![]))
        .await
        .unwrap();

    // Per-user override beats the global default.
    let u = t.svc.get_user(&t.admin, t.user.id).await.unwrap();
    let upd = UpdateUserDto {
        display_name: None,
        email: None,
        avatar: None,
        locale: None,
        permissions: u.permissions,
        movie_quota_limit: None,
        movie_quota_days: None,
        tv_quota_limit: None,
        tv_quota_days: None,
        book_quota_limit: Some(5),
        book_quota_days: Some(30),
        music_quota_limit: Some(0),
        music_quota_days: None,
        password: None,
        current_password: None,
    };
    let saved = t.svc.update_user(&t.admin, t.user.id, upd, None).await.unwrap();
    assert_eq!(saved.book_quota_limit, Some(5));
    assert_eq!(saved.book_quota_days, Some(30));
    assert_eq!(saved.music_quota_limit, Some(0));
    let q = t.svc.quota(&t.user, t.user.id).await.unwrap();
    assert_eq!(
        (q.book.limit, q.book.days, q.book.restricted),
        (Some(5), 30, false)
    );
    assert!(q.music.restricted);
}

#[tokio::test]
async fn options_need_advanced_and_list_content_profiles() {
    let t = setup().await;
    assert!(matches!(
        t.svc.content_request_options(&t.user, ContentKind::Ebook).await,
        Err(Error::Forbidden(_))
    ));
    let mut s = t.svc.request_settings().await.unwrap();
    s.book_audiobook.profile_id = Some(100);
    t.svc.set_request_settings(s).await.unwrap();
    let o = t
        .svc
        .content_request_options(&t.admin, ContentKind::Audiobook)
        .await
        .unwrap();
    assert_eq!(o.profiles.len(), 1);
    assert_eq!(o.default_profile_id, Some(100));
    assert!(matches!(
        t.svc.content_request_options(&t.admin, ContentKind::Movie).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn discover_rows_overlay_availability_and_requests() {
    let t = setup().await;
    t.lib.state().content_rows.insert(
        (ContentRequestKind::Book, "trending".into()),
        vec![
            fakes::book(DUNE, "Dune", "Frank Herbert"),
            fakes::book("OL1W", "Dune Messiah", "Frank Herbert"),
        ],
    );
    t.lib.state().content_rows.insert(
        (ContentRequestKind::Music, "popular-week".into()),
        vec![fakes::album(OK_COMPUTER, "OK Computer", "Radiohead")],
    );
    t.lib.set_content(
        ContentRequestKind::Book,
        "OL1W",
        Availability {
            status: AvailabilityStatus::Available,
            library_id: Some(5),
            ..Default::default()
        },
    );
    t.svc
        .create_content_request(&t.user, album_req(OK_COMPUTER))
        .await
        .unwrap();

    let books = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Book, "trending")
        .await
        .unwrap();
    let titles: Vec<_> = books.results.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Dune", "Dune Messiah"]);
    assert_eq!(books.results[0].kind, ContentRequestKind::Book);
    assert_eq!(books.results[1].availability, AvailabilityStatus::Available);
    assert_eq!(books.results[1].library_id, Some(5));

    let music = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Music, "popular-week")
        .await
        .unwrap();
    assert_eq!(music.results.len(), 1);
    assert_eq!(
        music.results[0].request.as_ref().unwrap().status,
        RequestStatus::Pending
    );

    // A known row with nothing in it is empty, not an error.
    let empty = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Music, "new-releases")
        .await
        .unwrap();
    assert!(empty.results.is_empty());

    // hide_available applies to these rows too.
    let mut st = t.svc.request_settings().await.unwrap();
    st.hide_available = true;
    t.svc.set_request_settings(st).await.unwrap();
    let books = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Book, "trending")
        .await
        .unwrap();
    let titles: Vec<_> = books.results.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Dune"]);
}

#[tokio::test]
async fn discover_rows_must_be_known() {
    let t = setup().await;
    for (kind, row) in [
        (ContentRequestKind::Book, "popular-week"),
        (ContentRequestKind::Book, "subject-anything"),
        (ContentRequestKind::Music, "trending"),
    ] {
        assert!(
            matches!(
                t.svc.discover_content(&t.user, kind, row).await,
                Err(Error::NotFound(_))
            ),
            "{kind:?}/{row}"
        );
    }
    // Every advertised row is accepted.
    for kind in ContentRequestKind::ALL {
        for (row, _) in kind.discover_rows() {
            t.svc.discover_content(&t.user, kind, row).await.unwrap();
        }
    }
}
