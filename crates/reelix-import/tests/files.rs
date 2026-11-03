use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use reelix_import::Error;
use reelix_import::files::{
    ExtraKind, FileKind, PathGuard, PermissionSettings, RecycleBin, SampleReason, SampleRules, ScanOptions,
    TransferMethod, TransferMode, TransferOptions, classify, delete_permanently, detect_extra, detect_sample,
    disk_space, same_filesystem, scan, transfer, transfer_with,
};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(d.path()).unwrap();
    (d, root)
}

#[test]
fn scan_finds_videos_and_skips_junk() {
    let (_d, root) = tmp();
    let dl = root.join("Movie.2010.1080p.BluRay.x264-GRP");
    write(&dl.join("movie.2010.1080p.bluray.x264-grp.mkv"), b"main");
    write(&dl.join("Sample/movie-sample.mkv"), b"s");
    write(&dl.join("Featurettes/Making Of.mkv"), b"f");
    write(&dl.join("movie-trailer.mp4"), b"t");
    write(&dl.join("movie.nfo"), b"nfo");
    write(&dl.join("Subs/English.srt"), b"srt");
    write(&dl.join("@eaDir/movie.mkv@SynoEAStream"), b"x");
    write(&dl.join("@eaDir/thumb.mkv"), b"x");
    write(&dl.join(".hidden/secret.mkv"), b"x");
    write(&dl.join(".partial.mkv"), b"x");
    write(&dl.join("incomplete.mkv.part"), b"x");
    write(&dl.join("#recycle/old.mkv"), b"x");

    let found = scan(&dl, &ScanOptions::default()).unwrap();
    let rel: Vec<String> = found
        .iter()
        .map(|f| f.relative.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(
        rel,
        vec![
            "Featurettes/Making Of.mkv",
            "Sample/movie-sample.mkv",
            "movie-trailer.mp4",
            "movie.2010.1080p.bluray.x264-grp.mkv"
        ]
    );
    assert_eq!(found[0].extra, Some(ExtraKind::Featurette));
    assert_eq!(found[2].extra, Some(ExtraKind::Trailer));
    assert_eq!(found[3].extra, None);
    assert_eq!(found[3].size, 4);
    assert!(found.iter().all(|f| f.kind == FileKind::Video));

    let all = scan(
        &dl,
        &ScanOptions {
            include_non_video: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.len(), 6);
    assert!(all.iter().any(|f| f.kind == FileKind::Subtitle));
    assert!(all.iter().any(|f| f.kind == FileKind::Metadata));

    // A single file can be scanned too.
    let single = scan(&dl.join("movie-trailer.mp4"), &ScanOptions::default()).unwrap();
    assert_eq!(single.len(), 1);
    assert!(matches!(
        scan(&root.join("missing"), &ScanOptions::default()),
        Err(Error::Io { .. })
    ));
}

#[cfg(unix)]
#[test]
fn scan_does_not_follow_symlinked_dirs() {
    let (_d, root) = tmp();
    write(&root.join("outside/evil.mkv"), b"x");
    write(&root.join("dl/real.mkv"), b"x");
    std::os::unix::fs::symlink(root.join("outside"), root.join("dl/link")).unwrap();
    let found = scan(&root.join("dl"), &ScanOptions::default()).unwrap();
    assert_eq!(found.len(), 1);
}

#[test]
fn classification_and_extras() {
    assert_eq!(classify(Path::new("a.MKV")), FileKind::Video);
    assert_eq!(classify(Path::new("a.ass")), FileKind::Subtitle);
    assert_eq!(classify(Path::new("poster.jpg")), FileKind::Image);
    assert_eq!(classify(Path::new("a.exe")), FileKind::Other);
    assert_eq!(classify(Path::new("noext")), FileKind::Other);
    assert_eq!(
        detect_extra(Path::new("/m/Movie-behindthescenes.mkv"), None),
        Some(ExtraKind::BehindTheScenes)
    );
    assert_eq!(
        detect_extra(Path::new("/m/Deleted Scenes/x.mkv"), Some(Path::new("/m"))),
        Some(ExtraKind::DeletedScene)
    );
    // Uppercase release group suffix is not an extra.
    assert_eq!(
        detect_extra(Path::new("/m/Show.S01E01.720p-SCENE.mkv"), None),
        None
    );
    // Folder above the scan root doesn't count.
    assert_eq!(
        detect_extra(Path::new("/Other/dl/x.mkv"), Some(Path::new("/Other/dl"))),
        None
    );
}

#[test]
fn sample_detection() {
    let rules = SampleRules::default();
    let mb = 1024 * 1024;
    let check = |p: &str, size, rt: Option<u64>, exp| {
        detect_sample(Path::new(p), size, rt.map(Duration::from_secs), exp, &rules)
    };
    assert_eq!(
        check("/d/movie-sample.mkv", 2000 * mb, None, None),
        Some(SampleReason::Name)
    );
    assert_eq!(
        check("/d/Sample/movie.mkv", 2000 * mb, None, None),
        Some(SampleReason::Name)
    );
    assert_eq!(
        check("/d/sample.mkv", 2000 * mb, None, None),
        Some(SampleReason::Name)
    );
    assert_eq!(
        check("/d/Samples.Of.Life.2010.mkv", 2000 * mb, None, Some(100)),
        None
    );
    // Runtime: 60s of a 120 min movie is a sample; 100 min isn't.
    assert_eq!(
        check("/d/m.mkv", 2000 * mb, Some(60), Some(120)),
        Some(SampleReason::Runtime)
    );
    assert_eq!(check("/d/m.mkv", 2000 * mb, Some(6000), Some(120)), None);
    // Short anime-style episode: 3 min expected, 2:30 actual is fine.
    assert_eq!(check("/d/e.mkv", 30 * mb, Some(150), Some(3)), None);
    // Unknown runtime falls back to size.
    assert_eq!(
        check("/d/e.mkv", 20 * mb, None, Some(45)),
        Some(SampleReason::Size)
    );
    assert_eq!(check("/d/e.mkv", 400 * mb, None, Some(45)), None);
    assert_eq!(check("/d/e.mkv", 20 * mb, None, None), None);
}

#[test]
fn path_guard_containment() {
    let (_d, root) = tmp();
    let lib = root.join("lib");
    let other = root.join("other");
    fs::create_dir_all(&lib).unwrap();
    fs::create_dir_all(&other).unwrap();
    write(&lib.join("a/file.mkv"), b"x");
    let guard = PathGuard::new([&lib]).unwrap();

    assert_eq!(
        guard.check(&lib.join("a/file.mkv")).unwrap(),
        lib.join("a/file.mkv")
    );
    // Not-yet-existing destinations are fine inside a root.
    assert_eq!(
        guard.check(&lib.join("new/dir/f.mkv")).unwrap(),
        lib.join("new/dir/f.mkv")
    );
    assert!(matches!(
        guard.check(&other.join("x")),
        Err(Error::OutsideRoots(_))
    ));
    assert!(matches!(
        guard.check(&lib.join("a/../../other/x")),
        Err(Error::InvalidPath { .. }) | Err(Error::OutsideRoots(_))
    ));
    assert!(matches!(
        guard.check(&lib.join("missing/../../other")),
        // Windows resolves `..` lexically, so the path exists and lands outside the roots.
        Err(Error::InvalidPath { .. }) | Err(Error::OutsideRoots(_))
    ));
    assert!(matches!(
        guard.check(Path::new("relative/x")),
        Err(Error::InvalidPath { .. })
    ));
    assert!(matches!(
        guard.check_existing(&lib.join("nope")),
        Err(Error::Io { .. })
    ));
    assert!(matches!(
        guard.check_deletable(&lib),
        Err(Error::InvalidPath { .. })
    ));
    assert!(guard.check_deletable(&lib.join("a")).is_ok());
    assert_eq!(guard.root_of(&lib.join("a/file.mkv")), Some(lib.as_path()));
    assert!(PathGuard::new([root.join("does-not-exist")]).is_err());
    assert!(
        PathGuard::new([lib.join("a/file.mkv")]).is_err(),
        "a file is not a root"
    );
}

#[cfg(unix)]
#[test]
fn path_guard_rejects_symlink_escapes() {
    use std::os::unix::fs::symlink;
    let (_d, root) = tmp();
    let lib = root.join("lib");
    write(&root.join("secret/passwd"), b"x");
    fs::create_dir_all(&lib).unwrap();
    symlink(root.join("secret"), lib.join("escape-dir")).unwrap();
    symlink(root.join("secret/passwd"), lib.join("escape-file")).unwrap();
    symlink(root.join("secret/not-there"), lib.join("dangling")).unwrap();
    symlink(lib.join("inner"), lib.join("inner-link")).unwrap();
    fs::create_dir_all(lib.join("inner")).unwrap();
    let guard = PathGuard::new([&lib]).unwrap();
    assert!(matches!(
        guard.check(&lib.join("escape-dir/passwd")),
        Err(Error::OutsideRoots(_))
    ));
    assert!(matches!(
        guard.check(&lib.join("escape-dir/new-file")),
        Err(Error::OutsideRoots(_))
    ));
    assert!(matches!(
        guard.check(&lib.join("escape-file")),
        Err(Error::OutsideRoots(_))
    ));
    assert!(matches!(
        guard.check(&lib.join("dangling")),
        Err(Error::InvalidPath { .. })
    ));
    // Symlinks that stay inside are fine.
    assert_eq!(
        guard.check(&lib.join("inner-link/x")).unwrap(),
        lib.join("inner/x")
    );
}

#[test]
fn hardlink_copy_move_in_one_filesystem() {
    let (_d, root) = tmp();
    let src = root.join("dl/Movie.mkv");
    write(&src, b"0123456789");

    let link = root.join("lib/Movie (2010)/Movie (2010).mkv");
    let out = transfer(&src, &link, TransferMode::HardlinkOrCopy).unwrap();
    assert_eq!(out.method, TransferMethod::Hardlinked);
    assert_eq!(out.bytes, 10);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&link).unwrap().ino()
        );
        assert_eq!(fs::metadata(&src).unwrap().nlink(), 2);
    }
    assert_eq!(
        transfer(&src, &link, TransferMode::Hardlink).unwrap().method,
        TransferMethod::AlreadyInPlace
    );

    let copy = root.join("lib/copy/Movie.mkv");
    assert_eq!(
        transfer(&src, &copy, TransferMode::Copy).unwrap().method,
        TransferMethod::Copied
    );
    assert_eq!(fs::read(&copy).unwrap(), b"0123456789");
    assert_eq!(
        fs::metadata(&copy).unwrap().modified().unwrap(),
        fs::metadata(&src).unwrap().modified().unwrap(),
        "mtime preserved"
    );
    assert!(matches!(
        transfer(&src, &copy, TransferMode::Copy),
        Err(Error::DestinationExists(_))
    ));

    // Overwrite replaces atomically.
    let src2 = root.join("dl/Better.mkv");
    write(&src2, b"better");
    let opts = TransferOptions {
        overwrite: true,
        ..Default::default()
    };
    transfer_with(&src2, &copy, TransferMode::Copy, &opts).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), b"better");

    let moved = root.join("lib/moved/Better.mkv");
    assert_eq!(
        transfer(&src2, &moved, TransferMode::Move).unwrap().method,
        TransferMethod::Moved
    );
    assert!(!src2.exists());
    assert!(moved.exists());

    // No temp files anywhere.
    let leftovers = scan(
        &root,
        &ScanOptions {
            include_non_video: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        leftovers
            .iter()
            .all(|f| !f.path.to_string_lossy().contains(".reelix-"))
    );
    assert!(fs::read_dir(root.join("lib/copy")).unwrap().count() == 1);

    assert!(matches!(
        transfer(&root.join("dl"), &root.join("x"), TransferMode::Copy),
        Err(Error::InvalidPath { .. })
    ));
    assert!(matches!(
        transfer(&root.join("nope"), &root.join("x"), TransferMode::Copy),
        Err(Error::Io { .. })
    ));
}

