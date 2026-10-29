//! Media information via `ffprobe`.
//!
//! [`probe`] runs `ffprobe -v quiet -print_format json -show_format -show_streams <file>`
//! (no shell involved) and [`MediaInfo::from_ffprobe_json`] maps the output, so tests can
//! work from fixture JSON without ffprobe installed.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reelix_parser::model::{AudioCodec, HdrFormat, Language, Resolution, VideoCodec};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Where to find ffprobe and how long to wait for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeConfig {
    pub ffprobe_path: PathBuf,
    pub timeout: Duration,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            ffprobe_path: PathBuf::from("ffprobe"),
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MediaInfo {
    /// Short container name, e.g. `matroska`, `mp4`, `avi`, `mpegts`.
    pub container: String,
    pub duration_secs: Option<f64>,
    /// Bits per second for the whole file.
    pub overall_bitrate: Option<u64>,
    /// Primary (first non-cover-art) video stream.
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub subtitles: Vec<SubtitleStream>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VideoStream {
    /// ffprobe codec name, e.g. `hevc`.
    pub codec_name: String,
    pub codec: Option<VideoCodec>,
    pub profile: Option<String>,
    pub bit_depth: Option<u8>,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub bitrate: Option<u64>,
    /// Dynamic range formats present; empty for SDR. Dolby Vision with an HDR10 base
    /// layer yields `[DolbyVision, Hdr10]`.
    pub hdr: Vec<HdrFormat>,
    pub dovi_profile: Option<u8>,
    pub is_3d: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStream {
    pub codec_name: String,
    pub codec: Option<AudioCodec>,
    pub profile: Option<String>,
    /// Channel count as ffprobe reports it (e.g. 8 for 7.1).
    pub channel_count: u32,
    /// Display channels, e.g. 5.1, 7.1, 2.0.
    pub channels: f32,
    pub language: Language,
    /// Raw language tag (ISO 639-2), when present.
    pub language_tag: Option<String>,
    pub title: Option<String>,
    pub is_atmos: bool,
    pub is_default: bool,
    pub bitrate: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubtitleStream {
    pub language: Language,
    pub language_tag: Option<String>,
    pub codec: String,
    pub forced: bool,
    pub title: Option<String>,
}

impl MediaInfo {
    /// Map raw `ffprobe -print_format json -show_format -show_streams` output.
    pub fn from_ffprobe_json(json: &[u8]) -> Result<Self> {
        let raw: raw::Probe = serde_json::from_slice(json)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: raw::Probe) -> Result<Self> {
        if raw.streams.is_empty() {
            return Err(Error::NoStreams);
        }
        let format = raw.format.unwrap_or_default();
        let mut info = MediaInfo {
            container: short_container(format.format_name.as_deref().unwrap_or_default()),
            duration_secs: num(&format.duration).filter(|d| *d > 0.0),
            overall_bitrate: num(&format.bit_rate).map(|b| b as u64),
            ..Default::default()
        };
        for s in &raw.streams {
            match s.codec_type.as_deref() {
                Some("video") if info.video.is_none() && !s.disposition.attached_pic() => {
                    info.video = Some(map_video(s));
                }
                Some("audio") => info.audio.push(map_audio(s)),
                Some("subtitle") => info.subtitles.push(SubtitleStream {
                    language_tag: s.tag("language").map(str::to_string),
                    language: s.tag("language").map_or(Language::Unknown, language_from_tag),
                    codec: s.codec_name.clone().unwrap_or_default(),
                    forced: s.disposition.flag("forced")
                        || s.tag("title")
                            .is_some_and(|t| t.to_ascii_lowercase().contains("forced")),
                    title: s.tag("title").map(str::to_string),
                }),
                _ => {}
            }
        }
        if info.duration_secs.is_none() {
            info.duration_secs = raw
                .streams
                .iter()
                .filter_map(|s| num(&s.duration))
                .fold(None, |acc: Option<f64>, d| Some(acc.map_or(d, |a| a.max(d))));
        }
        if info.video.is_none() && info.audio.is_empty() {
            return Err(Error::NoStreams);
        }
        Ok(info)
    }

    /// Resolution bucket derived from the video dimensions.
    pub fn resolution(&self) -> Resolution {
        self.video.as_ref().map_or(Resolution::Unknown, |v| {
            resolution_from_dimensions(v.width, v.height)
        })
    }

    pub fn video_codec(&self) -> Option<VideoCodec> {
        self.video.as_ref().and_then(|v| v.codec)
    }

    /// Primary audio stream: the default-flagged one, else the first.
    pub fn primary_audio(&self) -> Option<&AudioStream> {
        self.audio.iter().find(|a| a.is_default).or(self.audio.first())
    }

    /// Codec of the primary audio stream. TrueHD Atmos maps to [`AudioCodec::Atmos`];
    /// EAC3 Atmos stays `Eac3` with `is_atmos` set.
    pub fn audio_codec(&self) -> Option<AudioCodec> {
        self.primary_audio().and_then(|a| a.codec)
    }

    pub fn audio_channels(&self) -> Option<f32> {
        self.primary_audio().map(|a| a.channels)
    }

    pub fn hdr(&self) -> &[HdrFormat] {
        self.video.as_ref().map_or(&[], |v| v.hdr.as_slice())
    }

    /// Distinct audio languages in stream order (Unknown skipped).
    pub fn audio_languages(&self) -> Vec<Language> {
        dedup_langs(self.audio.iter().map(|a| a.language))
    }

    /// Distinct subtitle languages in stream order (Unknown skipped).
    pub fn subtitle_languages(&self) -> Vec<Language> {
        dedup_langs(self.subtitles.iter().map(|s| s.language))
    }
}

fn dedup_langs(it: impl Iterator<Item = Language>) -> Vec<Language> {
    let mut out = Vec::new();
    for l in it {
        if l != Language::Unknown && !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// Run ffprobe on `file` and parse its output.
pub async fn probe(file: &Path, config: &ProbeConfig) -> Result<MediaInfo> {
    // A leading '-' would be read as an option; anchor relative paths.
    let target = if file.to_string_lossy().starts_with('-') {
        Path::new(".").join(file)
    } else {
        file.to_path_buf()
    };
    let mut cmd = tokio::process::Command::new(&config.ffprobe_path);
    cmd.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(&target)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let child = cmd.spawn().map_err(|source| Error::FfprobeSpawn {
        path: config.ffprobe_path.clone(),
        source,
    })?;
    let output = tokio::time::timeout(config.timeout, child.wait_with_output())
        .await
        .map_err(|_| Error::FfprobeTimeout(config.timeout))?
        .map_err(|source| Error::FfprobeSpawn {
            path: config.ffprobe_path.clone(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::FfprobeFailed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    MediaInfo::from_ffprobe_json(&output.stdout)
}

/// Resolution bucket from pixel dimensions. Width is considered so that letterboxed
/// encodes (e.g. 1920x800) still count as 1080p.
pub fn resolution_from_dimensions(width: u32, height: u32) -> Resolution {
    match (width, height) {
        (0, 0) => Resolution::Unknown,
        (w, h) if w >= 3200 || h >= 2000 => Resolution::R2160p,
        (w, h) if w >= 1800 || h >= 1000 => Resolution::R1080p,
        (w, h) if w >= 1200 || h >= 700 => Resolution::R720p,
        (_, h) if h >= 560 => Resolution::R576p,
        (_, h) if h >= 520 => Resolution::R540p,
        (w, h) if h >= 400 || w >= 640 => Resolution::R480p,
        _ => Resolution::R360p,
    }
}

fn short_container(format_name: &str) -> String {
    let first = format_name.split(',').next().unwrap_or_default();
    match first {
        "mov" if format_name.contains("mp4") => "mp4".into(),
        other => other.to_string(),
    }
}

fn num(v: &Option<serde_json::Value>) -> Option<f64> {
    match v.as_ref()? {
        serde_json::Value::String(s) => s.trim().parse().ok(),
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn parse_rate(r: &str) -> Option<f64> {
    let (n, d) = r.split_once('/').unwrap_or((r, "1"));
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| (n / d * 1000.0).round() / 1000.0)
}

fn map_video(s: &raw::Stream) -> VideoStream {
    let codec_name = s.codec_name.clone().unwrap_or_default();
    let tag = s
        .codec_tag_string
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let codec = match codec_name.as_str() {
        "h264" => Some(VideoCodec::H264),
        "hevc" | "h265" => Some(VideoCodec::H265),
        "av1" => Some(VideoCodec::Av1),
        "vp9" => Some(VideoCodec::Vp9),
        "vc1" | "wmv3" => Some(VideoCodec::Vc1),
        "mpeg2video" | "mpeg1video" => Some(VideoCodec::Mpeg2),
        "mpeg4" | "msmpeg4v3" if tag.starts_with("div") || tag == "dx50" => Some(VideoCodec::Divx),
        "mpeg4" => Some(VideoCodec::Xvid),
        _ => None,
    };
    let bit_depth = s
        .bits_per_raw_sample
        .as_deref()
        .and_then(|b| b.parse::<u8>().ok())
        .or_else(|| bit_depth_from_pix_fmt(s.pix_fmt.as_deref().unwrap_or_default()));

    let mut hdr = Vec::new();
    let mut dovi_profile = None;
    let mut dovi_compat = None;
    let mut has_mastering = false;
    let mut has_hdr10plus = false;
    for sd in &s.side_data_list {
        let kind = sd.side_data_type.as_deref().unwrap_or_default();
        if kind.starts_with("DOVI configuration") {
            dovi_profile = sd.dv_profile.map(|p| p as u8);
            dovi_compat = sd.dv_bl_signal_compatibility_id;
        } else if kind.contains("Mastering display") || kind.contains("Content light level") {
            has_mastering = true;
        } else if kind.contains("SMPTE2094-40") || kind.contains("HDR10+") {
            has_hdr10plus = true;
        }
    }
    let transfer = s.color_transfer.as_deref().unwrap_or_default();
    let pq = transfer == "smpte2084";
    let hlg = transfer == "arib-std-b67";
    if dovi_profile.is_some() {
        hdr.push(HdrFormat::DolbyVision);
    }
    // Base-layer format: from the DV compatibility id when present, else the stream.
    match dovi_compat {
        Some(1 | 6) => hdr.push(HdrFormat::Hdr10),
        Some(4) => hdr.push(HdrFormat::Hlg),
        Some(_) => {}
        None if hlg => hdr.push(HdrFormat::Hlg),
        None if pq && dovi_profile.is_some() && dovi_profile != Some(5) => hdr.push(HdrFormat::Hdr10),
        None if pq && dovi_profile.is_none() => {
            hdr.push(
                if has_mastering || s.color_primaries.as_deref() == Some("bt2020") {
                    HdrFormat::Hdr10
                } else {
                    HdrFormat::Pq
                },
            );
        }
        None => {}
    }
    if has_hdr10plus {
        hdr.retain(|h| *h != HdrFormat::Hdr10);
        hdr.push(HdrFormat::Hdr10Plus);
    }
    let stereo = s.tag("stereo_mode").unwrap_or_default().to_ascii_lowercase();
    VideoStream {
        codec,
        codec_name,
        profile: s.profile.clone().filter(|p| !p.is_empty()),
        bit_depth,
        width: s.width.unwrap_or(0),
        height: s.height.unwrap_or(0),
        fps: s
            .avg_frame_rate
            .as_deref()
            .and_then(parse_rate)
            .or_else(|| s.r_frame_rate.as_deref().and_then(parse_rate)),
        bitrate: num(&s.bit_rate).map(|b| b as u64),
        hdr,
        dovi_profile,
        is_3d: !stereo.is_empty() && stereo != "mono",
    }
}

fn bit_depth_from_pix_fmt(pix: &str) -> Option<u8> {
    if pix.is_empty() {
        return None;
    }
    if pix.contains("p10") {
        Some(10)
    } else if pix.contains("p12") {
        Some(12)
    } else {
        Some(8)
    }
}

fn map_audio(s: &raw::Stream) -> AudioStream {
    let codec_name = s.codec_name.clone().unwrap_or_default();
    let profile = s.profile.clone().filter(|p| !p.is_empty());
    let prof = profile.as_deref().unwrap_or_default().to_ascii_lowercase();
    let title = s.tag("title").map(str::to_string);
    let title_l = title.as_deref().unwrap_or_default().to_ascii_lowercase();
    let is_atmos = prof.contains("atmos") || title_l.contains("atmos");
    let codec = match codec_name.as_str() {
        "aac" => Some(AudioCodec::Aac),
        "ac3" => Some(AudioCodec::Ac3),
        "eac3" => Some(AudioCodec::Eac3),
        "truehd" if is_atmos => Some(AudioCodec::Atmos),
        "truehd" => Some(AudioCodec::TrueHd),
        "dts" if prof.contains("dts:x") || prof.contains("dts-x") => Some(AudioCodec::DtsX),
        "dts" if prof.contains("ma") => Some(AudioCodec::DtsHdMa),
        "dts" => Some(AudioCodec::Dts),
        "flac" => Some(AudioCodec::Flac),
        "mp3" | "mp2" => Some(AudioCodec::Mp3),
        "opus" => Some(AudioCodec::Opus),
        c if c.starts_with("pcm_") => Some(AudioCodec::Pcm),
        _ => None,
    };
    let channel_count = s.channels.unwrap_or(0);
    AudioStream {
        codec,
        codec_name,
        profile,
        channel_count,
        channels: channels_display(s.channel_layout.as_deref(), channel_count),
        language_tag: s.tag("language").map(str::to_string),
        language: s.tag("language").map_or(Language::Unknown, language_from_tag),
        title,
        is_atmos,
        is_default: s.disposition.flag("default"),
        bitrate: num(&s.bit_rate).map(|b| b as u64),
    }
}

/// Display channels (5.1, 7.1, 2.0…) from an ffprobe layout, falling back to the count.
pub fn channels_display(layout: Option<&str>, count: u32) -> f32 {
    if let Some(layout) = layout {
        let base = layout.split('(').next().unwrap_or_default();
        match base {
            "mono" => return 1.0,
            "stereo" | "downmix" => return 2.0,
            "quad" => return 4.0,
            _ => {}
        }
        if let Some((main, lfe)) = base.split_once('.')
            && let (Ok(m), Ok(l)) = (main.parse::<u32>(), lfe.parse::<u32>())
        {
            return m as f32 + l as f32 / 10.0;
        }
    }
    match count {
        0 => 0.0,
        1 | 2 => count as f32,
        n => (n - 1) as f32 + 0.1,
    }
}

/// Map an ffprobe language tag (ISO 639-2 B/T, or 639-1) to a [`Language`].
pub fn language_from_tag(tag: &str) -> Language {
    let t = tag.trim().to_ascii_lowercase();
    let t = t.split(['-', '_']).next().unwrap_or_default();
    if t.len() == 2
        && let Some(l) = Language::from_iso639_1(t)
    {
        return l;
    }
    use Language as L;
    match t {
        "eng" => L::English,
        "fre" | "fra" => L::French,
        "spa" => L::Spanish,
        "ger" | "deu" => L::German,
        "ita" => L::Italian,
        "dan" => L::Danish,
        "dut" | "nld" => L::Dutch,
        "jpn" => L::Japanese,
        "ice" | "isl" => L::Icelandic,
        "chi" | "zho" | "cmn" | "yue" => L::Chinese,
        "rus" => L::Russian,
        "pol" => L::Polish,
        "vie" => L::Vietnamese,
        "swe" => L::Swedish,
        "nor" | "nob" | "nno" => L::Norwegian,
        "fin" => L::Finnish,
        "tur" => L::Turkish,
        "por" => L::Portuguese,
        "gre" | "ell" => L::Greek,
        "kor" => L::Korean,
        "hun" => L::Hungarian,
        "heb" => L::Hebrew,
        "lit" => L::Lithuanian,
        "cze" | "ces" => L::Czech,
        "hin" => L::Hindi,
        "rum" | "ron" => L::Romanian,
        "tha" => L::Thai,
        "bul" => L::Bulgarian,
        "ara" => L::Arabic,
        "ukr" => L::Ukrainian,
        "per" | "fas" => L::Persian,
        "ben" => L::Bengali,
        "slo" | "slk" => L::Slovak,
        "lav" => L::Latvian,
        "cat" => L::Catalan,
        "hrv" => L::Croatian,
        "srp" => L::Serbian,
        "ind" => L::Indonesian,
        "tam" => L::Tamil,
        "tel" => L::Telugu,
        "may" | "msa" => L::Malay,
        _ => L::Unknown,
    }
}

/// Serde shapes of the subset of ffprobe output we read.
mod raw {
    use std::collections::HashMap;

    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub struct Probe {
        #[serde(default)]
        pub streams: Vec<Stream>,
        pub format: Option<Format>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct Format {
        pub format_name: Option<String>,
        pub duration: Option<serde_json::Value>,
        pub bit_rate: Option<serde_json::Value>,
    }

    #[derive(Debug, Deserialize)]
    pub struct Stream {
        pub codec_name: Option<String>,
        pub codec_type: Option<String>,
        pub codec_tag_string: Option<String>,
        pub profile: Option<String>,
        pub width: Option<u32>,
        pub height: Option<u32>,
        pub pix_fmt: Option<String>,
        pub bits_per_raw_sample: Option<String>,
        pub color_transfer: Option<String>,
        pub color_primaries: Option<String>,
        pub r_frame_rate: Option<String>,
        pub avg_frame_rate: Option<String>,
        pub channels: Option<u32>,
        pub channel_layout: Option<String>,
        pub duration: Option<serde_json::Value>,
        pub bit_rate: Option<serde_json::Value>,
        #[serde(default)]
        pub side_data_list: Vec<SideData>,
        #[serde(default)]
        pub tags: HashMap<String, String>,
        #[serde(default)]
        pub disposition: Disposition,
    }

    impl Stream {
        /// Case-insensitive tag lookup (mkv uses `language`, some muxers `LANGUAGE`).
        pub fn tag(&self, key: &str) -> Option<&str> {
            self.tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
                .filter(|v| !v.is_empty())
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct SideData {
        pub side_data_type: Option<String>,
        pub dv_profile: Option<u32>,
        pub dv_bl_signal_compatibility_id: Option<u32>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct Disposition(#[serde(default)] pub HashMap<String, i64>);

    impl Disposition {
        pub fn flag(&self, key: &str) -> bool {
            self.0.get(key).copied().unwrap_or(0) != 0
        }
        pub fn attached_pic(&self) -> bool {
            self.flag("attached_pic")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_buckets() {
        assert_eq!(resolution_from_dimensions(3840, 1600), Resolution::R2160p);
        assert_eq!(resolution_from_dimensions(1920, 800), Resolution::R1080p);
        assert_eq!(resolution_from_dimensions(1280, 534), Resolution::R720p);
        assert_eq!(resolution_from_dimensions(720, 576), Resolution::R576p);
        assert_eq!(resolution_from_dimensions(720, 480), Resolution::R480p);
        assert_eq!(resolution_from_dimensions(480, 360), Resolution::R360p);
        assert_eq!(resolution_from_dimensions(0, 0), Resolution::Unknown);
    }

    #[test]
    fn channel_layouts() {
        assert_eq!(channels_display(Some("5.1(side)"), 6), 5.1);
        assert_eq!(channels_display(Some("7.1"), 8), 7.1);
        assert_eq!(channels_display(Some("stereo"), 2), 2.0);
        assert_eq!(channels_display(None, 6), 5.1);
        assert_eq!(channels_display(Some("weird"), 1), 1.0);
    }

    #[test]
    fn language_tags() {
        assert_eq!(language_from_tag("eng"), Language::English);
        assert_eq!(language_from_tag("ger"), Language::German);
        assert_eq!(language_from_tag("deu"), Language::German);
        assert_eq!(language_from_tag("fr"), Language::French);
        assert_eq!(language_from_tag("pt-BR"), Language::Portuguese);
        assert_eq!(language_from_tag("und"), Language::Unknown);
    }

    #[test]
    fn empty_streams_is_error() {
        assert!(matches!(
            MediaInfo::from_ffprobe_json(br#"{"streams":[],"format":{}}"#),
            Err(Error::NoStreams)
        ));
        assert!(MediaInfo::from_ffprobe_json(b"not json").is_err());
    }

    #[tokio::test]
    async fn missing_ffprobe_binary_is_spawn_error() {
        let cfg = ProbeConfig {
            ffprobe_path: PathBuf::from("/definitely/not/here/ffprobe"),
            timeout: Duration::from_secs(5),
        };
        let err = probe(Path::new("x.mkv"), &cfg).await.unwrap_err();
        assert!(matches!(err, Error::FfprobeSpawn { .. }), "{err}");
    }
}
