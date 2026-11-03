//! Music service flows against a fake MusicBrainz provider, temp directories and small FLAC
//! files generated (and tagged with lofty) inside the tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::NaiveDate;
use reelix_api_types::content::CreateContentRootFolderRequest;
use reelix_api_types::music::*;
use reelix_core::ContentKind;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_decision::{ProfileItem, QualityProfile};
use reelix_import::files::TransferMode;
use reelix_library::{DynMetadata, LibraryService};
use reelix_metadata::*;
use reelix_music::tags::{FileTags, read, write};
use reelix_music::{AlbumImportOutcome, ContentDownload, MusicDeps, MusicError, MusicService, Wanted};
use reelix_parser::{ParsedMusic, Quality, QualityFamily, parse_music};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------- fixtures

/// A minimal FLAC stream: STREAMINFO (44.1 kHz, stereo, 16 bit, `secs` long) + a few bytes.
fn flac(path: &Path, secs: u64, tags: &FileTags) {
    let mut b = b"fLaC".to_vec();
    b.push(0x00); // STREAMINFO (a PADDING block follows, like real encoders write)
    b.extend_from_slice(&[0, 0, 34]);
    b.extend_from_slice(&4096u16.to_be_bytes());
    b.extend_from_slice(&4096u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    let v: u64 = (44_100u64 << 44) | (1u64 << 41) | (15u64 << 36) | (secs * 44_100);
    b.extend_from_slice(&v.to_be_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[0x81, 0, 0, 16]); // last block: PADDING
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[0xAB; 256]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b).unwrap();
    if !tags.is_empty() {
        write(path, tags, None).unwrap();
    }
}

fn tags(track: u32, title: &str) -> FileTags {
    FileTags {
        track: Some(track),
        title: Some(title.into()),
        artist: Some("Radiohead".into()),
        album: Some("OK Computer".into()),
        ..Default::default()
    }
}

const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OKC: &str = "b1392450-e666-3926-a536-22c65f834433";
const KID_A: &str = "c2d56fdb-c3b3-40f5-8bb8-1f7ab2ab5c22";
const SINGLE: &str = "d3e67fdb-c3b3-40f5-8bb8-1f7ab2ab5c33";
const LIVE: &str = "e4f78fdb-c3b3-40f5-8bb8-1f7ab2ab5c44";
const FUTURE: &str = "f5089fdb-c3b3-40f5-8bb8-1f7ab2ab5c55";

fn credit() -> Vec<ArtistCredit> {
    vec![ArtistCredit {
        artist_id: ARTIST.into(),
        name: "Radiohead".into(),
        join_phrase: String::new(),
    }]
}

fn rg(id: &str, title: &str, primary: &str, secondary: &[&str], date: &str) -> ReleaseGroupMetadata {
    ReleaseGroupMetadata {
        id: id.into(),
        title: title.into(),
        disambiguation: None,
        primary_type: Some(primary.into()),
        secondary_types: secondary.iter().map(|s| s.to_string()).collect(),
        first_release_date: Some(date.into()),
        artist_credit: credit(),
        genres: vec!["rock".into()],
        rating: Some(4.5),
    }
}

fn medium(pos: u32, rel: &str, titles: &[(&str, u64)]) -> MediumMetadata {
    MediumMetadata {
        position: pos,
        format: Some("CD".into()),
        title: None,
        tracks: titles
            .iter()
            .enumerate()
            .map(|(i, (t, secs))| TrackMetadata {
                id: format!("{rel}-t{pos}-{}", i + 1),
                recording_id: format!("rec-{}", t.to_lowercase().replace(' ', "-")),
                position: i as u32 + 1,
                number: (i + 1).to_string(),
                title: (*t).into(),
                length_ms: Some(secs * 1000),
                artist_credit: Vec::new(),
            })
            .collect(),
    }
}

fn release(id: &str, group: &str, title: &str, country: &str, media: Vec<MediumMetadata>) -> ReleaseMetadata {
    ReleaseMetadata {
        id: id.into(),
        release_group_id: group.into(),
        title: title.into(),
        status: Some("Official".into()),
        country: Some(country.into()),
        date: Some("1997-05-21".into()),
        label: Some("Parlophone".into()),
        catalog_number: None,
        barcode: None,
        disambiguation: None,
        media,
    }
}

