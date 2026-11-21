//! RAR unpacking with the real `unrar`. The archives in `fixtures/rar` hold one 6,000-byte
//! `Some.Movie.2020.1080p.WEB-DL.x264-GRP.mkv` (random bytes, stored) unless noted, and were made
//! with RAR 7.01:
//!
//! - `single.rar`: one volume.
//! - `split.part1.rar` to `split.part3.rar`: `-v2500b`.
//! - `oldsplit.rar`, `oldsplit.r00`, `oldsplit.r01`: the same set, renamed to the old naming.
//! - `novideo.rar`: only `readme.txt`.
//! - `password.rar` (`-p`) and `password-headers.rar` (`-hp`), password `Reelix`.
//! - `corrupt.rar`: `single.rar` with 16 bytes of the video overwritten.
//!
//! Without `unrar` on PATH these tests pass without checking anything, unless
//! `REELIX_REQUIRE_UNRAR` is set (as in CI on Linux).

use std::fs;
use std::path::{Path, PathBuf};

use reelix_import::unpack::{
    ArchiveKind, STAGING_DIR, UnpackConfig, archive_kind, find_archives, holds_media, is_first_volume,
    remove_unpacked, unpack_download,
};

const VIDEO: &str = "Some.Movie.2020.1080p.WEB-DL.x264-GRP.mkv";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar")
}

/// A download folder holding copies of the named fixtures.
fn download(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let dl = fs::canonicalize(d.path())
        .unwrap()
        .join("Some.Movie.2020.1080p.WEB-DL.x264-GRP");
    fs::create_dir_all(&dl).unwrap();
    for n in names {
        fs::copy(fixtures().join(n), dl.join(n)).unwrap();
    }
    (d, dl)
}

fn unrar_available() -> bool {
    let found = std::process::Command::new("unrar")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !found {
        // CI sets it to "1" on Linux and to "" elsewhere, so an empty value means not required.
        assert!(
            std::env::var("REELIX_REQUIRE_UNRAR").map_or(true, |v| v.is_empty()),
            "REELIX_REQUIRE_UNRAR is set but unrar is not on PATH"
        );
        eprintln!("unrar is not on PATH; skipping");
    }
    found
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The archives are still there, the video is next to them and nothing is left in staging.
fn assert_unpacked(dl: &Path, archives: &[&str]) {
    let video = dl.join(VIDEO);
    assert_eq!(fs::metadata(&video).unwrap().len(), 6000);
    for a in archives {
        assert!(dl.join(a).is_file(), "{a} was removed");
    }
    assert!(!dl.join(STAGING_DIR).exists());
}

#[test]
fn first_volumes() {
    for first in [
        "a.rar",
        "A.RAR",
        "a.part1.rar",
        "a.part01.rar",
        "a.part001.rar",
        "a.partial.rar",
    ] {
        assert!(is_first_volume(Path::new(first)), "{first}");
    }
    for later in [
        "a.part2.rar",
        "a.part02.rar",
        "a.part10.rar",
        "a.r00",
        "a.r01",
        "a.mkv",
        "rar",
    ] {
        assert!(!is_first_volume(Path::new(later)), "{later}");
    }
}

#[test]
fn find_archives_skips_later_volumes_and_unpack_folders() {
    let (_d, dl) = download(&["single.rar", "split.part1.rar", "split.part2.rar", "oldsplit.r00"]);
    fs::create_dir_all(dl.join("_UNPACK_x")).unwrap();
    fs::copy(fixtures().join("single.rar"), dl.join("_UNPACK_x/single.rar")).unwrap();
    fs::create_dir_all(dl.join("Subs")).unwrap();
    fs::copy(fixtures().join("novideo.rar"), dl.join("Subs/subs.rar")).unwrap();
    let found: Vec<PathBuf> = find_archives(&dl)
        .into_iter()
        .map(|p| p.strip_prefix(&dl).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        found,
        [
            PathBuf::from("Subs/subs.rar"),
            PathBuf::from("single.rar"),
            PathBuf::from("split.part1.rar"),
        ]
    );
}

#[tokio::test]
async fn single_archive_is_unpacked_next_to_it() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["single.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &["single.rar"]);

    remove_unpacked(&u.files).await;
    assert_eq!(names(&dl), ["single.rar"]);
}

#[tokio::test]
async fn split_set_is_unpacked_once() {
    if !unrar_available() {
        return;
    }
    let parts = ["split.part1.rar", "split.part2.rar", "split.part3.rar"];
    let (_d, dl) = download(&parts);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &parts);
    let single = {
        let (_d2, dl2) = download(&["single.rar"]);
        unpack_download(&dl2, &UnpackConfig::default()).await;
        fs::read(dl2.join(VIDEO)).unwrap()
    };
    assert_eq!(fs::read(dl.join(VIDEO)).unwrap(), single);
}