#[cfg(unix)]
#[test]
fn permissions_are_applied_to_copies_and_new_dirs() {
    use std::os::unix::fs::PermissionsExt;
    let (_d, root) = tmp();
    let src = root.join("src.mkv");
    write(&src, b"x");
    let dst = root.join("new/folder/dst.mkv");
    let opts = TransferOptions {
        permissions: PermissionSettings {
            file_mode: Some(0o640),
            dir_mode: Some(0o750),
            uid: None,
            gid: None,
        },
        ..Default::default()
    };
    transfer_with(&src, &dst, TransferMode::Copy, &opts).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dst), 0o640);
    assert_eq!(mode(&root.join("new/folder")), 0o750);
    assert_eq!(mode(&root.join("new")), 0o750);
}

#[test]
fn recycle_bin_lifecycle() {
    let (_d, root) = tmp();
    let lib = root.join("lib");
    let bin_dir = root.join("bin");
    write(&lib.join("Movie (2010)/Movie (2010).mkv"), b"old");
    write(&lib.join("Other (2011)/Other (2011).mkv"), b"o");
    write(&root.join("outside/file.mkv"), b"x");
    let guard = PathGuard::new([&lib]).unwrap();
    let bin = RecycleBin::new(&bin_dir, Some(Duration::from_secs(7 * 86400))).unwrap();

    let old = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let to = bin
        .recycle_at(&lib.join("Movie (2010)/Movie (2010).mkv"), &guard, old)
        .unwrap();
    assert_eq!(
        to,
        bin_dir.join("2026-01-01_12-00-00/Movie (2010)/Movie (2010).mkv")
    );
    assert_eq!(fs::read(&to).unwrap(), b"old");
    assert!(!lib.join("Movie (2010)/Movie (2010).mkv").exists());

    // Same name again in the same second gets a suffix.
    write(&lib.join("Movie (2010)/Movie (2010).mkv"), b"newer");
    let to2 = bin
        .recycle_at(&lib.join("Movie (2010)/Movie (2010).mkv"), &guard, old)
        .unwrap();
    assert_eq!(to2.file_name().unwrap(), "Movie (2010) (2).mkv");

    // Whole folders.
    let recent = Utc.with_ymd_and_hms(2026, 1, 9, 0, 0, 0).unwrap();
    let dir_to = bin.recycle_at(&lib.join("Other (2011)"), &guard, recent).unwrap();
    assert_eq!(dir_to, bin_dir.join("2026-01-09_00-00-00/Other (2011)"));
    assert!(dir_to.join("Other (2011).mkv").exists());

    // Never outside the roots, never a root itself.
    assert!(matches!(
        bin.recycle(&root.join("outside/file.mkv"), &guard),
        Err(Error::OutsideRoots(_))
    ));
    assert!(bin.recycle(&lib, &guard).is_err());

    // Entries the bin did not create are never purged.
    write(&bin_dir.join("notes.txt"), b"keep");
    fs::create_dir_all(bin_dir.join("2020-01-01 not a stamp")).unwrap();
    fs::create_dir_all(bin_dir.join("Movies")).unwrap();

    let report = bin
        .cleanup_at(Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap())
        .unwrap();
    assert_eq!((report.removed, report.kept), (1, 1));
    assert!(!bin_dir.join("2026-01-01_12-00-00").exists());
    assert_eq!(bin.empty().unwrap().removed, 1);
    assert_eq!(fs::read_dir(&bin_dir).unwrap().count(), 3);
    assert!(bin_dir.join("notes.txt").exists() && bin_dir.join("Movies").exists());

    let forever = RecycleBin::new(root.join("bin2"), None).unwrap();
    assert_eq!(forever.cleanup().unwrap().removed, 0);

    write(&lib.join("gone.mkv"), b"x");
    delete_permanently(&lib.join("gone.mkv"), &guard).unwrap();
    assert!(!lib.join("gone.mkv").exists());
    assert!(delete_permanently(&root.join("outside/file.mkv"), &guard).is_err());
    assert!(root.join("outside/file.mkv").exists());
}

#[test]
fn free_space_and_filesystem_identity() {
    let (_d, root) = tmp();
    let space = disk_space(&root.join("not/yet/created")).unwrap();
    assert!(space.total > 0);
    assert!(space.available <= space.total);
    fs::create_dir_all(root.join("a")).unwrap();
    if cfg!(unix) {
        assert_eq!(same_filesystem(&root, &root.join("a")), Some(true));
    }
}