const OKC_TRACKS: &[(&str, u64)] = &[
    ("Airbag", 284),
    ("Paranoid Android", 383),
    ("Subterranean Homesick Alien", 267),
];
const KID_A_DISC1: &[(&str, u64)] = &[("Everything in Its Right Place", 251), ("Kid A", 284)];
const KID_A_DISC2: &[(&str, u64)] = &[("The National Anthem", 351), ("How to Disappear Completely", 356)];

#[derive(Default)]
struct FakeMb {
    cover_url: Mutex<Option<String>>,
    releases_calls: Mutex<HashMap<String, u32>>,
    extra_releases: Mutex<HashMap<String, Vec<ReleaseMetadata>>>,
}

impl FakeMb {
    fn groups() -> Vec<ReleaseGroupMetadata> {
        vec![
            rg(OKC, "OK Computer", "Album", &[], "1997-05-21"),
            rg(KID_A, "Kid A", "Album", &[], "2000-10-02"),
            rg(SINGLE, "Creep", "Single", &[], "1992-09-21"),
            rg(LIVE, "I Might Be Wrong", "Album", &["Live"], "2001-11-12"),
            rg(FUTURE, "Next One", "Album", &[], "2099-01-01"),
        ]
    }
}

#[async_trait]
impl MusicMetadataProvider for FakeMb {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn search_artists(&self, q: &str) -> Result<Vec<ArtistSearchHit>, MetaError> {
        Ok(if q.to_lowercase().contains("radiohead") {
            vec![ArtistSearchHit {
                id: ARTIST.into(),
                name: "Radiohead".into(),
                sort_name: "Radiohead".into(),
                disambiguation: None,
                artist_type: Some("Group".into()),
                country: Some("GB".into()),
                score: 100,
            }]
        } else {
            Vec::new()
        })
    }
    async fn search_release_groups(
        &self,
        q: &str,
        _artist: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError> {
        Ok(Self::groups()
            .into_iter()
            .filter(|g| g.title.to_lowercase().contains(&q.to_lowercase()))
            .map(|g| ReleaseGroupSearchHit {
                id: g.id,
                title: g.title,
                artist_id: ARTIST.into(),
                artist_name: "Radiohead".into(),
                primary_type: g.primary_type,
                secondary_types: g.secondary_types,
                first_release_date: g.first_release_date,
                score: 90,
            })
            .collect())
    }
    async fn artist(&self, mbid: &str) -> Result<ArtistMetadata, MetaError> {
        if mbid != ARTIST {
            return Err(MetaError::NotFound);
        }
        Ok(ArtistMetadata {
            id: ARTIST.into(),
            name: "Radiohead".into(),
            sort_name: "Radiohead".into(),
            disambiguation: None,
            artist_type: Some("Group".into()),
            country: Some("GB".into()),
            genres: vec!["rock".into()],
            links: vec![],
            release_groups: Self::groups(),
        })
    }
    async fn release_group(&self, mbid: &str) -> Result<ReleaseGroupMetadata, MetaError> {
        Self::groups()
            .into_iter()
            .find(|g| g.id == mbid)
            .ok_or(MetaError::NotFound)
    }
    async fn releases(&self, group: &str) -> Result<Vec<ReleaseMetadata>, MetaError> {
        *self
            .releases_calls
            .lock()
            .unwrap()
            .entry(group.into())
            .or_default() += 1;
        let mut out = match group {
            OKC => vec![
                release(
                    "okc-gb",
                    OKC,
                    "OK Computer",
                    "GB",
                    vec![medium(1, "okc-gb", OKC_TRACKS)],
                ),
                release(
                    "okc-us",
                    OKC,
                    "OK Computer",
                    "US",
                    vec![medium(1, "okc-us", OKC_TRACKS)],
                ),
            ],
            KID_A => vec![release(
                "kida",
                KID_A,
                "Kid A",
                "GB",
                vec![medium(1, "kida", KID_A_DISC1), medium(2, "kida", KID_A_DISC2)],
            )],
            FUTURE => vec![],
            _ => vec![release(
                "other",
                group,
                "Other",
                "GB",
                vec![medium(1, "other", &[("X", 100)])],
            )],
        };
        out.extend(
            self.extra_releases
                .lock()
                .unwrap()
                .get(group)
                .cloned()
                .unwrap_or_default(),
        );
        Ok(out)
    }
    async fn release(&self, _mbid: &str) -> Result<ReleaseMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn cover_art(&self, _group: &str) -> Result<Option<CoverArt>, MetaError> {
        Ok(self.cover_url.lock().unwrap().clone().map(|u| CoverArt {
            front: Some(u.clone()),
            thumb_500: Some(u),
            thumb_250: None,
        }))
    }
}

struct Env {
    svc: Arc<MusicService>,
    db: Db,
    mb: Arc<FakeMb>,
    root: PathBuf,
    root_id: i64,
    downloads: PathBuf,
    profile_id: i64,
    _dir: tempfile::TempDir,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("music");
    let downloads = dir.path().join("downloads");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&downloads).unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    let mb = Arc::new(FakeMb::default());
    let svc = Arc::new(MusicService::new(MusicDeps {
        db: db.clone(),
        events,
        http: reqwest::Client::new(),
        library,
        metadata: mb.clone(),
    }));
    svc.set_today(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
    let profile = db
        .quality_profiles()
        .create(&QualityProfile {
            id: 0,
            name: "Test Lossless".into(),
            items: Quality::MUSIC
                .iter()
                .map(|q| ProfileItem::quality(*q, true))
                .collect(),
            cutoff: u32::from(Quality::Flac.id()),
            upgrade_allowed: true,
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
            format_items: vec![],
            language: None,
            family: QualityFamily::Music,
            min_size_mb: None,
            max_size_mb: None,
        })
        .await
        .unwrap();
    let rf = svc
        .add_root_folder(CreateContentRootFolderRequest {
            path: root.to_string_lossy().into_owned(),
            kind: ContentKind::Music,
        })
        .await
        .unwrap();
    Env {
        svc,
        db,
        mb,
        root,
        root_id: rf.id,
        downloads,
        profile_id: profile.id,
        _dir: dir,
    }
}

fn add_req(e: &Env, monitor: ArtistMonitor) -> AddArtistRequest {
    AddArtistRequest {
        musicbrainz_id: ARTIST.into(),
        quality_profile_id: e.profile_id,
        metadata_profile_id: 1,
        root_folder_id: e.root_id,
        monitored: true,
        monitor,
        monitor_new_items: NewAlbumMonitor::All,
        selected_release_group_ids: vec![],
        tags: vec![],
        search_on_add: false,
    }
}

fn download(e: &Env, name: &str, mode: TransferMode) -> ContentDownload {
    ContentDownload {
        source: e.downloads.join(name),
        release_title: Some(format!("{name} [FLAC]")),
        mode,
        download_client: None,
        download_id: None,
    }
}

async fn album_id(e: &Env, rg: &str) -> i64 {
    e.svc.album_by_mbid(rg).await.unwrap().unwrap().id
}

/// The three OK Computer tracks as a download folder.
fn okc_download(e: &Env, name: &str) -> PathBuf {
    let d = e.downloads.join(name);
    for (i, (t, secs)) in OKC_TRACKS.iter().enumerate() {
        flac(
            &d.join(format!("{:02} - {t}.flac", i + 1)),
            secs + 1,
            &tags(i as u32 + 1, t),
        );
    }
    d
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn add_artist_applies_metadata_profile_and_monitoring() {
    let e = env().await;
    let a = e
        .svc
        .add_artist(add_req(&e, ArtistMonitor::Future))
        .await
        .unwrap();
    assert_eq!(a.name, "Radiohead");
    assert_eq!(a.path, e.root.join("Radiohead").to_string_lossy());
    let albums = e
        .svc
        .list_albums(&AlbumListQuery {
            artist_id: Some(a.id),
            ..Default::default()
        })
        .await
        .unwrap();
    let titles: Vec<&str> = albums.items.iter().map(|a| a.title.as_str()).collect();
    // Standard profile: albums + EPs without secondary types.
    assert_eq!(albums.total, 3, "{titles:?}");
    assert!(!titles.contains(&"Creep") && !titles.contains(&"I Might Be Wrong"));
    let future = albums.items.iter().find(|a| a.title == "Next One").unwrap();
    assert!(future.monitored);
    assert!(
        albums
            .items
            .iter()
            .filter(|a| a.title != "Next One")
            .all(|a| !a.monitored)
    );
    // Releases are fetched lazily for unmonitored albums.
    assert!(!e.mb.releases_calls.lock().unwrap().contains_key(OKC));
    let okc = e.svc.get_album(album_id(&e, OKC).await).await.unwrap();
    assert_eq!(okc.releases.len(), 2);
    assert_eq!(okc.releases.iter().filter(|r| r.selected).count(), 1);
    assert_eq!(okc.statistics.track_count, 3);
    let tracks = e.svc.tracks(okc.id).await.unwrap();
    assert_eq!(
        tracks.iter().map(|t| t.track_number).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    // Duplicate add → conflict; lookups flag the library artist.
    assert!(matches!(
        e.svc.add_artist(add_req(&e, ArtistMonitor::All)).await,
        Err(MusicError::Conflict(_))
    ));
    let hits = e.svc.lookup_artists("radiohead").await.unwrap();
    assert_eq!(hits[0].artist_id, Some(a.id));
    let hits = e.svc.lookup_artists(&format!("mbid:{ARTIST}")).await.unwrap();
    assert_eq!(hits.len(), 1);
    let albums = e.svc.lookup_albums("kid").await.unwrap();
    assert_eq!(albums[0].album_id, Some(album_id(&e, KID_A).await));
    assert_eq!(albums[0].year, Some(2000));
    // Metadata profiles: in use → refuse delete.
    assert!(matches!(
        e.svc.delete_metadata_profile(1).await,
        Err(MusicError::Conflict(_))
    ));
    let p = e
        .svc
        .create_metadata_profile(MetadataProfileDto {
            id: 0,
            name: "Everything".into(),
            primary_types: vec![AlbumType::Album, AlbumType::Single],
            secondary_types: vec![SecondaryAlbumType::Live],
            release_statuses: vec![ReleaseStatus::Official],
        })
        .await
        .unwrap();
    // Switching the profile + refresh adds the single and the live album.
    e.svc
        .update_artist(
            a.id,
            ArtistEdit {
                metadata_profile_id: Some(p.id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(e.svc.refresh_artists(Some(vec![a.id]), true).await.unwrap(), 0);
    let all = e.svc.get_artist(a.id).await.unwrap();
    assert_eq!(all.statistics.album_count, 5);
    let creep = e.svc.album_by_mbid(SINGLE).await.unwrap().unwrap();
    assert!(creep.monitored, "new items follow monitor_new_items = all");
    // Root folder in use.
    assert!(matches!(
        e.svc.delete_root_folder(e.root_id).await,
        Err(MusicError::Conflict(_))
    ));
}

#[tokio::test]
async fn artist_paths_stay_inside_music_roots() {
    let e = env().await;
    let a = e.svc.add_artist(add_req(&e, ArtistMonitor::None)).await.unwrap();
    let outside = e.root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let edit = |path: &Path, move_files: bool| ArtistEdit {
        path: Some(path.to_string_lossy().into_owned()),
        move_files,
        ..Default::default()
    };
    // A new path must resolve inside a music root, with or without moving.
    for bad in [
        PathBuf::from("/etc/reelix"),
        outside.join("Radiohead"),
        e.root.join("../outside/Radiohead"),
        e.root.clone(),
    ] {
        for move_files in [false, true] {
            assert!(
                matches!(
                    e.svc.update_artist(a.id, edit(&bad, move_files)).await,
                    Err(MusicError::Validation(_))
                ),
                "{}",
                bad.display()
            );
        }
    }
    assert_eq!(e.svc.get_artist(a.id).await.unwrap().path, a.path);
    // Moving inside the root works.
    std::fs::create_dir_all(&a.path).unwrap();
    let to = e.root.join("Radiohead (moved)");
    let moved = e.svc.update_artist(a.id, edit(&to, true)).await.unwrap();
    assert_eq!(Path::new(&moved.path), to);
    assert!(to.is_dir() && !Path::new(&a.path).exists());
    // A stored path outside the roots is never moved from.
    let victim = outside.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    sqlx::query("UPDATE artists SET path = ? WHERE id = ?")
        .bind(victim.to_string_lossy().into_owned())
        .bind(a.id)
        .execute(e.db.pool())
        .await
        .unwrap();
    let stolen = e.root.join("Stolen");
    assert!(e.svc.update_artist(a.id, edit(&stolen, true)).await.is_err());
    assert!(victim.is_dir());
    assert!(!stolen.exists());
}

#[tokio::test]
async fn import_writes_tags_to_copies_and_never_touches_the_source() {
    let e = env().await;
    let a = e.svc.add_artist(add_req(&e, ArtistMonitor::All)).await.unwrap();
    let id = album_id(&e, OKC).await;
    // A tiny PNG served as cover art.
    let server = MockServer::start().await;
    let png = b"\x89PNG\r\n\x1a\n0000IHDRfakeimagedata".to_vec();
    Mock::given(method("GET"))
        .and(path("/cover.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png.clone()))
        .mount(&server)
        .await;
    *e.mb.cover_url.lock().unwrap() = Some(format!("{}/cover.png", server.uri()));
    let src = okc_download(&e, "Radiohead - OK Computer (1997)");
    let before: Vec<Vec<u8>> = std::fs::read_dir(&src)
        .unwrap()
        .map(|f| std::fs::read(f.unwrap().path()).unwrap())
        .collect();

    let out = e
        .svc
        .import_api()
        .import_download(
            &download(&e, "Radiohead - OK Computer (1997)", TransferMode::HardlinkOrCopy),
            id,
        )
        .await
        .unwrap();
    let AlbumImportOutcome::Imported(imp) = out else {
        panic!("{out:?}")
    };
    assert_eq!(imp.files.len(), 3);
    assert!(!imp.is_upgrade);
    let first = e.root.join("Radiohead/OK Computer (1997)/01 - Airbag.flac");
    assert!(first.is_file());
    assert!(e.root.join("Radiohead/OK Computer (1997)/cover.png").is_file());
    let info = read(&first).unwrap();
    assert_eq!(info.tags.title.as_deref(), Some("Airbag"));
    assert_eq!(info.tags.album_artist.as_deref(), Some("Radiohead"));
    assert_eq!(info.tags.musicbrainz_release_group_id.as_deref(), Some(OKC));
    assert!(info.tags.musicbrainz_release_id.is_some());
    assert_eq!(info.tags.musicbrainz_recording_id.as_deref(), Some("rec-airbag"));
    // Seeding files untouched (copied, not hardlinked + rewritten).
    let after: Vec<Vec<u8>> = std::fs::read_dir(&src)
        .unwrap()
        .map(|f| std::fs::read(f.unwrap().path()).unwrap())
        .collect();
    assert_eq!(before, after);
    assert!(
        read(&src.join("01 - Airbag.flac"))
            .unwrap()
            .tags
            .musicbrainz_release_id
            .is_none()
    );
    // No staging leftovers.
    let names: Vec<String> = std::fs::read_dir(e.root.join("Radiohead"))
        .unwrap()
        .map(|f| f.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["OK Computer (1997)"]);
    // Database: every track linked, stats, wanted, availability.
    let album = e.svc.get_album(id).await.unwrap();
    assert_eq!(album.statistics.track_file_count, 3);
    assert_eq!(album.selected_release_id, Some(imp.release_id));
    let tracks = e.svc.tracks(id).await.unwrap();
    assert!(
        tracks
            .iter()
            .all(|t| t.track_file.as_ref().is_some_and(|f| f.cutoff_met))
    );
    assert_eq!(tracks[0].track_file.as_ref().unwrap().quality_name, "FLAC");
    let missing = e.svc.wanted(&AlbumWantedQuery::default()).await.unwrap();
    assert!(missing.items.iter().all(|a| a.id != id));
    assert!(missing.items.iter().any(|a| a.title == "Kid A"));
    let av = e
        .svc
        .availability(&[OKC.into(), KID_A.into(), "nope".into()])
        .await
        .unwrap();
    assert_eq!(av[OKC].status, reelix_music::AvailabilityStatus::Available);
    assert_eq!(av[KID_A].status, reelix_music::AvailabilityStatus::Monitored);
    assert!(!av.contains_key("nope"));
    let st = e.svc.get_artist(a.id).await.unwrap().statistics;
    assert_eq!(st.track_file_count, 3);
    // Same quality again → not an upgrade.
    let again = e
        .svc
        .import_api()
        .import_download(
            &download(&e, "Radiohead - OK Computer (1997)", TransferMode::Copy),
            id,
        )
        .await
        .unwrap();
    assert!(matches!(again, AlbumImportOutcome::Rejected(_)), "{again:?}");
    // Rescan keeps the links; delete a file.
    let s = e.svc.scan_artists(&[a.id]).await.unwrap();
    assert_eq!((s.added, s.updated, s.removed), (0, 3, 0));
    assert_eq!(e.svc.get_album(id).await.unwrap().statistics.track_file_count, 3);
    let f = e.svc.track_files(id).await.unwrap()[0].clone();
    e.svc.delete_track_file(f.id).await.unwrap();
    assert!(!Path::new(&f.path).exists());
    assert_eq!(e.svc.get_album(id).await.unwrap().statistics.track_file_count, 2);
}

#[tokio::test]
async fn multi_disc_albums_get_disc_folders_and_move_removes_sources() {
    let e = env().await;
    e.svc.add_artist(add_req(&e, ArtistMonitor::All)).await.unwrap();
    let mut s = e.svc.settings().await.unwrap();
    s.naming.multi_disc_folder = Some("CD {medium}".into());
    s.embed_cover = false;
    s.save_cover_file = false;
    e.svc.set_settings(s).await.unwrap();
    let d = e.downloads.join("Kid A");
    for (disc, list) in [(1, KID_A_DISC1), (2, KID_A_DISC2)] {
        for (i, (t, secs)) in list.iter().enumerate() {
            let mut tg = tags(i as u32 + 1, t);
            tg.disc = Some(disc);
            flac(&d.join(format!("{disc}-{:02} {t}.flac", i + 1)), *secs, &tg);
        }
    }
    let id = album_id(&e, KID_A).await;
    let out = e
        .svc
        .import_api()
        .import_download(&download(&e, "Kid A", TransferMode::Move), id)
        .await
        .unwrap();
    assert!(matches!(out, AlbumImportOutcome::Imported(_)), "{out:?}");
    let base = e.root.join("Radiohead/Kid A (2000)");
    assert!(
        base.join("CD 1/01 - Everything in Its Right Place.flac")
            .is_file()
    );
    assert!(base.join("CD 2/02 - How to Disappear Completely.flac").is_file());
    let info = read(&base.join("CD 2/01 - The National Anthem.flac")).unwrap();
    assert_eq!(
        (info.tags.disc, info.tags.disc_total, info.tags.track),
        (Some(2), Some(2), Some(1))
    );
    assert!(scan_is_empty(&d), "move removes the sources");
    assert_eq!(
        e.svc.import_api().destination_root(id).await.unwrap(),
        e.root.join("Radiohead")
    );

    // Rename to another scheme.
    let mut s = e.svc.settings().await.unwrap();
    s.naming.multi_disc_folder = None;
    e.svc.set_settings(s).await.unwrap();
    let artist_id = e.svc.get_album(id).await.unwrap().artist_id;
    let preview = e.svc.rename_preview(artist_id).await.unwrap();
    assert_eq!(preview.len(), 4);
    assert!(
        preview
            .iter()
            .any(|p| p.new_path == "Kid A (2000)/02-01 - The National Anthem.flac")
    );
    assert_eq!(e.svc.execute_rename(artist_id).await.unwrap(), 4);
    assert!(base.join("02-01 - The National Anthem.flac").is_file());
    assert!(!base.join("CD 2").exists(), "empty disc folders are pruned");
    assert!(e.svc.rename_preview(artist_id).await.unwrap().is_empty());
}

fn scan_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut r| r.next().is_none())
}

#[tokio::test]
async fn ambiguous_releases_need_a_choice_and_bad_matches_are_rejected() {
    let e = env().await;
    e.svc.add_artist(add_req(&e, ArtistMonitor::All)).await.unwrap();
    // A second OK Computer edition whose durations differ (a different master).
    let id = album_id(&e, OKC).await;
    e.mb.extra_releases.lock().unwrap().insert(
        OKC.into(),
        vec![release(
            "okc-remaster",
            OKC,
            "OK Computer (remaster)",
            "XE",
            vec![medium(
                1,
                "okc-remaster",
                &[
                    ("Airbag", 289),
                    ("Paranoid Android", 388),
                    ("Subterranean Homesick Alien", 272),
                ],
            )],
        )],
    );
    e.svc
        .refresh_artists(Some(vec![e.svc.get_album(id).await.unwrap().artist_id]), true)
        .await
        .unwrap();
    assert_eq!(e.svc.get_album(id).await.unwrap().releases.len(), 3);
    // Untagged files whose durations sit between both masters.
    let d = e.downloads.join("okc-untagged");
    for (t, secs) in [
        ("Airbag", 286),
        ("Paranoid Android", 385),
        ("Subterranean Homesick Alien", 269),
    ] {
        flac(&d.join(format!("{t}.flac")), secs, &FileTags::default());
    }
    let out = e
        .svc
        .import_api()
        .import_download(&download(&e, "okc-untagged", TransferMode::Copy), id)
        .await
        .unwrap();
    let AlbumImportOutcome::NeedsRelease {
        album_id: needs_id,
        candidates,
    } = out
    else {
        panic!("{out:?}")
    };
    assert_eq!(needs_id, id);
    assert!(candidates.len() >= 2);
    // The user picks the remaster and restricts imports to it.
    let remaster = e
        .svc
        .get_album(id)
        .await
        .unwrap()
        .releases
        .into_iter()
        .find(|r| r.musicbrainz_release_id == "okc-remaster")
        .unwrap();
    let chosen = e
        .svc
        .choose_release(
            id,
            ChooseReleaseRequest {
                release_id: remaster.id,
            },
        )
        .await
        .unwrap();
    assert_eq!(chosen.selected_release_id, Some(remaster.id));
    e.svc
        .update_album(
            id,
            AlbumEdit {
                any_release_ok: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let out = e
        .svc
        .import_api()
        .import_download(&download(&e, "okc-untagged", TransferMode::Copy), id)
        .await
        .unwrap();
    let AlbumImportOutcome::Imported(imp) = out else {
        panic!("{out:?}")
    };
    assert_eq!(imp.release_id, remaster.id);
    // Choosing another release keeps the files linked by disc/track.
    let gb = e
        .svc
        .get_album(id)
        .await
        .unwrap()
        .releases
        .into_iter()
        .find(|r| r.musicbrainz_release_id == "okc-gb")
        .unwrap();
    let a = e
        .svc
        .choose_release(id, ChooseReleaseRequest { release_id: gb.id })
        .await
        .unwrap();
    assert_eq!(a.statistics.track_file_count, 3);
    assert!(matches!(
        e.svc
            .choose_release(id, ChooseReleaseRequest { release_id: 9999 })
            .await,
        Err(MusicError::Validation(_))
    ));

    // Wrong durations → rejected, nothing written.
    let kid = album_id(&e, KID_A).await;
    let d = e.downloads.join("wrong");
    for i in 1..=4 {
        flac(
            &d.join(format!("{i:02} - Song {i}.flac")),
            30,
            &FileTags::default(),
        );
    }
    let out = e
        .svc
        .import_api()
        .import_download(&download(&e, "wrong", TransferMode::Copy), kid)
        .await
        .unwrap();
    assert!(matches!(out, AlbumImportOutcome::Rejected(_)), "{out:?}");
    assert!(!e.root.join("Radiohead/Kid A (2000)").exists());
}

#[tokio::test]
async fn import_is_all_or_nothing() {
    let e = env().await;
    e.svc.add_artist(add_req(&e, ArtistMonitor::All)).await.unwrap();
    let id = album_id(&e, OKC).await;
    let d = okc_download(&e, "broken");
    // The third file is not a real FLAC: staging + tag writing fails for it.
    std::fs::write(
        d.join("03 - Subterranean Homesick Alien.flac"),
        b"definitely not audio",
    )
    .unwrap();
    let res = e
        .svc
        .import_api()
        .import_download(&download(&e, "broken", TransferMode::Copy), id)
        .await;
    match res {
        Err(_) | Ok(AlbumImportOutcome::Rejected(_)) => {}
        Ok(other) => panic!("{other:?}"),
    }
    let artist_dir = e.root.join("Radiohead");
    let leftovers: Vec<_> = std::fs::read_dir(&artist_dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(e.svc.get_album(id).await.unwrap().statistics.track_file_count, 0);
}

#[tokio::test]
async fn library_import_scans_existing_folders() {
    let e = env().await;
    let folder = e.root.join("Radiohead (existing)");
    for (i, (t, secs)) in OKC_TRACKS.iter().enumerate() {
        let mut tg = tags(i as u32 + 1, t);
        tg.musicbrainz_release_group_id = Some(OKC.into());
        flac(
            &folder.join("OK Computer").join(format!("{:02} {t}.flac", i + 1)),
            *secs,
            &tg,
        );
    }
    let candidates = e.svc.import_scan(e.root_id).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].folder, "Radiohead (existing)");
    let res = e
        .svc
        .library_import(MusicLibraryImportRequest {
            root_folder_id: e.root_id,
            items: vec![MusicImportItem {
                path: candidates[0].path.clone(),
                musicbrainz_id: ARTIST.into(),
                quality_profile_id: e.profile_id,
                metadata_profile_id: 1,
                monitor: ArtistMonitor::Existing,
                tags: vec![],
            }],
        })
        .await
        .unwrap();
    assert_eq!(res.added.len(), 1, "{:?}", res.errors);
    let okc = e.svc.album_by_mbid(OKC).await.unwrap().unwrap();
    assert!(okc.monitored, "existing → albums with files monitored");
    assert_eq!(okc.statistics.track_file_count, 3);
    assert!(!e.svc.album_by_mbid(KID_A).await.unwrap().unwrap().monitored);
    assert!(e.svc.import_scan(e.root_id).await.unwrap().is_empty());
    // Search API targets.
    let api = e.svc.search_api();
    let t = api.target(okc.id).await.unwrap();
    assert_eq!(t.tracks.len(), 3);
    assert_eq!(t.runtime_minutes, Some(16));
    assert_eq!(t.existing.len(), 3);
    assert!(api.targets(&[], Wanted::Missing).await.unwrap().is_empty());
    assert_eq!(api.targets(&[okc.id], Wanted::Any).await.unwrap().len(), 1);
    let parsed = ParsedMusic {
        artist: Some("Radiohead".into()),
        album: Some("OK Computer".into()),
        ..parse_music("Radiohead - OK Computer (1997) [FLAC]")
    };
    assert_eq!(api.match_release(&parsed).await.unwrap(), Some(okc.id));
    let other = ParsedMusic {
        artist: Some("Muse".into()),
        album: Some("Absolution".into()),
        ..parse_music("x")
    };
    assert_eq!(api.match_release(&other).await.unwrap(), None);
}

#[tokio::test]
async fn add_single_album_creates_unmonitored_artist() {
    let e = env().await;
    let album = e
        .svc
        .add_album(AddAlbumRequest {
            musicbrainz_release_group_id: SINGLE.into(),
            quality_profile_id: None,
            metadata_profile_id: None,
            root_folder_id: None,
            tags: vec![],
            search_on_add: false,
            added_by_request_id: None,
        })
        .await;
    let album = album.unwrap();
    assert!(album.monitored);
    assert_eq!(album.title, "Creep");
    let others = e
        .svc
        .list_albums(&AlbumListQuery {
            artist_id: Some(album.artist_id),
            monitored: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(others.total, 1, "only the requested album is monitored");
    assert!(matches!(
        e.svc
            .add_album(AddAlbumRequest {
                musicbrainz_release_group_id: SINGLE.into(),
                quality_profile_id: None,
                metadata_profile_id: None,
                root_folder_id: None,
                tags: vec![],
                search_on_add: false,
                added_by_request_id: None,
            })
            .await,
        Err(MusicError::Conflict(_))
    ));
    // Monitoring + deletion.
    let kid = album_id(&e, KID_A).await;
    let m = e
        .svc
        .monitor_albums(AlbumMonitorRequest {
            album_ids: vec![kid],
            monitored: true,
        })
        .await
        .unwrap();
    assert!(m[0].monitored);
    e.svc.delete_album(kid, false).await.unwrap();
    assert!(e.svc.album_by_mbid(KID_A).await.unwrap().is_none());
    e.svc.delete_artist(album.artist_id, true).await.unwrap();
    assert!(e.svc.album_by_mbid(SINGLE).await.unwrap().is_none());
    // Settings validation.
    let mut s = MusicSettingsDto::default();
    s.naming.track_file = "{Movie Title}".into();
    assert!(matches!(
        e.svc.set_settings(s).await,
        Err(MusicError::Validation(_))
    ));
}
