use reelix_core::Protocol;
use reelix_db::Db;
use reelix_parser::{Quality, QualityModel};
use reelix_pipeline::GrabbedRelease;
use reelix_pipeline::store::{blocklist, history, pending, tracked};

fn release(title: &str) -> GrabbedRelease {
    GrabbedRelease {
        guid: "g1".into(),
        title: title.into(),
        indexer_id: Some(1),
        indexer: "Idx".into(),
        protocol: Protocol::Usenet,
        size: Some(1 << 30),
        publish_date: None,
        download_url: Some("http://x/nzb".into()),
        magnet_url: None,
        info_url: None,
        info_hash: None,
        seeders: None,
        quality: QualityModel::new(Quality::WebDl1080p),
        languages: vec![],
        release_group: Some("GRP".into()),
        custom_formats: vec![],
        format_score: 0,
        indexer_flags: vec![],
        seed_ratio: None,
        seed_time_minutes: None,
    }
}

#[tokio::test]
async fn store_roundtrips() {
    let db = Db::in_memory().await.unwrap();

    let mut h = history::NewHistory::new(history::HistoryEvent::Grabbed, "Movie.2024.1080p.WEB-DL-GRP");
    h.movie_id = Some(1);
    h.download_id = Some("SABnzbd_nzo_1".into());
    h.quality = Some(QualityModel::new(Quality::WebDl1080p));
    history::insert(&db, &h).await.unwrap();
    let (rows, total) = history::list(
        &db,
        &history::HistoryFilter {
            movie_id: Some(1),
            ..Default::default()
        },
        0,
        10,
    )
    .await
    .unwrap();
    assert_eq!((rows.len(), total), (1, 1));
    assert_eq!(
        history::grabs_for_download(&db, "SABnzbd_nzo_1")
            .await
            .unwrap()
            .len(),
        1
    );

    blocklist::insert(
        &db,
        &blocklist::NewBlocklist {
            movie_id: Some(1),
            source_title: "Bad.Release".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let entries = blocklist::for_media(&db, Some(1), None).await.unwrap();
    assert!(blocklist::is_blocked(&entries, "bad.release", None));
    assert!(!blocklist::is_blocked(&entries, "Good.Release", None));

    let new = tracked::NewTracked {
        download_client_id: 3,
        download_id: "SABnzbd_nzo_1".into(),
        protocol: Protocol::Usenet,
        title: "Movie.2024.1080p.WEB-DL-GRP".into(),
        movie_id: Some(1),
        series_id: None,
        episode_ids: vec![],
        release: release("Movie.2024.1080p.WEB-DL-GRP"),
    };
    let id = tracked::insert(&db, &new).await.unwrap();
    assert_eq!(
        tracked::insert(&db, &new).await.unwrap(),
        id,
        "re-grab replaces the row"
    );
    tracked::set_state(&db, id, tracked::TrackedState::ImportPending, &["waiting".into()])
        .await
        .unwrap();
    let t = tracked::by_download(&db, 3, "SABnzbd_nzo_1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, tracked::TrackedState::ImportPending);
    assert_eq!(t.release.unwrap().quality.quality, Quality::WebDl1080p);
    assert_eq!(tracked::active(&db).await.unwrap().len(), 1);

    pending::insert(&db, Some(1), None, &[], &release("Later"), "delay", 0)
        .await
        .unwrap();
    assert_eq!(pending::due(&db, 1).await.unwrap().len(), 1);
    assert_eq!(pending::delete_for_media(&db, Some(1), None).await.unwrap(), 1);
}