#[tokio::test]
async fn old_style_split_set_is_unpacked() {
    if !unrar_available() {
        return;
    }
    let parts = ["oldsplit.rar", "oldsplit.r00", "oldsplit.r01"];
    let (_d, dl) = download(&parts);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &parts);
}

#[tokio::test]
async fn archive_without_video_is_left_packed() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["novideo.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["The archives in the download hold no video, audio or book files"]
    );
    assert_eq!(names(&dl), ["novideo.rar"]);
}

#[tokio::test]
async fn password_protected_archives_say_so() {
    if !unrar_available() {
        return;
    }
    for archive in ["password.rar", "password-headers.rar"] {
        let (_d, dl) = download(&[archive]);
        let u = unpack_download(&dl, &UnpackConfig::default()).await;
        assert!(u.files.is_empty(), "{archive}");
        assert_eq!(
            u.messages,
            [format!("{archive}: the archive is password protected")]
        );
        assert_eq!(names(&dl), [archive]);
    }
}

#[tokio::test]
async fn damaged_archive_says_so_and_leaves_no_partial_video() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["corrupt.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(u.messages, ["corrupt.rar: the archive is damaged or incomplete"]);
    assert_eq!(names(&dl), ["corrupt.rar"]);
}

#[tokio::test]
async fn missing_volume_says_so() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["split.part1.rar", "split.part3.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["split.part1.rar: the archive is damaged or incomplete"]
    );
    assert!(!dl.join(VIDEO).exists());
}

#[tokio::test]
async fn existing_files_are_not_replaced() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["single.rar"]);
    fs::write(dl.join(VIDEO), b"already here").unwrap();
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(fs::read(dl.join(VIDEO)).unwrap(), b"already here");
    assert!(!dl.join(STAGING_DIR).exists());
}

#[tokio::test]
async fn missing_unrar_says_so() {
    let (_d, dl) = download(&["single.rar", "split.part1.rar"]);
    let config = UnpackConfig {
        unrar_path: dl.join("no-such-unrar"),
        ..UnpackConfig::default()
    };
    let u = unpack_download(&dl, &config).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["unrar is not installed, so the RAR archives cannot be unpacked"]
    );
}

#[tokio::test]
async fn folder_without_archives_does_nothing() {
    let (_d, dl) = download(&[]);
    fs::write(dl.join("readme.txt"), b"x").unwrap();
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty() && u.messages.is_empty());
}

/// Writes a ZIP at `path` holding `entries` (name, bytes), deflated.
fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        z.start_file(*name, opts).unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap();
}

fn seven_zip() -> Option<&'static str> {
    ["7zz", "7z", "7za"].into_iter().find(|p| {
        std::process::Command::new(p)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    })
}

#[test]
fn archive_kinds() {
    for (name, kind) in [
        ("a.zip", Some(ArchiveKind::Zip)),
        ("A.ZIP", Some(ArchiveKind::Zip)),
        ("a.7z", Some(ArchiveKind::SevenZip)),
        ("a.7z.001", Some(ArchiveKind::SevenZip)),
        ("a.7z.002", None),
        ("a.rar", Some(ArchiveKind::Rar)),
        ("a.part01.rar", Some(ArchiveKind::Rar)),
        ("a.part02.rar", None),
        ("a.r00", None),
        ("a.cbz", None),
        ("a.epub", None),
    ] {
        assert_eq!(archive_kind(Path::new(name)), kind, "{name}");
    }
}

#[tokio::test]
async fn a_zip_is_unpacked_and_its_programs_left_out() {
    let (_d, dl) = download(&[]);
    make_zip(
        &dl.join("bundle.zip"),
        &[
            (VIDEO, &[7u8; 6000]),
            ("Subs/English.srt", b"1"),
            ("setup.exe", b"MZ"),
            ("../escape.mkv", b"no"),
        ],
    );
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO), dl.join("Subs/English.srt")]);
    assert_unpacked(&dl, &["bundle.zip"]);
    assert!(!dl.join("setup.exe").exists(), "programs are never unpacked");
    assert!(!dl.parent().unwrap().join("escape.mkv").exists());
    remove_unpacked(&u.files).await;
    assert_eq!(names(&dl), ["Subs", "bundle.zip"]);
}

