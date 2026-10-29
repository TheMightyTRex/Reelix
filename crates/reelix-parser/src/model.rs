//! Quality, language and parsed-release types. Persisted ids (`Quality::id`, `Language::id`)
//! are stable and must never be renumbered.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Unknown,
    Workprint,
    Cam,
    Telesync,
    Telecine,
    Regional,
    Dvdscr,
    Tv,
    Dvd,
    WebRip,
    WebDl,
    Bluray,
    Remux,
    BrDisk,
    RawHd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Resolution {
    Unknown,
    #[serde(rename = "360p")]
    R360p,
    #[serde(rename = "480p")]
    R480p,
    #[serde(rename = "540p")]
    R540p,
    #[serde(rename = "576p")]
    R576p,
    #[serde(rename = "720p")]
    R720p,
    #[serde(rename = "1080p")]
    R1080p,
    #[serde(rename = "2160p")]
    R2160p,
}

impl Resolution {
    pub fn height(self) -> u32 {
        match self {
            Self::Unknown => 0,
            Self::R360p => 360,
            Self::R480p => 480,
            Self::R540p => 540,
            Self::R576p => 576,
            Self::R720p => 720,
            Self::R1080p => 1080,
            Self::R2160p => 2160,
        }
    }
}

/// Which kind of media a quality describes. Stored in `quality_profiles.media_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QualityFamily {
    #[default]
    Video,
    Ebook,
    Audiobook,
    Music,
}

impl QualityFamily {
    pub const ALL: [QualityFamily; 4] = [Self::Video, Self::Ebook, Self::Audiobook, Self::Music];

    /// `video` | `ebook` | `audiobook` | `music` (DB column values).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Ebook => "ebook",
            Self::Audiobook => "audiobook",
            Self::Music => "music",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// The family's qualities in default ranking order (worst → best).
    pub fn qualities(self) -> &'static [Quality] {
        match self {
            Self::Video => Quality::ALL,
            Self::Ebook => Quality::EBOOK,
            Self::Audiobook => Quality::AUDIOBOOK,
            Self::Music => Quality::MUSIC,
        }
    }

    /// Movie/Series → Video, Ebook → Ebook, Audiobook → Audiobook, Music → Music.
    pub fn of_content(k: reelix_core::ContentKind) -> Self {
        use reelix_core::ContentKind as K;
        match k {
            K::Movie | K::Series => Self::Video,
            K::Ebook => Self::Ebook,
            K::Audiobook => Self::Audiobook,
            K::Music => Self::Music,
        }
    }

    pub fn of_book(f: reelix_core::BookFormat) -> Self {
        match f {
            reelix_core::BookFormat::Ebook => Self::Ebook,
            reelix_core::BookFormat::Audiobook => Self::Audiobook,
        }
    }
}

