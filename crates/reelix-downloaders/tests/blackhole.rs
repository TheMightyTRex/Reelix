//! Torrent and Usenet blackhole against temporary folders.

mod common;

use std::time::{Duration, SystemTime};

use common::{build, fixture};
use reelix_core::Protocol;
use reelix_downloaders::torrent::sample_torrent;
use reelix_downloaders::*;
use serde_json::json;

struct Dirs {
    _tmp: tempfile::TempDir,
    drop: std::path::PathBuf,
    watch: std::path::PathBuf,
}

fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let drop = tmp.path().join("drop");
    let watch = tmp.path().join("watch");
    std::fs::create_dir_all(&drop).unwrap();
    std::fs::create_dir_all(&watch).unwrap();
    Dirs {
        _tmp: tmp,
        drop,
        watch,
    }
}

fn settings(d: &Dirs, extra: serde_json::Value) -> serde_json::Value {
    let mut s = json!({ "drop_folder": d.drop, "watch_folder": d.watch });
    s.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    s
}

fn backdate(path: &std::path::Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

#[tokio::test]
async fn torrent_blackhole_writes_files_and_magnets() {
    let d = dirs();
    let c = build(
        "torrent_blackhole",
        settings(&d, json!({ "save_magnet_files": true })),
    );
    assert_eq!(c.protocol(), Protocol::Torrent);
    c.test().await.unwrap();
    let opts = AddOptions {
        title: "Oppenheimer.2023.1080p.BluRay.x264-SPARKS".into(),
        ..Default::default()
    };
    let id = c
        .add(
            &ReleasePayload::TorrentFile {
                bytes: sample_torrent("o.mkv"),
                name: "x.torrent".into(),
            },
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "Oppenheimer.2023.1080p.BluRay.x264-SPARKS");
    assert_eq!(
        std::fs::read(d.drop.join("Oppenheimer.2023.1080p.BluRay.x264-SPARKS.torrent")).unwrap(),
        sample_torrent("o.mkv")
    );
    let m = "magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A";
    let opts = AddOptions {
        title: "Evil/../Name".into(),
        ..Default::default()
    };
    c.add(&ReleasePayload::Magnet(m.into()), &opts).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(d.drop.join("Evil_.._Name.magnet")).unwrap(),
        m
    );
    // No leftovers from the temp-then-rename write.
    assert_eq!(std::fs::read_dir(&d.drop).unwrap().count(), 2);

    let strict = build("torrent_blackhole", settings(&d, json!({})));
    assert!(matches!(
        strict
            .add(&ReleasePayload::Magnet(m.into()), &AddOptions::default())
            .await,
        Err(ClientError::InvalidPayload(_))
    ));
}

#[tokio::test]
async fn usenet_blackhole_validates_nzb() {
    let d = dirs();
    let c = build("usenet_blackhole", settings(&d, json!({})));
    assert_eq!(c.protocol(), Protocol::Usenet);
    let opts = AddOptions {
        title: "Arrival.2016.1080p".into(),
        ..Default::default()
    };
    let nzb = fixture("sabnzbd/release.nzb").into_bytes();
    c.add(
        &ReleasePayload::NzbFile {
            bytes: nzb,
            name: "a.nzb".into(),
        },
        &opts,
    )
    .await
    .unwrap();
    assert!(d.drop.join("Arrival.2016.1080p.nzb").is_file());
    let bad = ReleasePayload::NzbFile {
        bytes: b"not xml".to_vec(),
        name: "b.nzb".into(),
    };
    assert!(c.add(&bad, &opts).await.is_err());
    let wrong = ReleasePayload::Magnet("magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A".into());
    assert!(c.add(&wrong, &opts).await.is_err());
}

