//! Radio on Discover: rows, search and the show view with follow / request state, and radio
//! show requests (permissions, duplicates, quota, approval following the show) against an
//! in-memory DB and the in-memory fakes.

use std::sync::Arc;

use reelix_api_types::requests::*;
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, ContentKind, Permissions, User};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_notify::NotificationEvent;
use reelix_requests::fakes::{self, FakeDirectory, FakeLibrary, FakeMetadata, FakeNotifications};
use reelix_requests::{Availability, Error, RequestDeps, RequestService};

/// An Apple chart entry (followed as a feed once approved).
const ARCHERS: &str = "apple:1234567";
/// A BBC programme.
const JAM: &str = "bbc:b006s5dp";

struct T {
    svc: RequestService,
    lib: FakeLibrary,
    notes: FakeNotifications,
    admin: User,
    /// ROLE_USER + REQUEST_RADIO (no auto-approve, no MANAGE_LIBRARY).
    user: User,
    /// ROLE_USER only.
    plain: User,
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
        .with_radio(
            Some("apple-top"),
            fakes::radio_show("apple", "1234567", "The Archers"),
        )
        .with_radio(
            Some("apple-top"),
            fakes::radio_show("bbc", "b006s5dp", "Just a Minute"),
        );
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
    let u = user(&db, "alice", Permissions::ROLE_USER | Permissions::REQUEST_RADIO).await;
    let plain = user(&db, "bob", Permissions::ROLE_USER).await;
    T {
        svc,
        lib,
        notes,
        admin,
        user: u,
        plain,
    }
}

fn radio_req(id: &str) -> CreateContentRequestDto {
    CreateContentRequestDto {
        kind: ContentRequestKind::Radio,
        foreign_id: id.into(),
        formats: vec![],
        profile_id: None,
        root_folder_id: None,
        tags: vec![],
        user_id: None,
    }
}

fn followed(id: i64) -> Availability {
    Availability {
        status: AvailabilityStatus::Available,
        library_id: Some(id),
        ..Availability::default()
    }
}

#[test]
fn radio_foreign_ids() {
    assert_eq!(radio_foreign_id("rss", "https://x/feed"), "rss:https://x/feed");
    assert_eq!(
        split_radio_foreign_id("rss:https://x/feed"),
        Some(("rss", "https://x/feed"))
    );
    assert_eq!(split_radio_foreign_id("bbc:b006s5dp"), Some(("bbc", "b006s5dp")));
    assert_eq!(split_radio_foreign_id("b006s5dp"), None);
    assert_eq!(split_radio_foreign_id(":x"), None);
    assert_eq!(split_radio_foreign_id("bbc: "), None);
    assert_eq!(split_radio_foreign_id("a/b:x"), None);
}