#[tokio::test]
async fn a_book_in_a_rar_in_a_zip_is_unpacked_in_turn() {
    // Scene book releases (the owner's BitBook downloads, 19 November 2026): a .zip holding a
    // .rar holding the .epub, with an .nfo and a file_id.diz.
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&[]);
    let inner = fs::read(fixtures().join("single.rar")).unwrap();
    make_zip(
        &dl.join("bbj74pca.zip"),
        &[
            ("bbj74pc.rar", &inner),
            ("bb-utopi.nfo", b"nfo"),
            ("file_id.diz", b"diz"),
        ],
    );
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert!(u.files.contains(&dl.join("bbj74pc.rar")), "{:?}", u.files);
    assert!(u.files.contains(&dl.join(VIDEO)), "{:?}", u.files);
    assert_eq!(fs::metadata(dl.join(VIDEO)).unwrap().len(), 6000);
    // The zip from the client stays; everything unpacked goes after import.
    remove_unpacked(&u.files).await;
    assert_eq!(names(&dl), ["bbj74pca.zip"]);
}

#[tokio::test]
async fn each_episode_folder_of_a_season_pack_is_unpacked() {
    // The Addams Family S02 (19 November 2026): one folder per episode, each an old-style
    // split RAR set NZBGet did not unpack.
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&[]);
    for ep in ["S02E01", "S02E02"] {
        let folder = dl.join(format!("The.Addams.Family.1964.{ep}.1080p.BluRay.x264-BROADCAST"));
        fs::create_dir_all(&folder).unwrap();
        for part in ["oldsplit.rar", "oldsplit.r00", "oldsplit.r01"] {
            fs::copy(fixtures().join(part), folder.join(part)).unwrap();
        }
        fs::write(folder.join("x.nfo"), b"nfo").unwrap();
    }
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files.len(), 2, "{:?}", u.files);
    for f in &u.files {
        assert_eq!(f.file_name().unwrap().to_string_lossy(), VIDEO);
        assert!(
            f.parent().unwrap().join("oldsplit.r00").exists(),
            "the set stays for seeding"
        );
    }
}

#[tokio::test]
async fn a_damaged_zip_says_so() {
    let (_d, dl) = download(&[]);
    fs::write(dl.join("broken.zip"), b"PK\x03\x04 not really a zip").unwrap();
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(u.messages, ["broken.zip: the archive is damaged or incomplete"]);
}

#[tokio::test]
async fn a_7z_is_unpacked() {
    let Some(program) = seven_zip() else {
        assert!(
            std::env::var("REELIX_REQUIRE_UNRAR").map_or(true, |v| v.is_empty()),
            "7-Zip is required where unrar is"
        );
        eprintln!("7-Zip is not on PATH; skipping");
        return;
    };
    let (_d, dl) = download(&[]);
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join(VIDEO), [9u8; 6000]).unwrap();
    fs::write(src.path().join("trojan.scr"), b"MZ").unwrap();
    let status = std::process::Command::new(program)
        .args(["a", "-bd", "-y"])
        .arg(dl.join("movie.7z"))
        .arg(src.path().join(VIDEO))
        .arg(src.path().join("trojan.scr"))
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &["movie.7z"]);
    assert!(!dl.join("trojan.scr").exists());
}

#[tokio::test]
async fn a_missing_7zip_says_so() {
    let (_d, dl) = download(&[]);
    fs::write(dl.join("movie.7z"), b"7z").unwrap();
    let config = UnpackConfig {
        seven_zip_paths: vec![dl.join("no-such-7z")],
        ..UnpackConfig::default()
    };
    let u = unpack_download(&dl, &config).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["7-Zip is not installed, so the 7z archives cannot be unpacked"]
    );
}

#[tokio::test]
async fn holds_media_looks_inside_archives() {
    let (_d, dl) = download(&[]);
    fs::write(dl.join("Scrubs.S02E08.exe"), b"MZ").unwrap();
    assert!(!holds_media(&dl, &UnpackConfig::default()).await);
    assert!(!holds_media(&dl.join("Scrubs.S02E08.exe"), &UnpackConfig::default()).await);
    make_zip(
        &dl.join("payload.zip"),
        &[("readme.txt", b"x"), ("run.exe", b"MZ")],
    );
    assert!(
        !holds_media(&dl, &UnpackConfig::default()).await,
        "a zip of programs is not media"
    );
    make_zip(&dl.join("book.zip"), &[("Book.epub", b"x")]);
    assert!(holds_media(&dl, &UnpackConfig::default()).await);
    fs::remove_file(dl.join("book.zip")).unwrap();
    fs::write(dl.join("Song.flac"), b"x").unwrap();
    assert!(holds_media(&dl, &UnpackConfig::default()).await);
}
