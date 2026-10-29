//! Technical tags found after the title: quality, revision, languages, edition, HDR,
//! codecs, audio, streaming service, 3D and hardcoded subtitles.

use regex::Regex;

use crate::model::{AudioCodec, HdrFormat, Language, Quality, Resolution, Revision, Source, VideoCodec};

// ---------- resolution ----------

re!(
    RESOLUTION,
    r"(?i)\b(?:(2160|1080|720|576|540|480|360)[pi]|(4K|UHD)|(FHD)|(\d{3,4})x(\d{3,4}))\b"
);

pub(crate) fn resolution(s: &str) -> Resolution {
    for c in RESOLUTION.captures_iter(s) {
        if let Some(p) = c.get(1) {
            return match p.as_str() {
                "2160" => Resolution::R2160p,
                "1080" => Resolution::R1080p,
                "720" => Resolution::R720p,
                "576" => Resolution::R576p,
                "540" => Resolution::R540p,
                "480" => Resolution::R480p,
                _ => Resolution::R360p,
            };
        }
        if c.get(2).is_some() {
            return Resolution::R2160p;
        }
        if c.get(3).is_some() {
            return Resolution::R1080p;
        }
        let w: u32 = c.get(4).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let h: u32 = c.get(5).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let r = if w >= 3200 || h >= 1600 {
            Resolution::R2160p
        } else if w >= 1800 || h >= 1000 {
            Resolution::R1080p
        } else if w >= 1200 || h >= 700 {
            Resolution::R720p
        } else if h >= 560 {
            Resolution::R576p
        } else if h >= 400 {
            Resolution::R480p
        } else {
            continue;
        };
        return r;
    }
    Resolution::Unknown
}

// ---------- source ----------

re!(
    BRDISK_TAG,
    r"(?i)\b(?:COMPLETE[ .-]?(?:UHD[ .-]?)?BLU-?RAY|BD25|BD50|BD66|BD100|BDMV|BD-?ISO|BR-?DISK)\b"
);
re!(BLURAY_HYPHEN, r"(?i)\bBlu-Ray\b");
re!(DISC_CODEC, r"(?i)\b(?:AVC|VC-?1|MPEG-?2)\b");
re!(DISC_CODEC_HEVC, r"(?i)\bHEVC\b");
re!(
    ENCODE,
    r"(?i)\b(?:x[ .]?26[45]|h[ .]?26[45]|XviD|DivX|[a-z]*Rip|Remux|Encode)\b"
);
re!(REMUX, r"(?i)\b(?:BD|UHD|BluRay)?[ .-]?REMUX\b");
re!(RAWHD, r"(?i)\bRaw[ .-]?HD\b");
re!(INTERLACED_1080, r"(?i)\b1080i\b");
re!(MPEG2, r"(?i)\bMPEG-?2\b");
re!(WORKPRINT, r"(?i)\b(?:WORKPRINT|WP)\b");
re!(CAM, r"(?i)\b(?:CAM|CAMRip|CAM-Rip|HDCAM|HD-CAM|HQCAM|NewCAM)\b");
re!(
    TELESYNC,
    r"(?i)\b(?:TS|TELESYNC|HDTS|HD-TS|HQ-?TS|PDVD|TSRip|PreDVDRip)\b"
);
re!(TELECINE, r"(?i)\b(?:TC|TELECINE|HDTC|HD-TC)\b");
re!(
    SCREENER,
    r"(?i)\b(?:DVDSCR|DVD-SCR|SCR|SCREENER|DVDSCREENER|BDSCR|WEBSCR|WEBSCREENER)\b"
);
re!(REGIONAL, r"(?i)\b(?:R5|R6|REGIONAL)\b");
re!(
    BLURAY,
    r"(?i)\b(?:Blu-?Ray|BDRip|BD-Rip|BRRip|BR-Rip|BDMux|BRMux|BD|UHD-?BD|HD-?DVD|HDDVDRip)\b"
);
re!(WEBRIP, r"(?i)\b(?:WEB-?Rip|WEB-?Cap|WEBMux|HDRip)\b");
re!(
    WEBDL,
    r"(?i)\b(?:WEB-?DL|WEBDL|WEB-?HD|WEB|iTunesHD|AmazonHD|NetflixHD|NetflixUHD)\b"
);
re!(
    HDTV,
    r"(?i)\b(?:HDTV|HD-TV|UHDTV|PDTV|SDTV|TVRip|TV-Rip|HDTVRip|DSR|DSRip|SATRip|DTHRip|DVBRip|DTVRip)\b"
);
re!(DVDR, r"(?i)\b(?:DVD-?R|DVD-?5|DVD-?9|DVDR)\b");
re!(DVD, r"(?i)\b(?:DVDRip|DVD-Rip|DVD|DVDMux|NTSC|PAL|XviDVD)\b");
re!(XVID, r"(?i)\b(?:XviD|DivX)\b");