#[tokio::test]
async fn requesters_browse_radio_and_followed_shows_are_marked() {
    let t = setup().await;
    let lists = t.svc.radio_lists().await.unwrap();
    assert_eq!(lists.rows.len(), 1);
    assert_eq!(lists.rows[0].id, "apple-top");

    let row = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Radio, "apple-top")
        .await
        .unwrap();
    let ids: Vec<&str> = row.results.iter().map(|i| i.foreign_id.as_str()).collect();
    assert_eq!(ids, [ARCHERS, JAM]);
    assert_eq!(row.results[0].kind, ContentRequestKind::Radio);
    assert_eq!(row.results[0].subtitle.as_deref(), Some("Radio 4"));
    assert!(
        row.results
            .iter()
            .all(|i| i.availability == AvailabilityStatus::Unknown)
    );
    assert!(matches!(
        t.svc
            .discover_content(&t.user, ContentRequestKind::Radio, "bbc-nope")
            .await,
        Err(Error::NotFound(_))
    ));

    // Followed shows are "available" with the show id, in rows, search and the show view.
    t.lib.set_content(ContentRequestKind::Radio, JAM, followed(41));
    let row = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Radio, "apple-top")
        .await
        .unwrap();
    assert_eq!(row.results[1].availability, AvailabilityStatus::Available);
    assert_eq!(row.results[1].library_id, Some(41));
    let found = t.svc.search_radio(&t.user, "minute").await.unwrap();
    assert_eq!(found.results.len(), 1);
    assert_eq!(found.results[0].library_id, Some(41));
    assert!(
        t.svc
            .search_radio(&t.user, "  ")
            .await
            .unwrap()
            .results
            .is_empty()
    );

    let d = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Radio, JAM)
        .await
        .unwrap();
    assert_eq!(d.item.availability, AvailabilityStatus::Available);
    assert!(!d.request_ability.can_request);
    assert_eq!(d.request_ability.reason.as_deref(), Some("Already followed"));
    let preview = d.radio.unwrap();
    assert_eq!(preview.latest_episodes.len(), 1);
    assert_eq!(d.overview.as_deref(), Some("Just a Minute synopsis"));
    assert!(matches!(
        t.svc.create_content_request(&t.user, radio_req(JAM)).await,
        Err(Error::Conflict(_))
    ));

    // A hide-available setting drops followed shows from rows.
    let mut s = t.svc.request_settings().await.unwrap();
    s.hide_available = true;
    t.svc.set_request_settings(s).await.unwrap();
    let row = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Radio, "apple-top")
        .await
        .unwrap();
    assert_eq!(row.results.len(), 1);

    // Unknown shows and malformed ids are 404s.
    assert!(matches!(
        t.svc
            .content_detail(&t.user, ContentRequestKind::Radio, "bbc:p000none")
            .await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        t.svc
            .content_detail(&t.user, ContentRequestKind::Radio, "no-provider")
            .await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn radio_request_pending_approve_follows_with_defaults() {
    let t = setup().await;
    // Without REQUEST_RADIO: visible, not requestable.
    let d = t
        .svc
        .content_detail(&t.plain, ContentRequestKind::Radio, ARCHERS)
        .await
        .unwrap();
    assert!(!d.request_ability.can_request);
    assert_eq!(
        d.request_ability.reason.as_deref(),
        Some("You are not allowed to request radio shows")
    );
    assert!(matches!(
        t.svc.create_content_request(&t.plain, radio_req(ARCHERS)).await,
        Err(Error::Forbidden(_))
    ));

    let d = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Radio, ARCHERS)
        .await
        .unwrap();
    assert!(d.request_ability.can_request);
    assert!(d.request_ability.formats.is_empty());

    let r = t
        .svc
        .create_content_request(&t.user, radio_req(ARCHERS))
        .await
        .unwrap();
    assert_eq!(r.status, RequestStatus::Pending);
    assert_eq!(r.kind, ContentRequestKind::Radio);
    assert_eq!(r.foreign_id, ARCHERS);
    assert_eq!(r.title, "The Archers");
    assert_eq!(r.subtitle.as_deref(), Some("Radio 4"));
    assert!(
        t.lib.state().added_radio.is_empty(),
        "nothing followed before approval"
    );
    match &t.notes.take()[0].0 {
        NotificationEvent::RequestPending { media, .. } => {
            assert_eq!(media.content, Some(ContentKind::Radio));
            assert_eq!(media.title, "The Archers");
        }
        other => panic!("unexpected {other:?}"),
    }
    // Duplicates and approving one's own request are refused.
    assert!(matches!(
        t.svc.create_content_request(&t.user, radio_req(ARCHERS)).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        t.svc.approve_content_request(&t.user, r.id).await,
        Err(Error::Forbidden(_))
    ));
    let d = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Radio, ARCHERS)
        .await
        .unwrap();
    assert_eq!(d.request_ability.reason.as_deref(), Some("Already requested"));
    assert_eq!(d.requests.len(), 1);
    let row = t
        .svc
        .discover_content(&t.user, ContentRequestKind::Radio, "apple-top")
        .await
        .unwrap();
    assert_eq!(
        row.results[0].request.as_ref().map(|b| b.status),
        Some(RequestStatus::Pending)
    );
    let media = t
        .svc
        .list_content_requests(
            &t.admin,
            &RequestListQuery {
                kind: Some("radio".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(media.total, 1);
    let counts = t.svc.request_counts(&t.admin).await.unwrap();
    assert_eq!(
        (counts.total, counts.pending, counts.radio, counts.music),
        (1, 1, 1, 0)
    );

    // Approval follows the show with the radio request defaults (the follow choice is the
    // radio settings' default), then the request is available.
    let mut s = t.svc.request_settings().await.unwrap();
    s.radio = RequestDefaultsDto {
        profile_id: Some(12),
        root_folder_id: Some(5),
        tags: vec![3],
    };
    t.svc.set_request_settings(s).await.unwrap();
    let a = t.svc.approve_content_request(&t.admin, r.id).await.unwrap();
    assert_eq!(a.status, RequestStatus::Available, "a followed show is available");
    let show_id = a.radio_show_id.unwrap();
    {
        let st = t.lib.state();
        let added = &st.added_radio[0];
        assert_eq!(
            (added.provider.as_str(), added.external_id.as_str()),
            ("apple", "1234567")
        );
        assert_eq!(added.quality_profile_id, Some(12));
        assert_eq!(added.root_folder_id, Some(5));
        assert_eq!(added.tags, vec![3]);
        assert_eq!(added.request_id, r.id);
        // search_on_approve (default on): the show's wanted episodes.
        assert_eq!(
            st.content_searches,
            vec![(ContentRequestKind::Radio, vec![show_id], vec![])]
        );
    }
    let kinds: Vec<_> = t.notes.take().into_iter().map(|(e, _)| e).collect();
    assert!(
        kinds
            .iter()
            .any(|e| matches!(e, NotificationEvent::RequestApproved { .. }))
    );
    assert!(
        kinds
            .iter()
            .any(|e| matches!(e, NotificationEvent::RequestAvailable { .. }))
    );
    let d = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Radio, ARCHERS)
        .await
        .unwrap();
    assert_eq!(d.item.library_id, Some(show_id));
    assert_eq!(d.request_ability.reason.as_deref(), Some("Already followed"));

    // Radio requests have no formats.
    assert!(matches!(
        t.svc
            .update_content_request(
                &t.admin,
                r.id,
                UpdateContentRequestDto {
                    formats: Some(vec![reelix_core::BookFormat::Ebook]),
                    ..Default::default()
                },
            )
            .await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn radio_auto_approve_quota_and_failed_follow() {
    let t = setup().await;
    let mut s = t.svc.request_settings().await.unwrap();
    s.radio_quota_limit = Some(1);
    s.radio_quota_days = 7;
    t.svc.set_request_settings(s).await.unwrap();
    t.svc
        .create_content_request(&t.user, radio_req(ARCHERS))
        .await
        .unwrap();
    let q = t.svc.quota(&t.user, t.user.id).await.unwrap();
    assert_eq!(
        (q.radio.used, q.radio.limit, q.radio.restricted),
        (1, Some(1), true)
    );
    let d = t
        .svc
        .content_detail(&t.user, ContentRequestKind::Radio, JAM)
        .await
        .unwrap();
    assert!(
        d.request_ability
            .reason
            .unwrap()
            .starts_with("Request quota reached")
    );
    assert!(matches!(
        t.svc.create_content_request(&t.user, radio_req(JAM)).await,
        Err(Error::Forbidden(_))
    ));

    // AUTO_APPROVE_RADIO follows right away; a failed follow can be retried.
    let db = t.svc.db().clone();
    let auto = user(
        &db,
        "carol",
        Permissions::ROLE_USER | Permissions::REQUEST_RADIO | Permissions::AUTO_APPROVE_RADIO,
    )
    .await;
    t.lib.state().fail_adds = Some("the source is down".into());
    let r = t.svc.create_content_request(&auto, radio_req(JAM)).await.unwrap();
    assert_eq!(r.status, RequestStatus::Failed);
    assert_eq!(r.failure_reason.as_deref(), Some("the source is down"));
    t.lib.state().fail_adds = None;
    let r = t.svc.retry_content_request(&t.admin, r.id).await.unwrap();
    assert_eq!(r.status, RequestStatus::Available);
    assert!(r.radio_show_id.is_some());
}
