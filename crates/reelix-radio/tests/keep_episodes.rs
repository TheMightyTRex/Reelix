//! Keeping only the newest N episodes of a show (RFX-106).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use reelix_api_types::radio::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_import::content::ContentDownload;
use reelix_import::files::TransferMode;
use reelix_radio::*;

fn day(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
}

struct Fake;

fn ep(id: &str, number: u32, title: &str, at: DateTime<Utc>) -> EpisodeData {
    EpisodeData {
        external_id: id.into(),
        series_external_id: "s1".into(),
        number: Some(number),
        title: title.into(),
        first_broadcast: Some(at),
        duration_secs: Some(30 * 60),
        ..EpisodeData::default()
    }
}

fn data() -> ShowData {
    ShowData {
        external_id: "pod1".into(),
        title: "Weekly Pod".into(),
        series: vec![SeriesData {
            external_id: "s1".into(),
            number: Some(1),
            ..SeriesData::default()
        }],
        episodes: vec![
            ep("e1", 1, "One", day(2024, 1, 1)),
            ep("e2", 2, "Two", day(2024, 1, 8)),
            ep("e3", 3, "Three", day(2024, 1, 15)),
            ep("e4", 4, "Four", day(2024, 1, 22)),
        ],
        complete: true,
        ..ShowData::default()
    }
}

#[async_trait::async_trait]
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
    async fn preview(&self, _link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(None)
    }
    async fn show(&self, _id: &str, _f: &ShowFetch) -> Result<ShowData, ProviderError> {
        Ok(data())
    }
}

struct Ctx {
    svc: RadioService,
    show: RadioShowDto,
    library: tempfile::TempDir,
    downloads: tempfile::TempDir,
}