/// Determines the named quality of a release from its tag section.
pub(crate) fn quality(tail: &str, has_service: bool) -> Quality {
    let res = resolution(tail);
    let is = |re: &Regex| re.is_match(tail);

    let bluray = is(&BLURAY);
    let encode = is(&ENCODE);
    if (is(&BRDISK_TAG) && !encode)
        || (bluray && !encode && (is(&DISC_CODEC) || (is(&BLURAY_HYPHEN) && is(&DISC_CODEC_HEVC))))
    {
        return Quality::BrDisk;
    }
    if is(&REMUX) {
        return Quality::from_parts(Source::Remux, res);
    }
    if is(&RAWHD) || (is(&INTERLACED_1080) && is(&MPEG2) && is(&HDTV)) {
        return Quality::RawHd;
    }
    let source = if is(&WORKPRINT) {
        Source::Workprint
    } else if is(&CAM) {
        Source::Cam
    } else if is(&TELESYNC) {
        Source::Telesync
    } else if is(&TELECINE) {
        Source::Telecine
    } else if is(&SCREENER) {
        Source::Dvdscr
    } else if is(&REGIONAL) {
        Source::Regional
    } else if bluray {
        Source::Bluray
    } else if is(&WEBRIP) {
        Source::WebRip
    } else if is(&WEBDL) {
        Source::WebDl
    } else if is(&HDTV) {
        Source::Tv
    } else if is(&DVDR) {
        return Quality::DvdR;
    } else if is(&DVD) {
        Source::Dvd
    } else if has_service {
        Source::WebDl
    } else if res == Resolution::Unknown && is(&XVID) {
        Source::Tv
    } else {
        Source::Unknown
    };
    Quality::from_parts(source, res)
}

// ---------- revision ----------

re!(PROPER, r"(?i)\b(?:PROPER|REPACK|RERIP)(\d)?\b");
re!(REPACK, r"(?i)\b(?:REPACK|RERIP)\d?\b");
re!(VERSION, r"(?i)(?:\b|\d)v([2-9])\b");
re!(REAL, r"\bREAL\b");

pub(crate) fn revision(tail: &str) -> Revision {
    let mut version: u8 = 1;
    for c in PROPER.captures_iter(tail) {
        let n = c.get(1).and_then(|m| m.as_str().parse::<u8>().ok()).unwrap_or(1);
        version = version.max(n.saturating_add(1));
    }
    for c in VERSION.captures_iter(tail) {
        if let Some(n) = c.get(1).and_then(|m| m.as_str().parse::<u8>().ok()) {
            version = version.max(n);
        }
    }
    Revision {
        version,
        real: REAL.find_iter(tail).count().min(u8::MAX as usize) as u8,
        is_repack: REPACK.is_match(tail),
    }
}

// ---------- languages ----------