// Rows are grouped by family; each group lists its qualities worst → best.
macro_rules! qualities {
    (
        video { $( $v:ident = $vid:literal, $vname:literal, $vsource:ident, $vres:ident; )* }
        $( $family:ident / $konst:ident { $( $variant:ident = $id:literal, $name:literal; )* } )*
    ) => {
        /// A named quality, as used in quality profiles and file records.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Quality {
            $( #[serde(rename = $vname)] $v, )*
            $( $( #[serde(rename = $name)] $variant, )* )*
        }

        impl Quality {
            /// Every video quality in default ranking order (worst → best).
            pub const ALL: &'static [Quality] = &[ $( Quality::$v, )* ];
            $(
                #[doc = concat!("Every ", stringify!($family), " quality in default ranking order (worst → best).")]
                pub const $konst: &'static [Quality] = &[ $( Quality::$variant, )* ];
            )*
            /// Every quality of every family, in declaration order.
            pub const EVERY: &'static [Quality] = &[ $( Quality::$v, )* $( $( Quality::$variant, )* )* ];

            /// Stable persisted id.
            pub fn id(self) -> u16 {
                match self { $( Self::$v => $vid, )* $( $( Self::$variant => $id, )* )* }
            }
            pub fn from_id(id: u16) -> Option<Self> {
                match id { $( $vid => Some(Self::$v), )* $( $( $id => Some(Self::$variant), )* )* _ => None }
            }
            /// Display name, e.g. `WEBDL-1080p`.
            pub fn name(self) -> &'static str {
                match self { $( Self::$v => $vname, )* $( $( Self::$variant => $name, )* )* }
            }
            pub fn from_name(name: &str) -> Option<Self> {
                Self::EVERY.iter().copied().find(|q| q.name().eq_ignore_ascii_case(name))
            }
            /// Non-video qualities report `Source::Unknown`.
            pub fn source(self) -> Source {
                match self { $( Self::$v => Source::$vsource, )* _ => Source::Unknown }
            }
            /// Non-video qualities report `Resolution::Unknown`.
            pub fn resolution(self) -> Resolution {
                match self { $( Self::$v => Resolution::$vres, )* _ => Resolution::Unknown }
            }
            pub fn family(self) -> QualityFamily {
                match self {
                    $( Self::$v => QualityFamily::Video, )*
                    $( $( Self::$variant => QualityFamily::$family, )* )*
                }
            }
        }
    };
}

qualities! {
    video {
        Unknown     = 0,  "Unknown",      Unknown,   Unknown;
        Workprint   = 1,  "WORKPRINT",    Workprint, Unknown;
        Cam         = 2,  "CAM",          Cam,       Unknown;
        Telesync    = 3,  "TELESYNC",     Telesync,  Unknown;
        Telecine    = 4,  "TELECINE",     Telecine,  Unknown;
        Regional    = 5,  "REGIONAL",     Regional,  R480p;
        Dvdscr      = 6,  "DVDSCR",       Dvdscr,    R480p;
        Sdtv        = 7,  "SDTV",         Tv,        R480p;
        Dvd         = 8,  "DVD",          Dvd,       Unknown;
        DvdR        = 9,  "DVD-R",        Dvd,       R480p;
        WebDl480p   = 10, "WEBDL-480p",   WebDl,     R480p;
        WebRip480p  = 11, "WEBRip-480p",  WebRip,    R480p;
        Bluray480p  = 12, "Bluray-480p",  Bluray,    R480p;
        Bluray576p  = 13, "Bluray-576p",  Bluray,    R576p;
        Hdtv720p    = 14, "HDTV-720p",    Tv,        R720p;
        WebDl720p   = 15, "WEBDL-720p",   WebDl,     R720p;
        WebRip720p  = 16, "WEBRip-720p",  WebRip,    R720p;
        Bluray720p  = 17, "Bluray-720p",  Bluray,    R720p;
        Hdtv1080p   = 18, "HDTV-1080p",   Tv,        R1080p;
        WebDl1080p  = 19, "WEBDL-1080p",  WebDl,     R1080p;
        WebRip1080p = 20, "WEBRip-1080p", WebRip,    R1080p;
        Bluray1080p = 21, "Bluray-1080p", Bluray,    R1080p;
        Remux1080p  = 22, "Remux-1080p",  Remux,     R1080p;
        Hdtv2160p   = 23, "HDTV-2160p",   Tv,        R2160p;
        WebDl2160p  = 24, "WEBDL-2160p",  WebDl,     R2160p;
        WebRip2160p = 25, "WEBRip-2160p", WebRip,    R2160p;
        Bluray2160p = 26, "Bluray-2160p", Bluray,    R2160p;
        Remux2160p  = 27, "Remux-2160p",  Remux,     R2160p;
        BrDisk      = 28, "BR-DISK",      BrDisk,    R1080p;
        RawHd       = 29, "Raw-HD",       RawHd,     R1080p;
    }
    Ebook / EBOOK {
        UnknownText = 100, "Unknown Text";
        Pdf         = 101, "PDF";
        Mobi        = 102, "MOBI";
        Azw3        = 103, "AZW3";
        Epub        = 104, "EPUB";
    }
    Audiobook / AUDIOBOOK {
        UnknownAudiobook = 120, "Unknown Audiobook";
        AudiobookMp3     = 121, "Audiobook MP3";
        AudiobookM4b     = 122, "Audiobook M4B";
        AudiobookFlac    = 123, "Audiobook FLAC";
    }
    Music / MUSIC {
        UnknownMusic = 140, "Unknown Music";
        Wma          = 141, "WMA";
        Mp3_128      = 142, "MP3-128";
        Mp3_192      = 143, "MP3-192";
        AacVbr       = 144, "AAC-VBR";
        Vorbis       = 145, "OGG Vorbis";
        Opus         = 146, "Opus";
        Mp3VbrV2     = 147, "MP3-VBR-V2";
        Mp3_256      = 148, "MP3-256";
        Aac256       = 149, "AAC-256";
        Mp3VbrV0     = 150, "MP3-VBR-V0";
        Mp3_320      = 151, "MP3-320";
        Aac320       = 152, "AAC-320";
        Wav          = 153, "WAV";
        Alac         = 154, "ALAC";
        Flac         = 155, "FLAC";
        Alac24       = 156, "ALAC 24bit";
        Flac24       = 157, "FLAC 24bit";
    }
}

impl Quality {
    /// Best-matching named quality for a source + resolution pair.
    pub fn from_parts(source: Source, resolution: Resolution) -> Self {
        use Resolution as R;
        use Source as S;
        match (source, resolution) {
            (S::Workprint, _) => Self::Workprint,
            (S::Cam, _) => Self::Cam,
            (S::Telesync, _) => Self::Telesync,
            (S::Telecine, _) => Self::Telecine,
            (S::Regional, _) => Self::Regional,
            (S::Dvdscr, _) => Self::Dvdscr,
            (S::BrDisk, _) => Self::BrDisk,
            (S::RawHd, _) => Self::RawHd,
            (S::Dvd, _) => Self::Dvd,
            (S::Tv, R::R2160p) => Self::Hdtv2160p,
            (S::Tv, R::R1080p) => Self::Hdtv1080p,
            (S::Tv, R::R720p) => Self::Hdtv720p,
            (S::Tv, _) => Self::Sdtv,
            (S::WebDl, R::R2160p) => Self::WebDl2160p,
            (S::WebDl, R::R1080p) => Self::WebDl1080p,
            (S::WebDl, R::R720p) => Self::WebDl720p,
            (S::WebDl, _) => Self::WebDl480p,
            (S::WebRip, R::R2160p) => Self::WebRip2160p,
            (S::WebRip, R::R1080p) => Self::WebRip1080p,
            (S::WebRip, R::R720p) => Self::WebRip720p,
            (S::WebRip, _) => Self::WebRip480p,
            (S::Bluray, R::R2160p) => Self::Bluray2160p,
            (S::Bluray, R::R1080p) => Self::Bluray1080p,
            (S::Bluray, R::R720p) => Self::Bluray720p,
            (S::Bluray, R::R576p) => Self::Bluray576p,
            (S::Bluray, _) => Self::Bluray480p,
            (S::Remux, R::R2160p) => Self::Remux2160p,
            (S::Remux, _) => Self::Remux1080p,
            (S::Unknown, R::R2160p) => Self::WebDl2160p,
            (S::Unknown, R::R1080p) => Self::WebDl1080p,
            (S::Unknown, R::R720p) => Self::WebDl720p,
            (S::Unknown, R::R480p | R::R540p | R::R576p | R::R360p) => Self::WebDl480p,
            (S::Unknown, R::Unknown) => Self::Unknown,
        }
    }
}

/// PROPER/REPACK/REAL/vN information. Higher revisions are upgrades within the same quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Revision {
    /// 1 for the original release; 2+ for PROPER/REPACK/v2…
    pub version: u8,
    /// Number of REAL tags.
    pub real: u8,
    pub is_repack: bool,
}

