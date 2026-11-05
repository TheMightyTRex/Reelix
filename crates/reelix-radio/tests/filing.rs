//! Indexer-search targets and filing of finished downloads into the podcast library.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, TimeZone, Utc};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::radio::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_import::content::ContentDownload;
use reelix_import::files::TransferMode;
use reelix_parser::QualityFamily;
use reelix_radio::*;

fn day(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
}

struct Fake;

fn ep(id: &str, series: &str, number: u32, title: &str, at: DateTime<Utc>) -> EpisodeData {
    EpisodeData {
        external_id: id.into(),
        series_external_id: series.into(),
        number: Some(number),
        title: title.into(),
        first_broadcast: Some(at),
        duration_secs: Some(13 * 60),
        ..EpisodeData::default()
    }
}

fn data() -> ShowData {
    ShowData {
        external_id: "p001".into(),
        title: "The Archers".into(),
        station: Some(StationData {
            external_id: "radio4".into(),
            name: "Radio 4".into(),
            logo_url: None,
        }),
        series: vec![
            SeriesData {
                external_id: "s1".into(),
                number: Some(1),
                ..SeriesData::default()
            },
            SeriesData {
                external_id: "specials".into(),
                kind: RadioSeriesKind::Specials,
                ..SeriesData::default()
            },
        ],
        episodes: vec![
            ep("e1", "s1", 1, "Monday", day(2024, 1, 1)),
            ep("e2", "s1", 2, "Tuesday", day(2024, 1, 2)),
            ep("e3", "specials", 1, "Christmas Special", day(2023, 12, 25)),
            // Not broadcast yet ("now" is 2024-04-01).
            ep("e4", "s1", 3, "Later", day(2024, 6, 1)),
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

/// Counts scan requests.
#[derive(Default)]
struct Scans(AtomicUsize);

#[async_trait::async_trait]
impl RadioLibrarySync for Scans {
    async fn sync(&self, _svc: &RadioService) -> anyhow::Result<LibrarySyncSummary> {
        Ok(LibrarySyncSummary::default())
    }
    async fn request_scan(&self) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Ctx {
    svc: RadioService,
    scans: Arc<Scans>,
    show: RadioShowDto,
    library: tempfile::TempDir,
    downloads: tempfile::TempDir,
}

impl Ctx {
    async fn episode(&self, ext: &str) -> i64 {
        self.svc
            .show_episodes(self.show.id)
            .await
            .unwrap()
            .episodes
            .iter()
            .find(|e| e.external_id == ext)
            .unwrap()
            .id
    }

    /// A download folder holding tiny FLAC files.
    fn download(&self, name: &str, files: &[&str]) -> PathBuf {
        let dir = self.downloads.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            tiny_flac(&dir.join(f));
        }
        dir
    }
}

async fn ctx() -> Ctx {
    let scans = Arc::new(Scans::default());
    let svc = RadioService::new(RadioDeps {
        db: Db::in_memory().await.unwrap(),
        events: EventBus::default(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(Arc::new(Fake)),
        library_sync: Some(scans.clone()),
    });
    svc.set_now(day(2024, 4, 1));
    let show = svc
        .add_show(AddRadioShowRequest {
            provider: Some("fake".into()),
            external_id: Some("p001".into()),
            follow: Some(RadioFollow::All),
            monitored: true,
            ..AddRadioShowRequest::default()
        })
        .await
        .unwrap();
    let library = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir_in(library.path().parent().unwrap()).unwrap();
    Ctx {
        svc,
        scans,
        show,
        library,
        downloads,
    }
}

async fn use_library(c: &Ctx) {
    c.svc
        .set_settings(RadioSettingsDto {
            library_folder: Some(c.library.path().to_string_lossy().into_owned()),
            ..RadioSettingsDto::default()
        })
        .await
        .unwrap();
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

fn download(source: PathBuf, mode: TransferMode) -> ContentDownload {
    ContentDownload {
        source,
        release_title: Some("The.Archers.S01E01.Monday.FLAC-GRP".into()),
        mode,
        download_client: Some("qBittorrent".into()),
        download_id: Some("abc".into()),
    }
}

fn filed(o: FilingOutcome) -> FiledEpisode {
    match o {
        FilingOutcome::Filed(f) => f,
        FilingOutcome::Rejected(m) => panic!("rejected: {m:?}"),
    }
}

fn rejected(o: FilingOutcome) -> String {
    match o {
        FilingOutcome::Rejected(m) => m.join("; "),
        FilingOutcome::Filed(f) => panic!("filed {f:?}"),
    }
}

#[tokio::test]
async fn search_targets_are_wanted_monitored_episodes() {
    let c = ctx().await;
    let missing = RadioSearchRequest {
        kind: ContentSearchKind::Missing,
        ids: vec![],
    };
    let targets = c.svc.search_targets(&missing).await.unwrap();
    let mut ids: Vec<&str> = targets.iter().map(|t| t.episode.external_id.as_str()).collect();
    ids.sort_unstable();
    // e4 is not broadcast yet.
    assert_eq!(ids, ["e1", "e2", "e3"]);
    let profile = c
        .svc
        .db()
        .quality_profiles()
        .list_family(QualityFamily::Audiobook)
        .await
        .unwrap()[0]
        .id;
    assert!(
        targets
            .iter()
            .all(|t| t.quality_profile_id == profile && t.monitored)
    );
    let special = targets.iter().find(|t| t.episode.external_id == "e3").unwrap();
    assert_eq!(special.number_label().as_deref(), Some("S00E01"));
    assert_eq!(special.runtime_minutes(), Some(13));
    assert_eq!(
        special.search_terms(),
        [
            "The Archers S00E01",
            "The Archers 2023 12 25",
            "The Archers Christmas Special"
        ]
    );

    // A show's wanted episodes; one episode by hand (even if not broadcast yet).
    let parent = c
        .svc
        .search_targets(&RadioSearchRequest {
            kind: ContentSearchKind::Parent,
            ids: vec![c.show.id],
        })
        .await
        .unwrap();
    assert_eq!(parent.len(), 3);
    let e4 = c.episode("e4").await;
    let item = c
        .svc
        .search_targets(&RadioSearchRequest {
            kind: ContentSearchKind::Item,
            ids: vec![e4],
        })
        .await
        .unwrap();
    assert_eq!(item.len(), 1);

    // Unmonitored show: nothing is wanted.
    c.svc
        .update_show(
            c.show.id,
            RadioShowEdit {
                monitored: Some(false),
                ..RadioShowEdit::default()
            },
        )
        .await
        .unwrap();
    assert!(c.svc.search_targets(&missing).await.unwrap().is_empty());

    // Requests are validated; without a job queue nothing is enqueued.
    assert!(
        c.svc
            .request_search(RadioSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![],
            })
            .await
            .is_err()
    );
    assert!(matches!(
        c.svc
            .request_search(RadioSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![999_999],
            })
            .await,
        Err(RadioError::NotFound(_))
    ));
    assert_eq!(c.svc.request_search(missing).await.unwrap(), None);
}

#[tokio::test]
async fn filing_copies_tags_the_copy_and_never_overwrites() {
    let c = ctx().await;
    let e1 = c.episode("e1").await;
    let src_dir = c.download("The.Archers.S01E01", &["The.Archers.S01E01.Monday.flac"]);
    let src = src_dir.join("The.Archers.S01E01.Monday.flac");
    let before = std::fs::read(&src).unwrap();

    // No library folder yet.
    let msg = rejected(
        c.svc
            .file_download(&download(src_dir.clone(), TransferMode::HardlinkOrCopy), e1)
            .await
            .unwrap(),
    );
    assert!(msg.contains("no podcast library folder"), "{msg}");
    use_library(&c).await;

    let f = filed(
        c.svc
            .file_download(&download(src_dir.clone(), TransferMode::HardlinkOrCopy), e1)
            .await
            .unwrap(),
    );
    let want = c
        .library
        .path()
        .join("The Archers")
        .join("The Archers - S01E01 - Monday.flac");
    assert_eq!(f.destination, want);
    assert_eq!(f.source, src);
    assert!(!f.hardlinked && f.tags_written && !f.remuxed);
    assert_eq!(f.quality.quality, reelix_parser::Quality::AudiobookFlac);
    assert_eq!(f.release_group.as_deref(), Some("GRP"));
    // Tags are in the destination only; the (seeding) source is byte-for-byte untouched.
    let t = tags::read(&want).unwrap();
    assert_eq!(
        t,
        tags::EpisodeTags {
            album: "The Archers".into(),
            title: "Monday".into(),
            disc: 1,
            track: Some(1),
            date: Some("2024-01-01".into()),
            genre: "Radio".into(),
            artist: Some("Radio 4".into()),
            comment: Some("fake:e1".into()),
        }
    );
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert!(tags::read(&src).is_none_or(|t| t.album.is_empty()));
    // No staging leftovers.
    let names: Vec<String> = std::fs::read_dir(want.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["The Archers - S01E01 - Monday.flac"]);
    assert_eq!(c.scans.0.load(Ordering::SeqCst), 1);
    assert_eq!(c.svc.filed_path(e1).await.unwrap(), Some(want.clone()));

    // A filed episode is not searched again while its file exists.
    let missing = RadioSearchRequest {
        kind: ContentSearchKind::Missing,
        ids: vec![],
    };
    let ids: Vec<i64> = c
        .svc
        .search_targets(&missing)
        .await
        .unwrap()
        .iter()
        .map(RadioTarget::episode_id)
        .collect();
    assert!(!ids.contains(&e1));

    // Never overwrite.
    let filed_bytes = std::fs::read(&want).unwrap();
    let msg = rejected(
        c.svc
            .file_download(&download(src_dir.clone(), TransferMode::Copy), e1)
            .await
            .unwrap(),
    );
    assert!(msg.contains("never overwrites"), "{msg}");
    assert_eq!(std::fs::read(&want).unwrap(), filed_bytes);
    assert_eq!(c.scans.0.load(Ordering::SeqCst), 1);

    // The file went away: the episode is wanted again.
    std::fs::remove_file(&want).unwrap();
    let ids: Vec<i64> = c
        .svc
        .search_targets(&missing)
        .await
        .unwrap()
        .iter()
        .map(RadioTarget::episode_id)
        .collect();
    assert!(ids.contains(&e1));
}

#[tokio::test]
async fn specials_hard_links_and_ambiguous_downloads() {
    let c = ctx().await;
    use_library(&c).await;

    // Specials are filed as series 0.
    let e3 = c.episode("e3").await;
    let src = c.download("special", &["xmas.flac"]);
    let f = filed(
        c.svc
            .file_download(&download(src.join("xmas.flac"), TransferMode::Copy), e3)
            .await
            .unwrap(),
    );
    assert_eq!(
        f.destination,
        c.library
            .path()
            .join("The Archers")
            .join("The Archers - S00E01 - Christmas Special.flac")
    );
    assert_eq!(tags::read(&f.destination).unwrap().disc, 0);

    // Several files: the one that names the episode.
    let e2 = c.episode("e2").await;
    let dir = c.download(
        "pack",
        &[
            "The Archers S01E01.flac",
            "The Archers S01E02.flac",
            "cover-sample.flac",
        ],
    );
    let f = filed(
        c.svc
            .file_download(&download(dir.clone(), TransferMode::Copy), e2)
            .await
            .unwrap(),
    );
    assert_eq!(f.source, dir.join("The Archers S01E02.flac"));

    // Same-size files that don't say which is the episode.
    let e1 = c.episode("e1").await;
    let dir = c.download("mystery", &["a.flac", "b.flac"]);
    let msg = rejected(
        c.svc
            .file_download(&download(dir, TransferMode::Copy), e1)
            .await
            .unwrap(),
    );
    assert!(msg.contains("2 audio files"), "{msg}");

    // Without tag writing the file can be hard linked (the source is still not changed).
    c.svc
        .set_naming(RadioNamingDto {
            write_tags: false,
            ..RadioNamingDto::default()
        })
        .await
        .unwrap();
    let dir = c.download("linked", &["ep1.flac"]);
    let before = std::fs::read(dir.join("ep1.flac")).unwrap();
    let f = filed(
        c.svc
            .file_download(&download(dir.clone(), TransferMode::HardlinkOrCopy), e1)
            .await
            .unwrap(),
    );
    assert!(!f.tags_written);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (
            std::fs::metadata(dir.join("ep1.flac")).unwrap(),
            std::fs::metadata(&f.destination).unwrap(),
        );
        assert!(f.hardlinked);
        assert_eq!(a.ino(), b.ino());
    }
    assert_eq!(std::fs::read(dir.join("ep1.flac")).unwrap(), before);
    assert_eq!(c.scans.0.load(Ordering::SeqCst), 3);

    // Naming is validated.
    assert!(
        c.svc
            .set_naming(RadioNamingDto {
                episode_file: "{Nope}".into(),
                ..RadioNamingDto::default()
            })
            .await
            .is_err()
    );
}