/// (language, pattern). Patterns are matched case-insensitively as whole words.
const LANGUAGE_PATTERNS: &[(Language, &str)] = &[
    (Language::English, r"ENGLISH|ENG"),
    (
        Language::French,
        r"FRENCH|TRUEFRENCH|VFF|VFQ|VFI|VF2|VF|VOF|VOSTFR|SUBFRENCH|FRA|FR",
    ),
    (Language::German, r"GERMAN|DEUTSCH|GER"),
    (Language::SpanishLatino, r"LATINO|LATAM|LAT"),
    (Language::Spanish, r"SPANISH|ESPANOL|ESP|CASTELLANO|SPA"),
    (Language::Italian, r"ITALIAN|ITA"),
    (Language::Danish, r"DANISH|DAN"),
    (Language::Dutch, r"DUTCH|NL"),
    (Language::Japanese, r"JAPANESE|JAP|JPN"),
    (Language::Icelandic, r"ICELANDIC"),
    (Language::Chinese, r"CHINESE|MANDARIN|CANTONESE|CHI|CHS|CHT"),
    (Language::Russian, r"RUSSIAN|RUS"),
    (Language::Polish, r"POLISH|PL|PLDUB|LEKTOR"),
    (Language::Vietnamese, r"VIETNAMESE|VIE"),
    (Language::Swedish, r"SWEDISH|SWE"),
    (Language::Norwegian, r"NORWEGIAN|NORSK|NOR"),
    (Language::Finnish, r"FINNISH|FIN"),
    (Language::Turkish, r"TURKISH|TUR"),
    (Language::PortugueseBr, r"PT-BR|PTBR|BRAZILIAN|DUBLADO|NACIONAL"),
    (Language::Portuguese, r"PORTUGUESE|POR"),
    (Language::Flemish, r"FLEMISH"),
    (Language::Greek, r"GREEK|GRE"),
    (Language::Korean, r"KOREAN|KOR"),
    (Language::Hungarian, r"HUNGARIAN|HUN"),
    (Language::Hebrew, r"HEBREW|HEB"),
    (Language::Lithuanian, r"LITHUANIAN|LIT"),
    (Language::Czech, r"CZECH|CZE|CZ"),
    (Language::Hindi, r"HINDI|HIN"),
    (Language::Romanian, r"ROMANIAN|RUM|RON"),
    (Language::Thai, r"THAI"),
    (Language::Bulgarian, r"BULGARIAN|BUL"),
    (Language::Arabic, r"ARABIC|ARA"),
    (Language::Ukrainian, r"UKRAINIAN|UKR"),
    (Language::Persian, r"PERSIAN|FARSI"),
    (Language::Bengali, r"BENGALI"),
    (Language::Slovak, r"SLOVAK"),
    (Language::Latvian, r"LATVIAN"),
    (Language::Catalan, r"CATALAN"),
    (Language::Croatian, r"CROATIAN|HRV"),
    (Language::Serbian, r"SERBIAN|SRB"),
    (Language::Indonesian, r"INDONESIAN"),
    (Language::Tamil, r"TAMIL"),
    (Language::Telugu, r"TELUGU"),
    (Language::Malay, r"MALAY"),
];

static LANGUAGE_RES: std::sync::LazyLock<Vec<(Language, Regex)>> = std::sync::LazyLock::new(|| {
    LANGUAGE_PATTERNS
        .iter()
        .map(|(l, p)| {
            // A trailing "SUB(S)" marks a subtitle language, which does not count as audio.
            let re = Regex::new(&format!(
                r"(?i)\b(?:{p})\b(?:[ .\-]?(SUBS?|SUBBED|SUBTITLES?)\b)?"
            ))
            .expect("static regex is valid");
            (*l, re)
        })
        .collect()
});

re!(
    MULTI,
    r"(?i)\b(?:MULTi(?:[ .-]?(?:LANG|AUDIO|\d+))?|DUAL[ .-]?AUDIO|DUAL)\b"
);
re!(GERMAN_DL, r"(?i)\b(?:GERMAN|GER)[ .]DL\b");