async fn ctx() -> Ctx {
    let svc = RadioService::new(RadioDeps {
        db: Db::in_memory().await.unwrap(),
        events: EventBus::default(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(Arc::new(Fake)),
        library_sync: None,
    });
    svc.set_now(day(2024, 4, 1));
    let show = svc
        .add_show(AddRadioShowRequest {
            provider: Some("fake".into()),
            external_id: Some("pod1".into()),
            follow: Some(RadioFollow::All),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    let library = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    svc.set_settings(RadioSettingsDto {
        library_folder: Some(library.path().to_string_lossy().into_owned()),
        ..RadioSettingsDto::default()
    })
    .await
    .unwrap();
    Ctx {
        svc,
        show,
        library,
        downloads,
    }
}

impl Ctx {
    async fn episodes(&self) -> Vec<RadioEpisodeDto> {
        self.svc.show_episodes(self.show.id).await.unwrap().episodes
    }

    async fn episode(&self, ext: &str) -> RadioEpisodeDto {
        self.episodes()
            .await
            .into_iter()
            .find(|e| e.external_id == ext)
            .unwrap()
    }

    /// Files a tiny download as episode `ext`; returns where it went.
    async fn file(&self, ext: &str) -> PathBuf {
        let dir = self.downloads.path().join(ext);
        std::fs::create_dir_all(&dir).unwrap();
        tiny_flac(&dir.join("audio.flac"));
        let id = self.episode(ext).await.id;
        let d = ContentDownload {
            source: dir,
            release_title: Some(format!("Weekly Pod {ext}")),
            mode: TransferMode::Copy,
            download_client: None,
            download_id: None,
        };
        match self.svc.file_download(&d, id).await.unwrap() {
            FilingOutcome::Filed(f) => f.destination,
            FilingOutcome::Rejected(m) => panic!("rejected: {m:?}"),
        }
    }

    async fn keep(&self, n: Option<i64>) -> reelix_radio::Result<RadioShowDto> {
        self.svc
            .update_show(
                self.show.id,
                RadioShowEdit {
                    keep_episodes: Some(n),
                    ..RadioShowEdit::default()
                },
            )
            .await
    }

    /// Asserts the episode lost its file and is unmonitored, and its file is gone from disk.
    async fn assert_pruned(&self, ext: &str, path: &Path) {
        let e = self.episode(ext).await;
        assert!(!path.exists(), "{ext}: {} still exists", path.display());
        assert_eq!(e.filed_path, None, "{ext}");
        assert_eq!(e.file_id, None, "{ext}");
        assert!(!e.monitored, "{ext}");
        assert_eq!(e.status, RadioEpisodeStatus::NotRequested, "{ext}");
    }

    async fn assert_kept(&self, ext: &str, path: &Path) {
        let e = self.episode(ext).await;
        assert!(path.exists(), "{ext}: {} was deleted", path.display());
        assert_eq!(
            e.filed_path.as_deref(),
            Some(path.to_string_lossy().as_ref()),
            "{ext}"
        );
        assert!(e.monitored, "{ext}");
    }
}

/// Minimal FLAC: STREAMINFO (44.1 kHz, stereo, 16 bit, 2 s) + padding + filler bytes.
fn tiny_flac(path: &Path) {
    let mut b = b"fLaC".to_vec();
    b.push(0x00);
    b.extend_from_slice(&[0, 0, 34]);
    b.extend_from_slice(&4096u16.to_be_bytes());
    b.extend_from_slice(&4096u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    let v: u64 = (44_100u64 << 44) | (1u64 << 41) | (15u64 << 36) | (2 * 44_100);
    b.extend_from_slice(&v.to_be_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[0x81, 0, 0, 16]);
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[0xAB; 256]);
    std::fs::write(path, b).unwrap();
}

#[tokio::test]
async fn null_keeps_every_episode_and_two_keeps_the_newest_two() {
    let c = ctx().await;
    assert_eq!(c.show.keep_episodes, None);
    let mut paths = Vec::new();
    for ext in ["e1", "e2", "e3", "e4"] {
        paths.push(c.file(ext).await);
    }
    // No limit: every file stays.
    assert_eq!(c.svc.prune_show(c.show.id).await.unwrap(), KeepSummary::default());
    assert_eq!(c.svc.prune_all_shows().await.unwrap(), KeepSummary::default());
    for (ext, p) in ["e1", "e2", "e3", "e4"].iter().zip(&paths) {
        c.assert_kept(ext, p).await;
    }

    // Saving a limit of 2 applies it at once: the two oldest go.
    let s = c.keep(Some(2)).await.unwrap();
    assert_eq!(s.keep_episodes, Some(2));
    let json = serde_json::to_value(c.svc.get_show(c.show.id).await.unwrap()).unwrap();
    assert_eq!(json["keep_episodes"], 2);
    c.assert_pruned("e1", &paths[0]).await;
    c.assert_pruned("e2", &paths[1]).await;
    c.assert_kept("e3", &paths[2]).await;
    c.assert_kept("e4", &paths[3]).await;
    // The library folder itself is left in place.
    assert!(c.library.path().exists());

    // Applying it again (the hourly job) changes nothing.
    assert_eq!(c.svc.prune_all_shows().await.unwrap(), KeepSummary::default());

    // `null` keeps every episode again.
    let s = c.keep(None).await.unwrap();
    assert_eq!(s.keep_episodes, None);
    let json = serde_json::to_value(&s).unwrap();
    assert!(json["keep_episodes"].is_null());
}

#[tokio::test]
async fn limit_applies_after_filing_by_broadcast_date() {
    let c = ctx().await;
    c.keep(Some(2)).await.unwrap();
    // Filed newest first: the older episode filed third is removed at once.
    let p4 = c.file("e4").await;
    let p3 = c.file("e3").await;
    let p2 = c.file("e2").await;
    c.assert_pruned("e2", &p2).await;
    let p1 = c.file("e1").await;
    c.assert_pruned("e1", &p1).await;
    c.assert_kept("e3", &p3).await;
    c.assert_kept("e4", &p4).await;
    // Unmonitored, so no longer wanted: only the kept episodes count as in the library or
    // wanted, and the pruned two are not searched for again.
    let targets = c
        .svc
        .search_targets(&RadioSearchRequest {
            kind: reelix_api_types::content::ContentSearchKind::Missing,
            ids: vec![],
        })
        .await
        .unwrap();
    assert!(
        targets
            .iter()
            .all(|t| t.episode.external_id != "e1" && t.episode.external_id != "e2")
    );
}

#[tokio::test]
async fn file_outside_the_library_folder_is_not_deleted() {
    let c = ctx().await;
    let p2 = c.file("e2").await;
    let p3 = c.file("e3").await;
    let p4 = c.file("e4").await;
    // e1's library file lies outside the podcast library folder.
    let outside = c.downloads.path().join("elsewhere.mp3");
    std::fs::write(&outside, b"audio").unwrap();
    let ids = c
        .svc
        .upsert_episode_files(
            &[NewEpisodeFile {
                abs_item_id: "item1".into(),
                abs_episode_id: "ep1".into(),
                title: Some("One".into()),
                path: Some(outside.to_string_lossy().into_owned()),
                ..NewEpisodeFile::default()
            }],
            day(2024, 4, 1),
        )
        .await
        .unwrap();
    let e1 = c.episode("e1").await.id;
    c.svc.set_episode_file(e1, ids[0]).await.unwrap();

    c.keep(Some(2)).await.unwrap();
    // e2 is inside the folder and goes; e1 is outside and is left exactly as it was.
    c.assert_pruned("e2", &p2).await;
    c.assert_kept("e3", &p3).await;
    c.assert_kept("e4", &p4).await;
    assert!(outside.exists());
    let e = c.episode("e1").await;
    assert_eq!(e.file_id, Some(ids[0]));
    assert!(e.monitored);
    assert_eq!(e.status, RadioEpisodeStatus::InLibrary);
    let again = c.svc.prune_show(c.show.id).await.unwrap();
    assert_eq!(
        again,
        KeepSummary {
            episodes: 0,
            files_deleted: 0,
            skipped: 1,
        }
    );
}

#[tokio::test]
async fn library_file_inside_the_folder_is_deleted_and_its_record_removed() {
    let c = ctx().await;
    let p3 = c.file("e3").await;
    let p4 = c.file("e4").await;
    // e1 is only known from the podcast library, inside the folder.
    let inside = c.library.path().join("Weekly Pod").join("one.mp3");
    std::fs::write(&inside, b"audio").unwrap();
    let ids = c
        .svc
        .upsert_episode_files(
            &[NewEpisodeFile {
                abs_item_id: "item1".into(),
                abs_episode_id: "ep1".into(),
                path: Some(inside.to_string_lossy().into_owned()),
                ..NewEpisodeFile::default()
            }],
            day(2024, 4, 1),
        )
        .await
        .unwrap();
    let e1 = c.episode("e1").await.id;
    c.svc.set_episode_file(e1, ids[0]).await.unwrap();

    c.keep(Some(2)).await.unwrap();
    c.assert_pruned("e1", &inside).await;
    assert!(c.svc.episode_files().await.unwrap().is_empty());
    c.assert_kept("e3", &p3).await;
    c.assert_kept("e4", &p4).await;
}

#[tokio::test]
async fn following_again_wants_only_the_newest_n() {
    let c = ctx().await;
    let (p3, p4) = (c.file("e3").await, c.file("e4").await);
    c.keep(Some(2)).await.unwrap();
    for follow in [RadioFollow::None, RadioFollow::All] {
        c.svc
            .update_show(
                c.show.id,
                RadioShowEdit {
                    follow: Some(follow),
                    ..RadioShowEdit::default()
                },
            )
            .await
            .unwrap();
    }
    // The two older episodes stay unwanted, so they are not searched for and fetched again.
    for ext in ["e1", "e2"] {
        let e = c.episode(ext).await;
        assert!(!e.monitored, "{ext}");
        assert_eq!(e.status, RadioEpisodeStatus::NotRequested, "{ext}");
    }
    c.assert_kept("e3", &p3).await;
    c.assert_kept("e4", &p4).await;
}

#[tokio::test]
async fn edit_validation() {
    let c = ctx().await;
    for bad in [0, -1, 1001] {
        let err = c.keep(Some(bad)).await.unwrap_err();
        assert!(matches!(err, RadioError::Validation(_)), "{bad}: {err:?}");
    }
    // A refused edit saves nothing.
    assert_eq!(c.svc.get_show(c.show.id).await.unwrap().keep_episodes, None);
    assert_eq!(c.keep(Some(1)).await.unwrap().keep_episodes, Some(1));
    assert_eq!(c.keep(Some(1000)).await.unwrap().keep_episodes, Some(1000));
    // Absent leaves the limit as it is.
    let s = c
        .svc
        .update_show(
            c.show.id,
            RadioShowEdit {
                monitored: Some(true),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.keep_episodes, Some(1000));
    assert_eq!(c.keep(None).await.unwrap().keep_episodes, None);

    // The wire form: absent, `null` and a number.
    let e: RadioShowEdit = serde_json::from_str("{}").unwrap();
    assert_eq!(e.keep_episodes, None);
    let e: RadioShowEdit = serde_json::from_str(r#"{"keep_episodes": null}"#).unwrap();
    assert_eq!(e.keep_episodes, Some(None));
    let e: RadioShowEdit = serde_json::from_str(r#"{"keep_episodes": 3}"#).unwrap();
    assert_eq!(e.keep_episodes, Some(Some(3)));
}