impl Default for Revision {
    fn default() -> Self {
        Self {
            version: 1,
            real: 0,
            is_repack: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualityModel {
    pub quality: Quality,
    #[serde(default)]
    pub revision: Revision,
}

impl QualityModel {
    pub fn new(quality: Quality) -> Self {
        Self {
            quality,
            revision: Revision::default(),
        }
    }
}

macro_rules! languages {
    ($( $variant:ident = $id:literal, $name:literal, $iso:literal; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Language { $( #[serde(rename = $name)] $variant, )* }

        impl Language {
            pub const ALL: &'static [Language] = &[ $( Language::$variant, )* ];
            pub fn id(self) -> u16 { match self { $( Self::$variant => $id, )* } }
            pub fn from_id(id: u16) -> Option<Self> { match id { $( $id => Some(Self::$variant), )* _ => None } }
            pub fn name(self) -> &'static str { match self { $( Self::$variant => $name, )* } }
            /// ISO 639-1 code (`""` for Unknown/Original).
            pub fn iso639_1(self) -> &'static str { match self { $( Self::$variant => $iso, )* } }
            pub fn from_iso639_1(code: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|l| !l.iso639_1().is_empty() && l.iso639_1().eq_ignore_ascii_case(code))
            }
        }
    };
}

languages! {
    Unknown = 0, "Unknown", "";
    English = 1, "English", "en";
    French = 2, "French", "fr";
    Spanish = 3, "Spanish", "es";
    German = 4, "German", "de";
    Italian = 5, "Italian", "it";
    Danish = 6, "Danish", "da";
    Dutch = 7, "Dutch", "nl";
    Japanese = 8, "Japanese", "ja";
    Icelandic = 9, "Icelandic", "is";
    Chinese = 10, "Chinese", "zh";
    Russian = 11, "Russian", "ru";
    Polish = 12, "Polish", "pl";
    Vietnamese = 13, "Vietnamese", "vi";
    Swedish = 14, "Swedish", "sv";
    Norwegian = 15, "Norwegian", "no";
    Finnish = 16, "Finnish", "fi";
    Turkish = 17, "Turkish", "tr";
    Portuguese = 18, "Portuguese", "pt";
    Flemish = 19, "Flemish", "";
    Greek = 20, "Greek", "el";
    Korean = 21, "Korean", "ko";
    Hungarian = 22, "Hungarian", "hu";
    Hebrew = 23, "Hebrew", "he";
    Lithuanian = 24, "Lithuanian", "lt";
    Czech = 25, "Czech", "cs";
    Hindi = 26, "Hindi", "hi";
    Romanian = 27, "Romanian", "ro";
    Thai = 28, "Thai", "th";
    Bulgarian = 29, "Bulgarian", "bg";
    PortugueseBr = 30, "Portuguese (Brazil)", "";
    Arabic = 31, "Arabic", "ar";
    Ukrainian = 32, "Ukrainian", "uk";
    Persian = 33, "Persian", "fa";
    Bengali = 34, "Bengali", "bn";
    Slovak = 35, "Slovak", "sk";
    Latvian = 36, "Latvian", "lv";
    SpanishLatino = 37, "Spanish (Latino)", "";
    Catalan = 38, "Catalan", "ca";
    Croatian = 39, "Croatian", "hr";
    Serbian = 40, "Serbian", "sr";
    Indonesian = 41, "Indonesian", "id";
    Tamil = 42, "Tamil", "ta";
    Telugu = 43, "Telugu", "te";
    Malay = 44, "Malay", "ms";
    // Original = the media's original language (profile setting only, never parsed).
    Original = 1000, "Original", "";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrFormat {
    DolbyVision,
    Hdr10,
    Hdr10Plus,
    Hlg,
    Pq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
    Vp9,
    Vc1,
    Mpeg2,
    Xvid,
    Divx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    Aac,
    Ac3,
    Eac3,
    Dts,
    DtsHdMa,
    DtsX,
    TrueHd,
    Atmos,
    Flac,
    Mp3,
    Opus,
    Pcm,
}

/// Season/episode information for TV releases.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EpisodeInfo {
    /// `None` for absolute-only (anime) and daily releases.
    pub season: Option<u32>,
    pub episodes: Vec<u32>,
    pub absolute_episodes: Vec<u32>,
    pub air_date: Option<NaiveDate>,
    /// A complete-season pack (no episode numbers).
    pub full_season: bool,
    /// Multi-season pack, inclusive range (e.g. S01-S03).
    pub season_range: Option<(u32, u32)>,
    pub is_special: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReleaseKind {
    Movie,
    Episode(EpisodeInfo),
    Unknown,
}

/// Everything the parser extracts from a release title or file name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedRelease {
    pub raw_title: String,
    /// Cleaned media title (movie or series name), e.g. `The Show Name`.
    pub title: String,
    pub year: Option<u16>,
    pub kind: ReleaseKind,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    /// Edition tags, e.g. `Director's Cut`, `Extended`, `IMAX`.
    pub edition: Option<String>,
    pub hdr: Vec<HdrFormat>,
    pub video_codec: Option<VideoCodec>,
    pub audio_codecs: Vec<AudioCodec>,
    /// e.g. 5.1, 7.1, 2.0
    pub audio_channels: Option<f32>,
    pub is_3d: bool,
    pub is_hardcoded_subs: bool,
    /// Torrent info hash or similar, when present in the name.
    pub hash: Option<String>,
    /// Streaming service tag, e.g. `NF`, `AMZN`, `DSNP`.
    pub streaming_service: Option<String>,
}

impl ParsedRelease {
    /// An empty result for `raw_title` (Unknown quality, no metadata).
    pub fn empty(raw_title: &str) -> Self {
        Self {
            raw_title: raw_title.to_string(),
            title: String::new(),
            year: None,
            kind: ReleaseKind::Unknown,
            quality: QualityModel::new(Quality::Unknown),
            languages: Vec::new(),
            release_group: None,
            edition: None,
            hdr: Vec::new(),
            video_codec: None,
            audio_codecs: Vec::new(),
            audio_channels: None,
            is_3d: false,
            is_hardcoded_subs: false,
            hash: None,
            streaming_service: None,
        }
    }

    pub fn episode(&self) -> Option<&EpisodeInfo> {
        match &self.kind {
            ReleaseKind::Episode(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_ids_roundtrip_and_are_unique() {
        for all in [Quality::ALL, Quality::EVERY] {
            let mut ids: Vec<u16> = all.iter().map(|q| q.id()).collect();
            for q in all {
                assert_eq!(Quality::from_id(q.id()), Some(*q));
                assert_eq!(Quality::from_name(q.name()), Some(*q));
            }
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), all.len());
        }
    }

    #[test]
    fn families_partition_every_quality() {
        assert_eq!(Quality::ALL.len(), 30);
        let total: usize = QualityFamily::ALL.iter().map(|f| f.qualities().len()).sum();
        assert_eq!(total, Quality::EVERY.len());
        for f in QualityFamily::ALL {
            assert_eq!(QualityFamily::parse(f.as_str()), Some(f));
            for q in f.qualities() {
                assert_eq!(q.family(), f, "{q:?}");
            }
        }
        assert_eq!(Quality::Flac24.id(), 157);
        assert_eq!(Quality::Epub.source(), Source::Unknown);
        assert_eq!(
            serde_json::to_string(&Quality::Mp3VbrV0).unwrap(),
            "\"MP3-VBR-V0\""
        );
        assert_eq!(
            QualityFamily::of_content(reelix_core::ContentKind::Series),
            QualityFamily::Video
        );
        assert_eq!(
            QualityFamily::of_book(reelix_core::BookFormat::Audiobook),
            QualityFamily::Audiobook
        );
    }

    #[test]
    fn from_parts_matches_named_quality() {
        for q in Quality::ALL {
            // DVD-R (a full DVD image) shares source + resolution with DVD.
            if q.resolution() != Resolution::Unknown && q.source() != Source::Unknown && *q != Quality::DvdR {
                assert_eq!(Quality::from_parts(q.source(), q.resolution()), *q, "{q:?}");
            }
        }
    }

    #[test]
    fn serde_uses_display_names() {
        assert_eq!(
            serde_json::to_string(&Quality::WebDl1080p).unwrap(),
            "\"WEBDL-1080p\""
        );
        assert_eq!(
            serde_json::to_string(&Language::PortugueseBr).unwrap(),
            "\"Portuguese (Brazil)\""
        );
    }
}