/// Audio languages tagged in the release; `[English]` when nothing is tagged.
pub(crate) fn languages(tail: &str) -> Vec<Language> {
    let mut found: Vec<(usize, Language)> = Vec::new();
    for (lang, re) in LANGUAGE_RES.iter() {
        for c in re.captures_iter(tail) {
            let Some(m) = c.get(0) else { continue };
            let word = m.as_str().to_ascii_uppercase();
            // Subtitle-only tags (except VOSTFR, which is commonly used for French releases).
            if c.get(1).is_some() && !word.starts_with("VOST") {
                continue;
            }
            if !found.iter().any(|(_, l)| l == lang) {
                found.push((m.start(), *lang));
            }
        }
    }
    found.sort_by_key(|(pos, _)| *pos);
    let mut langs: Vec<Language> = found.into_iter().map(|(_, l)| l).collect();
    let multi = MULTI.is_match(tail) || GERMAN_DL.is_match(tail);
    if (multi || langs.is_empty()) && !langs.contains(&Language::English) {
        langs.push(Language::English);
    }
    langs
}

// ---------- edition ----------

re!(
    EDITION,
    r"(?i)\b(?:(?:\d{1,3}(?:st|nd|rd|th)[ .])?Anniversary(?:[ .](?:Edition|Cut))?|Director'?s[ .]Cut|Directors[ .]Cut|Director[ .]Cut|Extended(?:[ .](?:Cut|Edition|Version))?|Theatrical(?:[ .](?:Cut|Edition|Version))?|Ultimate[ .](?:Cut|Edition)|Final[ .]Cut|Rogue[ .]Cut|Assembly[ .]Cut|Producers?'?[ .]Cut|Unrated(?:[ .](?:Cut|Edition))?|Uncut|Uncensored|IMAX(?:[ .]Enhanced)?|Remastered|Restored|Criterion(?:[ .]Collection)?|Special[ .]Edition|Collector'?s[ .]Edition|Limited[ .]Edition|Open[ .]Matte|Despecialized|Black[ .]and[ .]Chrome|Redux|Fan[ .]Edit)\b"
);