#[tokio::test]
async fn watch_folder_items_settle_and_remove() {
    let d = dirs();
    // A finished folder (old), a file still being written (new), and hidden junk.
    let done = d.watch.join("Past.Lives.2023.1080p.WEB-DL");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(done.join("past.lives.mkv"), vec![0u8; 2048]).unwrap();
    backdate(&done.join("past.lives.mkv"), 600);
    let dir = std::fs::File::open(&done).unwrap();
    let _ = dir.set_modified(SystemTime::now() - Duration::from_secs(600));
    std::fs::write(d.watch.join("Aftersun.2022.720p.mkv"), vec![0u8; 100]).unwrap();
    std::fs::write(d.watch.join(".DS_Store"), b"x").unwrap();

    let c = build("torrent_blackhole", settings(&d, json!({ "settle_seconds": 60 })));
    let items = c.items().await.unwrap();
    assert_eq!(items.len(), 2);
    let aftersun = &items[0];
    assert_eq!(aftersun.download_id.as_str(), "Aftersun.2022.720p.mkv");
    assert_eq!(aftersun.title, "Aftersun.2022.720p");
    assert_eq!(aftersun.state, DownloadState::Downloading);
    let past = &items[1];
    assert_eq!(past.state, DownloadState::Completed);
    assert_eq!(past.total_bytes, 2048);
    assert!(past.can_be_removed);
    assert!(!past.can_move_files); // torrent blackhole defaults to read-only
    assert_eq!(past.output_path.as_deref(), Some(done.to_string_lossy().as_ref()));

    // Path traversal ids are rejected; real ids are deleted with their data.
    assert!(c.remove(&"../drop".into(), true).await.is_err());
    c.remove(&past.download_id, true).await.unwrap();
    assert!(!done.exists());
    c.remove(&"already-gone".into(), true).await.unwrap();

    let usenet = build("usenet_blackhole", settings(&d, json!({ "settle_seconds": 0 })));
    let items = usenet.items().await.unwrap();
    assert!(items[0].can_move_files && items[0].state == DownloadState::Completed);
}

#[tokio::test]
async fn missing_folders_fail_test_and_raise_health() {
    let d = dirs();
    let s = json!({ "drop_folder": d.drop.join("nope"), "watch_folder": d.watch });
    let c = build("usenet_blackhole", s);
    assert!(matches!(c.test().await, Err(ClientError::InvalidSettings(_))));
    let st = c.status().await.unwrap();
    assert_eq!(st.warnings[0].kind, HealthKind::Misconfigured);
    assert!(!c.test_report().await.ok());
    // Folders are required settings.
    assert!(reelix_downloaders::build("usenet_blackhole", &json!({}), &BuildContext::default()).is_err());
}

#[tokio::test]
async fn book_watch_folder_items_carry_the_book_category() {
    let d = dirs();
    let books = d.watch.parent().unwrap().join("books");
    std::fs::create_dir_all(&books).unwrap();
    let book = books.join("Dune.1965.EPUB");
    std::fs::create_dir_all(&book).unwrap();
    std::fs::write(book.join("dune.epub"), b"epub").unwrap();
    backdate(&book.join("dune.epub"), 600);
    std::fs::write(d.watch.join("Movie.2023.mkv"), b"x").unwrap();
    let c = build(
        "usenet_blackhole",
        settings(&d, json!({ "book_folder": books, "settle_seconds": 1 })),
    );
    c.test().await.unwrap();
    let items = c.items().await.unwrap();
    assert_eq!(items.len(), 2);
    let b = items
        .iter()
        .find(|i| i.download_id.as_str() == "Dune.1965.EPUB")
        .unwrap();
    assert_eq!(b.category.as_deref(), Some(blackhole::BOOK_CATEGORY));
    let m = items
        .iter()
        .find(|i| i.download_id.as_str() == "Movie.2023.mkv")
        .unwrap();
    assert_eq!(m.category, None);
    // Removal finds entries in the extra folder.
    c.remove(&b.download_id, true).await.unwrap();
    assert!(!book.exists());
    let status = c.status().await.unwrap();
    assert_eq!(status.output_dirs.len(), 2);
}
