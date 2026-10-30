//! RequestService flows against an in-memory DB and the in-memory fakes.

use std::sync::Arc;

use reelix_api_types::requests::*;
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, MediaKind, Permissions, User};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_notify::NotificationEvent;
use reelix_requests::fakes::{
    self, FakeDirectory, FakeLibrary, FakeMediaServer, FakeMetadata, FakeNotifications,
};
use reelix_requests::{Availability, Error, MediaServerKind, RequestDeps, RequestService};

struct T {
    svc: RequestService,
    lib: FakeLibrary,
    notes: FakeNotifications,
    db: Db,
    admin: User,
    user: User,
}

fn metadata() -> FakeMetadata {
    FakeMetadata::default()
        .with_movie(fakes::movie(603, "The Matrix", 1999))
        .with_movie(fakes::movie(604, "The Matrix Reloaded", 2003))
        .with_movie(fakes::movie(605, "The Matrix Revolutions", 2003))
        .with_series(fakes::series(1399, "Game of Thrones", 3))
        .with_hits(vec![
            fakes::hit(MediaKind::Movie, 603, "The Matrix"),
            fakes::hit(MediaKind::Series, 1399, "Game of Thrones"),
        ])
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

async fn setup_with(dir: FakeDirectory) -> T {
    let db = Db::in_memory().await.unwrap();
    let lib = FakeLibrary::default();
    let notes = FakeNotifications::default();
    let svc = RequestService::new(RequestDeps {
        db: db.clone(),
        events: EventBus::default(),
        secrets: SecretBox::from_key([3; 32]),
        library: Arc::new(lib.clone()),
        media_servers: Arc::new(dir),
        metadata: Arc::new(metadata()),
        notifications: Arc::new(notes.clone()),
        http: None,
    });
    let admin = user(&db, "admin", Permissions::ROLE_ADMIN).await;
    let u = user(&db, "alice", Permissions::ROLE_USER).await;
    T {
        svc,
        lib,
        notes,
        db,
        admin,
        user: u,
    }
}

async fn setup() -> T {
    setup_with(FakeDirectory::new("http://127.0.0.1:9")).await
}

fn movie_req(id: u32) -> CreateRequestDto {
    CreateRequestDto {
        kind: MediaKind::Movie,
        tmdb_id: id,
        seasons: None,
        is_4k: false,
        profile_id: None,
        root_folder_id: None,
        tags: vec![],
        user_id: None,
    }
}

fn tv_req(seasons: Option<Vec<u32>>) -> CreateRequestDto {
    CreateRequestDto {
        kind: MediaKind::Series,
        tmdb_id: 1399,
        seasons,
        ..movie_req(0)
    }
}

#[tokio::test]
async fn movie_request_lifecycle() {
    let t = setup().await;
    let r = t.svc.create_request(&t.user, movie_req(603)).await.unwrap();
    assert_eq!(r.status, RequestStatus::Pending);
    assert_eq!(r.title, "The Matrix");
    assert_eq!(r.requested_by.username, "alice");
    assert!(matches!(
        t.notes.take()[0].0,
        NotificationEvent::RequestPending { .. }
    ));

    // Requesters cannot approve; duplicates conflict.
    assert!(matches!(
        t.svc.approve_request(&t.user, r.id).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        t.svc.create_request(&t.user, movie_req(603)).await,
        Err(Error::Conflict(_))
    ));

    let mut s = t.svc.request_settings().await.unwrap();
    s.movie.profile_id = Some(1);
    s.movie.tags = vec![9];
    t.svc.set_request_settings(s).await.unwrap();
    let a = t.svc.approve_request(&t.admin, r.id).await.unwrap();
    assert_eq!(a.status, RequestStatus::Approved);
    assert_eq!(a.modified_by.as_ref().unwrap().username, "admin");
    let lib_id = a.movie_id.unwrap();
    {
        let st = t.lib.state();
        assert_eq!(st.added_movies[0].quality_profile_id, Some(1));
        assert_eq!(st.added_movies[0].tags, vec![9]);
        assert_eq!(st.added_movies[0].minimum_availability, "released");
        assert_eq!(st.searches, vec![(MediaKind::Movie, vec![lib_id])]);
    }
    let n = t.notes.take();
    assert!(matches!(n[0].0, NotificationEvent::RequestApproved { .. }));
    assert_eq!(n[0].1, vec![t.user.id], "requester opted in by default");

    // Status sync: processing, then available.
    t.lib.set(
        MediaKind::Movie,
        603,
        Availability {
            status: AvailabilityStatus::Processing,
            library_id: Some(lib_id),
            ..Default::default()
        },
    );
    assert_eq!(t.svc.sync_request_statuses().await.unwrap(), 1);
    assert_eq!(
        t.svc.get_request(&t.user, r.id).await.unwrap().status,
        RequestStatus::Processing
    );
    t.lib.set(
        MediaKind::Movie,
        603,
        Availability {
            status: AvailabilityStatus::Available,
            library_id: Some(lib_id),
            ..Default::default()
        },
    );
    assert_eq!(t.svc.sync_request_statuses().await.unwrap(), 1);
    let done = t.svc.get_request(&t.user, r.id).await.unwrap();
    assert_eq!(done.status, RequestStatus::Available);
    assert!(done.available_at.is_some());
    assert!(matches!(
        t.notes.take()[0].0,
        NotificationEvent::RequestAvailable { .. }
    ));
    assert_eq!(t.svc.sync_request_statuses().await.unwrap(), 0);

    // Already available.
    let mine = t.svc.create_request(&t.admin, movie_req(603)).await;
    assert!(matches!(mine, Err(Error::Conflict(_))), "{mine:?}");
}

#[tokio::test]
async fn visibility_counts_decline_delete() {
    let t = setup().await;
    let bob = user(&t.db, "bob", Permissions::ROLE_USER).await;
    let r1 = t.svc.create_request(&t.user, movie_req(603)).await.unwrap();
    let r2 = t.svc.create_request(&bob, movie_req(604)).await.unwrap();
    let mine = t
        .svc
        .list_requests(&t.user, &RequestListQuery::default())
        .await
        .unwrap();
    assert_eq!(mine.total, 1);
    assert_eq!(mine.items[0].id, r1.id);
    assert!(matches!(
        t.svc.get_request(&t.user, r2.id).await,
        Err(Error::NotFound(_))
    ));
    let all = t
        .svc
        .list_requests(&t.admin, &RequestListQuery::default())
        .await
        .unwrap();
    assert_eq!(all.total, 2);
    let q = RequestListQuery {
        status: Some("pending".into()),
        user_id: Some(bob.id),
        ..Default::default()
    };
    assert_eq!(
        t.svc.list_requests(&t.admin, &q).await.unwrap().items[0].id,
        r2.id
    );

    let d = t
        .svc
        .decline_request(&t.admin, r2.id, Some(" not now ".into()))
        .await
        .unwrap();
    assert_eq!(d.status, RequestStatus::Declined);
    assert_eq!(d.decline_reason.as_deref(), Some("not now"));
    let c = t.svc.request_counts(&t.admin).await.unwrap();
    assert_eq!((c.total, c.pending, c.declined, c.movie), (2, 1, 1, 2));
    assert_eq!(t.svc.request_counts(&t.user).await.unwrap().total, 1);

    // Edit own pending; delete others' → not found; delete own.
    assert!(matches!(
        t.svc.delete_request(&t.user, r2.id).await,
        Err(Error::NotFound(_))
    ));
    t.svc.delete_request(&t.user, r1.id).await.unwrap();
    assert_eq!(
        t.svc
            .list_requests(&t.user, &RequestListQuery::default())
            .await
            .unwrap()
            .total,
        0
    );
    // A declined movie can be requested again.
    t.svc.create_request(&bob, movie_req(604)).await.unwrap();
}

#[tokio::test]
async fn series_seasons_partial_and_dedupe() {
    let t = setup().await;
    let r = t
        .svc
        .create_request(&t.user, tv_req(Some(vec![1])))
        .await
        .unwrap();
    assert_eq!(r.seasons, vec![1]);
    assert!(matches!(
        t.svc.create_request(&t.user, tv_req(Some(vec![9]))).await,
        Err(Error::Validation(_))
    ));
    // Season 2 already available → only season 3 is requested.
    t.lib.set(
        MediaKind::Series,
        1399,
        Availability {
            status: AvailabilityStatus::PartiallyAvailable,
            library_id: Some(5),
            seasons: [(2, AvailabilityStatus::Available)].into(),
        },
    );
    let r2 = t.svc.create_request(&t.user, tv_req(None)).await.unwrap();
    assert_eq!(r2.seasons, vec![3]);
    assert!(matches!(
        t.svc.create_request(&t.user, tv_req(None)).await,
        Err(Error::Conflict(_))
    ));

    // Detail shows per-season state.
    let d = t
        .svc
        .media_detail(&t.user, MediaKind::Series, 1399)
        .await
        .unwrap();
    assert_eq!(d.seasons.len(), 3, "specials excluded");
    assert_eq!(d.seasons[0].request_status, Some(RequestStatus::Pending));
    assert_eq!(d.seasons[1].availability, AvailabilityStatus::Available);
    assert!(!d.request_ability.can_request);

    // Approving monitors the requested seasons; sync marks it available when they are.
    let a = t.svc.approve_request(&t.admin, r.id).await.unwrap();
    assert_eq!(a.series_id, Some(5));
    assert_eq!(t.lib.state().added_series[0].seasons, vec![1]);
    t.lib.set(
        MediaKind::Series,
        1399,
        Availability {
            status: AvailabilityStatus::PartiallyAvailable,
            library_id: Some(5),
            seasons: [
                (1, AvailabilityStatus::Available),
                (2, AvailabilityStatus::Available),
            ]
            .into(),
        },
    );
    t.svc.sync_request_statuses().await.unwrap();
    assert_eq!(
        t.svc.get_request(&t.admin, r.id).await.unwrap().status,
        RequestStatus::Available
    );

    // Partial requests disabled → whole series.
    let mut s = t.svc.request_settings().await.unwrap();
    s.partial_requests = false;
    t.svc.set_request_settings(s).await.unwrap();
    t.svc.delete_request(&t.admin, r2.id).await.unwrap();
    let whole = t
        .svc
        .create_request(&t.user, tv_req(Some(vec![3])))
        .await
        .unwrap();
    assert_eq!(whole.seasons, vec![3], "only not-yet-available seasons remain");
}

#[tokio::test]
async fn quotas_permissions_4k_auto_approve() {
    let t = setup().await;
    let mut s = t.svc.request_settings().await.unwrap();
    s.movie_quota_limit = Some(1);
    s.tv_quota_limit = Some(2);
    t.svc.set_request_settings(s.clone()).await.unwrap();
    t.svc.create_request(&t.user, movie_req(603)).await.unwrap();
    let e = t.svc.create_request(&t.user, movie_req(604)).await.unwrap_err();
    assert!(matches!(&e, Error::Forbidden(m) if m.contains("quota")), "{e:?}");
    let q = t.svc.quota(&t.user, t.user.id).await.unwrap();
    assert_eq!(
        (q.movie.used, q.movie.remaining, q.movie.restricted),
        (1, Some(0), true)
    );
    assert!(matches!(
        t.svc.quota(&t.user, t.admin.id).await,
        Err(Error::Forbidden(_))
    ));
    // TV counts seasons.
    assert!(matches!(
        t.svc.create_request(&t.user, tv_req(None)).await,
        Err(Error::Forbidden(_))
    ));
    t.svc
        .create_request(&t.user, tv_req(Some(vec![1, 2])))
        .await
        .unwrap();
    // Admin exempt and auto-approved.
    let a = t.svc.create_request(&t.admin, movie_req(604)).await.unwrap();
    assert_eq!(a.status, RequestStatus::Approved);

    // 4K: disabled, then enabled but no permission, then power user (auto-approve 4K).
    let mut m = movie_req(605);
    m.is_4k = true;
    assert!(matches!(
        t.svc.create_request(&t.admin, m.clone()).await,
        Err(Error::Validation(_))
    ));
    s.enable_4k_movie = true;
    s.movie_quota_limit = None;
    t.svc.set_request_settings(s).await.unwrap();
    assert!(matches!(
        t.svc.create_request(&t.user, m.clone()).await,
        Err(Error::Forbidden(_))
    ));
    let power = user(&t.db, "power", Permissions::ROLE_POWER_USER).await;
    let r = t.svc.create_request(&power, m).await.unwrap();
    assert!(r.is_4k);
    assert_eq!(r.status, RequestStatus::Approved);
    assert!(t.lib.state().added_movies.iter().any(|x| x.is_4k));

    // Advanced options need REQUEST_ADVANCED.
    let mut adv = movie_req(605);
    adv.root_folder_id = Some(1);
    assert!(matches!(
        t.svc.create_request(&t.user, adv).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        t.svc.request_options(&t.user, MediaKind::Movie, false).await,
        Err(Error::Forbidden(_))
    ));
    let o = t
        .svc
        .request_options(&power, MediaKind::Movie, false)
        .await
        .unwrap();
    assert!(!o.profiles.is_empty());
}

#[tokio::test]
async fn failed_approval_can_be_retried() {
    let t = setup().await;
    t.lib.state().fail_adds = Some("root folder missing".into());
    let r = t.svc.create_request(&t.user, movie_req(603)).await.unwrap();
    let f = t.svc.approve_request(&t.admin, r.id).await.unwrap();
    assert_eq!(f.status, RequestStatus::Failed);
    assert_eq!(f.failure_reason.as_deref(), Some("root folder missing"));
    t.lib.state().fail_adds = None;
    let ok = t.svc.retry_request(&t.admin, r.id).await.unwrap();
    assert_eq!(ok.status, RequestStatus::Approved);
    assert!(ok.failure_reason.is_none());
    assert!(matches!(
        t.svc.retry_request(&t.admin, r.id).await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn discovery_overlays_availability_and_requests() {
    let t = setup().await;
    t.lib.set(
        MediaKind::Movie,
        603,
        Availability {
            status: AvailabilityStatus::Available,
            library_id: Some(1),
            ..Default::default()
        },
    );
    t.svc
        .create_request(&t.user, tv_req(Some(vec![1])))
        .await
        .unwrap();
    let p = t.svc.discover(&t.user, "trending", 1).await.unwrap();
    assert_eq!(p.results.len(), 2);
    assert_eq!(p.results[0].availability, AvailabilityStatus::Available);
    assert_eq!(
        p.results[1].request.as_ref().unwrap().status,
        RequestStatus::Pending
    );
    // Other users don't see alice's request brief.
    let bob = user(&t.db, "bob", Permissions::ROLE_USER).await;
    assert!(
        t.svc.discover(&bob, "popular-movies", 1).await.unwrap().results[1]
            .request
            .is_none()
    );
    assert!(matches!(
        t.svc.discover(&bob, "bogus", 1).await,
        Err(Error::NotFound(_))
    ));
    let s = t.svc.search(&bob, "matrix", 1).await.unwrap();
    assert_eq!(s.results.len(), 1);
    let d = t.svc.media_detail(&bob, MediaKind::Movie, 603).await.unwrap();
    assert_eq!(d.availability, AvailabilityStatus::Available);
    assert!(!d.request_ability.can_request);
    assert_eq!(d.request_ability.reason.as_deref(), Some("already available"));
    let d = t.svc.media_detail(&bob, MediaKind::Movie, 604).await.unwrap();
    assert!(d.request_ability.can_request);
    assert!(matches!(
        t.svc.media_detail(&bob, MediaKind::Movie, 1).await,
        Err(Error::NotFound(_))
    ));
    // hide_available drops available titles from feeds.
    let mut st = t.svc.request_settings().await.unwrap();
    st.hide_available = true;
    t.svc.set_request_settings(st).await.unwrap();
    assert_eq!(
        t.svc.discover(&bob, "trending", 1).await.unwrap().results.len(),
        1
    );
}

#[tokio::test]
async fn issues_flow() {
    let t = setup().await;
    let i = t
        .svc
        .create_issue(
            &t.user,
            CreateIssueDto {
                kind: MediaKind::Series,
                tmdb_id: 1399,
                issue_type: IssueType::Audio,
                season: Some(1),
                episode: Some(2),
                message: "  out of sync ".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(i.message, "out of sync");
    assert_eq!(i.status, IssueStatus::Open);
    assert!(matches!(
        t.notes.take()[0].0,
        NotificationEvent::IssueReported { .. }
    ));
    let bob = user(&t.db, "bob", Permissions::ROLE_USER).await;
    assert!(matches!(
        t.svc.get_issue(&bob, i.id).await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        t.svc
            .list_issues(&bob, &IssueListQuery::default())
            .await
            .unwrap()
            .total,
        0
    );

    let c = t
        .svc
        .add_comment(
            &t.admin,
            i.id,
            CreateCommentDto {
                message: "looking".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(c.comments.len(), 1);
    assert_eq!(c.comments[0].user.username, "admin");
    let r = t
        .svc
        .set_issue_status(&t.admin, i.id, IssueStatus::Resolved)
        .await
        .unwrap();
    assert_eq!(r.resolved_by.unwrap().username, "admin");
    let n = t.notes.take();
    assert!(
        n.iter()
            .any(|(e, to)| matches!(e, NotificationEvent::IssueResolved { .. }) && to == &vec![t.user.id])
    );
    let c = t.svc.issue_counts(&t.admin).await.unwrap();
    assert_eq!((c.total, c.resolved, c.audio), (1, 1, 1));
    let o = t
        .svc
        .set_issue_status(&t.user, i.id, IssueStatus::Open)
        .await
        .unwrap();
    assert!(o.resolved_at.is_none());
    let list = t
        .svc
        .list_issues(
            &t.admin,
            &IssueListQuery {
                status: Some("open".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(list.items[0].comment_count, 1);
    // Reporter can't delete once commented; manager can.
    assert!(matches!(
        t.svc.delete_issue(&t.user, i.id).await,
        Err(Error::Forbidden(_))
    ));
    t.svc.delete_issue(&t.admin, i.id).await.unwrap();
    // Movies have no seasons; empty messages rejected.
    let bad = CreateIssueDto {
        kind: MediaKind::Movie,
        tmdb_id: 603,
        issue_type: IssueType::Video,
        season: Some(1),
        episode: None,
        message: "x".into(),
    };
    assert!(matches!(
        t.svc.create_issue(&t.user, bad).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn user_management_rules() {
    let t = setup().await;
    let manager = user(&t.db, "manager", Permissions::ROLE_MANAGER).await;
    assert!(matches!(
        t.svc.list_users(&t.user).await,
        Err(Error::Forbidden(_))
    ));
    let created = t
        .svc
        .create_user(
            &manager,
            CreateUserDto {
                username: "carol".into(),
                password: "unused-here".into(),
                email: None,
                display_name: Some("Carol".into()),
                permissions: None,
                movie_quota_limit: Some(3),
                movie_quota_days: None,
                tv_quota_limit: None,
                tv_quota_days: None,
                book_quota_limit: Some(2),
                book_quota_days: None,
                music_quota_limit: None,
                music_quota_days: None,
            },
            "hash".into(),
        )
        .await
        .unwrap();
    assert_eq!(created.permissions, Permissions::ROLE_USER);
    assert_eq!(created.movie_quota_limit, Some(3));
    assert_eq!(created.book_quota_limit, Some(2));
    assert_eq!(created.display_name, "Carol");
    let mut dup = CreateUserDto {
        username: "CAROL".into(),
        password: String::new(),
        email: None,
        display_name: None,
        permissions: Some(Permissions::ADMIN),
        movie_quota_limit: None,
        movie_quota_days: None,
        tv_quota_limit: None,
        tv_quota_days: None,
        book_quota_limit: None,
        book_quota_days: None,
        music_quota_limit: None,
        music_quota_days: None,
    };
    assert!(matches!(
        t.svc.create_user(&manager, dup.clone(), "h".into()).await,
        Err(Error::Forbidden(_))
    ));
    dup.permissions = None;
    assert!(matches!(
        t.svc.create_user(&manager, dup, "h".into()).await,
        Err(Error::Conflict(_))
    ));

    let upd = |p| UpdateUserDto {
        display_name: None,
        email: None,
        avatar: None,
        locale: None,
        permissions: p,
        movie_quota_limit: None,
        movie_quota_days: None,
        tv_quota_limit: None,
        tv_quota_days: None,
        book_quota_limit: None,
        book_quota_days: None,
        music_quota_limit: None,
        music_quota_days: None,
        password: None,
    };
    assert!(matches!(
        t.svc
            .update_user(&manager, t.admin.id, upd(Permissions::ROLE_USER), None)
            .await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        t.svc
            .update_user(&t.admin, t.admin.id, upd(Permissions::ROLE_USER), None)
            .await,
        Err(Error::Conflict(_))
    ));
    // Self-edit by a plain user keeps permissions.
    let me = t
        .svc
        .update_user(&t.user, t.user.id, upd(Permissions::ADMIN), None)
        .await
        .unwrap();
    assert_eq!(me.permissions, Permissions::ROLE_USER);
    assert!(matches!(
        t.svc.delete_user(&t.admin, t.admin.id).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        t.svc.delete_user(&manager, t.admin.id).await,
        Err(Error::Forbidden(_))
    ));
    t.svc.delete_user(&manager, created.id).await.unwrap();
    assert_eq!(t.svc.list_users(&t.admin).await.unwrap().len(), 3);
    assert_eq!(reelix_requests::users::roles().roles.len(), 4);
}

#[tokio::test]
async fn jellyfin_login_and_import() {
    let mut jf_user = FakeMediaServer::user("jf-1", "dave");
    jf_user.is_admin = true;
    let dir = FakeDirectory::new("http://127.0.0.1:9").with(
        7,
        "Home Jellyfin",
        FakeMediaServer {
            kind: MediaServerKind::Jellyfin,
            users: vec![
                (jf_user, Some("pw".into())),
                (FakeMediaServer::user("jf-2", "alice"), Some("pw2".into())),
            ],
        },
    );
    let t = setup_with(dir).await;
    assert!(matches!(
        t.svc
            .media_server_login(MediaServerKind::Jellyfin, "dave", "pw")
            .await,
        Err(Error::Forbidden(_))
    ));
    let mut sec = t.svc.security_settings().await.unwrap();
    sec.jellyfin_login = true;
    t.svc.set_security_settings(sec.clone()).await.unwrap();
    assert!(t.svc.auth_methods().await.unwrap().jellyfin);
    assert!(matches!(
        t.svc
            .media_server_login(MediaServerKind::Jellyfin, "dave", "nope")
            .await,
        Err(Error::Unauthorized(_))
    ));
    let u = t
        .svc
        .media_server_login(MediaServerKind::Jellyfin, "dave", "pw")
        .await
        .unwrap();
    assert_eq!(u.auth_source, AuthSource::Jellyfin);
    assert_eq!(u.permissions, Permissions::ROLE_USER);
    let again = t
        .svc
        .media_server_login(MediaServerKind::Jellyfin, "DAVE", "pw")
        .await
        .unwrap();
    assert_eq!(again.id, u.id);
    // Username collision with local "alice" gets a suffix.
    let a2 = t
        .svc
        .media_server_login(MediaServerKind::Jellyfin, "alice", "pw2")
        .await
        .unwrap();
    assert_eq!(a2.username, "alice-2");

    let preview = t.svc.import_preview(&t.admin, 7).await.unwrap();
    assert!(preview.iter().all(|p| p.existing_user_id.is_some()));
    let res = t
        .svc
        .import_users(&t.admin, 7, ImportUsersDto::default())
        .await
        .unwrap();
    assert_eq!((res.imported.len(), res.skipped), (0, 2));
    assert_eq!(t.svc.import_servers(&t.admin).await.unwrap()[0].kind, "jellyfin");

    // No auto-create: unknown users are rejected.
    sec.media_server_auto_create = false;
    t.svc.set_security_settings(sec).await.unwrap();
    t.svc.delete_user(&t.admin, u.id).await.unwrap();
    assert!(matches!(
        t.svc
            .media_server_login(MediaServerKind::Jellyfin, "dave", "pw")
            .await,
        Err(Error::Unauthorized(_))
    ));
    let res = t
        .svc
        .import_users(
            &t.admin,
            7,
            ImportUsersDto {
                external_ids: Some(vec!["jf-1".into()]),
                permissions: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(res.imported[0].external_id.as_deref(), Some("jf-1"));
    t.svc
        .media_server_login(MediaServerKind::Jellyfin, "dave", "pw")
        .await
        .unwrap();
}

#[tokio::test]
async fn notification_prefs_roundtrip() {
    let t = setup().await;
    let p = t.svc.notification_prefs(t.user.id).await.unwrap();
    assert!(p.events.contains(&"request_available".to_string()));
    let saved = t
        .svc
        .set_notification_prefs(
            t.user.id,
            NotificationPrefsDto {
                events: vec!["request_declined".into(), "bogus".into()],
                email: Some("  ".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.events, vec!["request_declined"]);
    assert!(saved.email.is_none());
    let bad = NotificationPrefsDto {
        watchlist_sync_movies: true,
        ..Default::default()
    };
    assert!(matches!(
        t.svc.set_notification_prefs(t.user.id, bad).await,
        Err(Error::Validation(_))
    ));
    // Opted out of approvals → no recipients.
    let r = t.svc.create_request(&t.user, movie_req(603)).await.unwrap();
    t.notes.take();
    t.svc.approve_request(&t.admin, r.id).await.unwrap();
    assert!(t.notes.take()[0].1.is_empty());
}

#[test]
fn settings_dtos_default_from_empty_json() {
    let s: RequestSettingsDto = serde_json::from_str("{}").unwrap();
    assert_eq!(s, RequestSettingsDto::default());
    let sec: SecuritySettingsDto = serde_json::from_str(r#"{"plex_login":true}"#).unwrap();
    assert!(sec.plex_login && sec.local_login);
    for st in RequestStatus::ALL {
        assert_eq!(RequestStatus::parse(st.as_str()), Some(st));
        assert_eq!(
            serde_json::to_string(&st).unwrap(),
            format!("\"{}\"", st.as_str())
        );
    }
}

#[tokio::test]
async fn security_settings_mask_secret() {
    let t = setup().await;
    let mut s = SecuritySettingsDto::default();
    s.oidc.enabled = true;
    s.oidc.issuer_url = "https://id.example.com/".into();
    s.oidc.client_id = "reelix".into();
    s.oidc.client_secret = "s3cret".into();
    let saved = t.svc.set_security_settings(s.clone()).await.unwrap();
    assert_eq!(saved.oidc.client_secret, "********");
    assert_eq!(saved.oidc.issuer_url, "https://id.example.com");
    let raw: serde_json::Value = t.db.settings().get("auth.security").await.unwrap().unwrap();
    assert_ne!(raw["oidc"]["client_secret"], "s3cret", "stored encrypted");
    s.oidc.client_secret = "********".into();
    t.svc.set_security_settings(s.clone()).await.unwrap();
    assert_eq!(
        t.svc.security_settings().await.unwrap().oidc.client_secret,
        "s3cret"
    );
    let m = t.svc.auth_methods().await.unwrap();
    assert!(m.oidc);
    assert_eq!(m.oidc_label.as_deref(), Some("Single sign-on"));
    s.oidc.username_claim = "groups".into();
    assert!(matches!(
        t.svc.set_security_settings(s).await,
        Err(Error::Validation(_))
    ));
}