pub(crate) fn edition(tail: &str) -> Option<String> {
    let parts: Vec<String> = EDITION
        .find_iter(tail)
        .map(|m| m.as_str().replace(['.', '_'], " "))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

// ---------- HDR ----------

re!(
    HDR,
    r"(?i)\b(?:(DV|DoVi|Dolby[ .]?Vision)\b|(HDR10(?:\+|Plus\b|P\b))|(HDR10|HDR)\b|(HLG)\b|(PQ)\b)"
);

pub(crate) fn hdr(tail: &str) -> Vec<HdrFormat> {
    let mut out = Vec::new();
    for c in HDR.captures_iter(tail) {
        let f = if c.get(1).is_some() {
            HdrFormat::DolbyVision
        } else if c.get(2).is_some() {
            HdrFormat::Hdr10Plus
        } else if c.get(3).is_some() {
            HdrFormat::Hdr10
        } else if c.get(4).is_some() {
            HdrFormat::Hlg
        } else {
            HdrFormat::Pq
        };
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

// ---------- video codec ----------

re!(
    VIDEO,
    r"(?i)\b(?:(x[ .]?264|h[ .]?264|AVC)|(x[ .]?265|h[ .]?265|HEVC)|(AV1)|(VP9)|(VC-?1)|(MPEG-?2)|(XviD)|(DivX))\b"
);

pub(crate) fn video_codec(tail: &str) -> Option<VideoCodec> {
    let c = VIDEO.captures(tail)?;
    const ORDER: [VideoCodec; 8] = [
        VideoCodec::H264,
        VideoCodec::H265,
        VideoCodec::Av1,
        VideoCodec::Vp9,
        VideoCodec::Vc1,
        VideoCodec::Mpeg2,
        VideoCodec::Xvid,
        VideoCodec::Divx,
    ];
    (1..=8).find(|i| c.get(*i).is_some()).map(|i| ORDER[i - 1])
}

// ---------- audio ----------

const CH: &str = r"(?:[ .]?([1-9])[ .]([0-2]))?";

static AUDIO_RES: std::sync::LazyLock<Vec<(AudioCodec, Regex)>> = std::sync::LazyLock::new(|| {
    let pats: &[(AudioCodec, &str)] = &[
        (AudioCodec::TrueHd, r"True-?HD"),
        (AudioCodec::Atmos, r"Atmos"),
        (
            AudioCodec::DtsHdMa,
            r"DTS[ .-]?HD[ .-]?MA|DTS[ .-]?MA|DTSHD[ .-]?MA",
        ),
        (AudioCodec::DtsX, r"DTS[ .:-]?X"),
        (AudioCodec::Dts, r"DTS(?:[ .-]?(?:HD|HRA|HR|ES))?"),
        (AudioCodec::Eac3, r"DDP|DD\+|EAC-?3|E-AC-?3|DD[ .]?Plus|DDPA"),
        (AudioCodec::Ac3, r"DD|AC-?3|Dolby[ .]?Digital"),
        (AudioCodec::Aac, r"(?:HE-?)?AAC(?:-?LC)?"),
        (AudioCodec::Flac, r"FLAC"),
        (AudioCodec::Mp3, r"MP3"),
        (AudioCodec::Opus, r"Opus"),
        (AudioCodec::Pcm, r"L?PCM"),
    ];
    pats.iter()
        .map(|(a, p)| {
            let re = Regex::new(&format!(r"(?i)\b(?:{p}){CH}(?:\b|$|[^0-9A-Za-z])"))
                .expect("static regex is valid");
            (*a, re)
        })
        .collect()
});

re!(CHANNELS, r"\b([1-9])\.([0-2])\b");
re!(CHANNELS_CH, r"(?i)\b([2-8])ch\b");

/// Audio codecs in order of appearance plus the channel layout, if tagged.
pub(crate) fn audio(tail: &str) -> (Vec<AudioCodec>, Option<f32>) {
    let mut found: Vec<(usize, AudioCodec)> = Vec::new();
    let mut channels: Option<(usize, f32)> = None;
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (codec, re) in AUDIO_RES.iter() {
        for c in re.captures_iter(tail) {
            let Some(m) = c.get(0) else { continue };
            // Longer patterns run first; skip matches inside an already-claimed span (DTS in DTS-HD MA).
            if taken.iter().any(|(s, e)| m.start() >= *s && m.start() < *e) {
                continue;
            }
            taken.push((m.start(), m.end()));
            if !found.iter().any(|(_, a)| a == codec) {
                found.push((m.start(), *codec));
            }
            if let (Some(a), Some(b)) = (c.get(1), c.get(2))
                && channels.is_none_or(|(p, _)| m.start() < p)
                && let Ok(v) = format!("{}.{}", a.as_str(), b.as_str()).parse::<f32>()
            {
                channels = Some((m.start(), v));
            }
        }
    }
    found.sort_by_key(|(p, _)| *p);
    let channels = channels.map(|(_, v)| v).or_else(|| {
        CHANNELS
            .captures(tail)
            .and_then(|c| {
                format!("{}.{}", c.get(1)?.as_str(), c.get(2)?.as_str())
                    .parse()
                    .ok()
            })
            .or_else(|| {
                CHANNELS_CH.captures(tail).and_then(|c| match c.get(1)?.as_str() {
                    "2" => Some(2.0),
                    "6" => Some(5.1),
                    "8" => Some(7.1),
                    _ => None,
                })
            })
    });
    (found.into_iter().map(|(_, a)| a).collect(), channels)
}

// ---------- streaming service ----------

// Tags that are unambiguous anywhere in the tag section (case-sensitive).
re!(
    SERVICE,
    r"\b(AMZN|NF|NFLX|DSNP|DSNY|HMAX|ATVP|HULU|Hulu|PCOK|PMTP|PMNT|STAN|CRAV|FUNI|HIDIVE|HIDI|VRV|DCU|BCORE|CRKL|ROKU|TUBI|VUDU|MUBI|iTunes|iT|iP|ALL4|ITVX|SHO|STZ|SKST|TVNZ|ZEE5|KNPY|PLUZ|USAN|TVING|WAVVE|BBC|ESPN|DSCP)\b"
);
// Short tags that only count right before a WEB source tag.
re!(
    SERVICE_BEFORE_WEB,
    r"\b(MA|MAX|CR|RED|NOW|PLAY|CC|AMC|NBC|CBC|FOX|CW|AE|ATV|YT|HBO|APTN|SNXT|MY5|JC|HS)[ .](?:WEB|WEB-?DL|WEB-?Rip|WEBRip)\b"
);

pub(crate) fn streaming_service(tail: &str) -> Option<String> {
    let first = SERVICE.captures(tail).and_then(|c| c.get(1));
    let before_web = SERVICE_BEFORE_WEB.captures(tail).and_then(|c| c.get(1));
    let m = match (first, before_web) {
        (Some(a), Some(b)) => {
            if a.start() <= b.start() {
                a
            } else {
                b
            }
        }
        (a, b) => a.or(b)?,
    };
    let canon = match m.as_str() {
        "NFLX" => "NF",
        "DSNY" => "DSNP",
        "Hulu" => "HULU",
        "iTunes" => "iT",
        "HIDI" => "HIDIVE",
        other => other,
    };
    Some(canon.to_string())
}

// ---------- flags ----------

re!(
    THREE_D,
    r"(?i)\b(?:3D|SBS|H-?SBS|Half-?SBS|Full-?SBS|HOU|H-?OU|Half-?OU|BD3D|MVC)\b"
);
re!(
    HARDCODED,
    r"(?i)\b(?:HC|HCSUBS?|HC-SUBS?|HARDSUBS?|HARD-?SUBBED|HARDCODED|KORSUBS?|KOR-SUBS?|SUBBED)\b"
);
re!(LANG_SUBS, r"(?i)\b([a-z]{2,4})SUBS?\b");

pub(crate) fn is_3d(tail: &str) -> bool {
    THREE_D.is_match(tail)
}

pub(crate) fn hardcoded_subs(tail: &str) -> bool {
    HARDCODED.is_match(tail)
        || LANG_SUBS.captures_iter(tail).any(|c| {
            let prefix = c.get(1).map_or("", |m| m.as_str()).to_ascii_lowercase();
            !matches!(prefix.as_str(), "soft" | "multi" | "vost" | "nl" | "no" | "dual")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_codecs_and_channels() {
        let (a, ch) = audio(".1080p.WEB-DL.DDP5.1.Atmos.H.264");
        assert_eq!(a, vec![AudioCodec::Eac3, AudioCodec::Atmos]);
        assert_eq!(ch, Some(5.1));
        let (a, ch) = audio(".BluRay.DTS-HD.MA.7.1.x264");
        assert_eq!(a, vec![AudioCodec::DtsHdMa]);
        assert_eq!(ch, Some(7.1));
        let (a, ch) = audio(".TrueHD.Atmos.7.1");
        assert_eq!(a, vec![AudioCodec::TrueHd, AudioCodec::Atmos]);
        assert_eq!(ch, Some(7.1));
    }

    #[test]
    fn hdr_formats() {
        assert_eq!(
            hdr(".DV.HDR10+.HEVC"),
            vec![HdrFormat::DolbyVision, HdrFormat::Hdr10Plus]
        );
        assert_eq!(hdr(".2160p.HDR.x265"), vec![HdrFormat::Hdr10]);
        assert!(hdr(".DVDRip.x264").is_empty());
    }

    #[test]
    fn languages_default_english() {
        assert_eq!(languages(".1080p.BluRay"), vec![Language::English]);
        assert_eq!(languages(".FRENCH.1080p"), vec![Language::French]);
        assert_eq!(
            languages(".MULTi.VFF.1080p"),
            vec![Language::French, Language::English]
        );
        assert_eq!(languages(".1080p.ENG.SUBS"), vec![Language::English]);
    }
}
