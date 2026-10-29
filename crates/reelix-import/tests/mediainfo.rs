use reelix_import::mediainfo::MediaInfo;
use reelix_parser::model::{AudioCodec, HdrFormat, Language, Resolution, VideoCodec};

fn load(name: &str) -> MediaInfo {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    MediaInfo::from_ffprobe_json(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn uhd_dolby_vision_remux() {
    let m = load("ffprobe_movie_uhd_dv.json");
    assert_eq!(m.container, "matroska");
    assert_eq!(m.duration_secs, Some(8164.352));
    assert_eq!(m.overall_bitrate, Some(57_089_012));
    let v = m.video.as_ref().unwrap();
    assert_eq!(v.codec, Some(VideoCodec::H265));
    assert_eq!((v.width, v.height), (3840, 1600));
    assert_eq!(v.bit_depth, Some(10));
    assert_eq!(v.fps, Some(23.976));
    assert_eq!(v.dovi_profile, Some(8));
    assert_eq!(v.hdr, vec![HdrFormat::DolbyVision, HdrFormat::Hdr10]);
    assert_eq!(m.resolution(), Resolution::R2160p);
    // Cover art is not the video stream.
    assert_eq!(m.audio.len(), 2);
    assert_eq!(m.audio_codec(), Some(AudioCodec::Atmos));
    assert!(m.primary_audio().unwrap().is_atmos);
    assert_eq!(m.audio_channels(), Some(7.1));
    assert_eq!(m.audio[1].codec, Some(AudioCodec::Ac3));
    assert_eq!(m.audio[1].channels, 5.1);
    assert_eq!(m.audio_languages(), vec![Language::English, Language::German]);
    assert_eq!(m.subtitles.len(), 3);
    assert!(!m.subtitles[0].forced);
    assert!(m.subtitles[1].forced);
    assert!(m.subtitles[2].forced, "forced via title");
    assert_eq!(m.subtitles[2].language, Language::French);
    assert_eq!(m.subtitle_languages(), vec![Language::English, Language::French]);
}

#[test]
fn web_episode_with_eac3_atmos() {
    let m = load("ffprobe_episode_web_eac3_atmos.json");
    assert_eq!(m.container, "mp4");
    assert_eq!(m.video_codec(), Some(VideoCodec::H264));
    assert_eq!(m.video.as_ref().unwrap().bit_depth, Some(8));
    assert!(m.hdr().is_empty());
    assert_eq!(m.resolution(), Resolution::R1080p);
    let a = m.primary_audio().unwrap();
    assert_eq!(a.codec, Some(AudioCodec::Eac3));
    assert!(a.is_atmos);
    assert_eq!(a.channels, 5.1);
    assert_eq!(m.subtitles[0].language, Language::Spanish);
    assert_eq!(m.subtitles[0].codec, "mov_text");
}

#[test]
fn anime_10bit_dual_audio() {
    let m = load("ffprobe_anime_10bit.json");
    assert_eq!(m.video.as_ref().unwrap().bit_depth, Some(10));
    assert!(m.hdr().is_empty());
    assert_eq!(m.audio_codec(), Some(AudioCodec::Flac));
    assert_eq!(m.audio[1].codec, Some(AudioCodec::Aac));
    assert_eq!(m.audio_languages(), vec![Language::Japanese, Language::English]);
    assert_eq!(m.duration_secs, Some(1420.064));
}

#[test]
fn sd_xvid_avi_without_format_duration() {
    let m = load("ffprobe_sd_xvid.json");
    assert_eq!(m.container, "avi");
    assert_eq!(m.video_codec(), Some(VideoCodec::Xvid));
    assert_eq!(m.resolution(), Resolution::R480p);
    assert_eq!(m.audio_codec(), Some(AudioCodec::Mp3));
    assert_eq!(m.audio_channels(), Some(2.0));
    assert_eq!(m.duration_secs, Some(2580.12), "falls back to the longest stream");
    assert_eq!(m.audio[0].language, Language::Unknown);
}

#[test]
fn hdr10plus_and_dtsx() {
    let m = load("ffprobe_uhd_hdr10plus_dtsx.json");
    assert_eq!(m.hdr(), &[HdrFormat::Hdr10Plus]);
    assert_eq!(m.audio_codec(), Some(AudioCodec::DtsX));
    assert_eq!(m.audio[1].codec, Some(AudioCodec::DtsHdMa));
    assert_eq!(m.audio[1].language, Language::Italian);
    assert_eq!(m.overall_bitrate, Some(61_000_000));
}

#[test]
fn hlg_broadcast() {
    let m = load("ffprobe_hlg_broadcast.json");
    assert_eq!(m.container, "mpegts");
    assert_eq!(m.hdr(), &[HdrFormat::Hlg]);
    assert_eq!(m.video.as_ref().unwrap().fps, Some(50.0));
    assert_eq!(m.audio[0].language, Language::Norwegian);
    assert_eq!(m.audio[0].channels, 2.0);
}

/// End-to-end `probe` against a fake ffprobe script (no real ffprobe needed).
#[cfg(unix)]
mod fake_ffprobe {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use reelix_import::Error;
    use reelix_import::mediainfo::{ProbeConfig, probe};

    fn script(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("ffprobe");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn cfg(path: PathBuf, secs: u64) -> ProbeConfig {
        ProbeConfig {
            ffprobe_path: path,
            timeout: Duration::from_secs(secs),
        }
    }

    #[tokio::test]
    async fn probe_reads_stdout_and_passes_file_as_last_arg() {
        let d = tempfile::tempdir().unwrap();
        let fixture = format!(
            "{}/tests/fixtures/ffprobe_sd_xvid.json",
            env!("CARGO_MANIFEST_DIR")
        );
        // Fails unless invoked with the expected arguments.
        let body = format!(
            r#"[ "$1 $2 $3 $4 $5 $6" = "-v quiet -print_format json -show_format -show_streams" ] || exit 3
[ "$7" = "./-odd name.avi" ] || exit 4
cat '{fixture}'"#
        );
        let info = probe(Path::new("-odd name.avi"), &cfg(script(d.path(), &body), 10))
            .await
            .unwrap();
        assert_eq!(info.container, "avi");
    }

    #[tokio::test]
    async fn probe_failure_and_timeout() {
        let d = tempfile::tempdir().unwrap();
        let err = probe(
            Path::new("x.mkv"),
            &cfg(script(d.path(), "echo boom >&2; exit 1"), 10),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, Error::FfprobeFailed { code: Some(1), ref stderr } if stderr == "boom"),
            "{err}"
        );
        let d2 = tempfile::tempdir().unwrap();
        let err = probe(Path::new("x.mkv"), &cfg(script(d2.path(), "sleep 5"), 0))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::FfprobeTimeout(_)), "{err}");
    }
}
