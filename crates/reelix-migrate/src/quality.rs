//! Maps *arr quality names onto Reelix qualities and builds equivalent profiles.

use reelix_api_types::migrate::SourceKind;
use reelix_decision::QualityProfile;
use reelix_parser::{Quality, QualityFamily};

use crate::source;

/// The Reelix quality for an *arr quality name (`None` if there's no equivalent).
pub fn map(kind: SourceKind, name: &str) -> Option<Quality> {
    let n = name.trim();
    let lower = n.to_ascii_lowercase();
    match kind {
        SourceKind::Readarr => {
            return match lower.as_str() {
                "mp3" => Some(Quality::AudiobookMp3),
                "m4b" => Some(Quality::AudiobookM4b),
                "flac" => Some(Quality::AudiobookFlac),
                "unknown audio" => Some(Quality::UnknownAudiobook),
                "unknown text" | "unknown" => Some(Quality::UnknownText),
                _ => Quality::from_name(n)
                    .filter(|q| matches!(q.family(), QualityFamily::Ebook | QualityFamily::Audiobook)),
            };
        }
        SourceKind::Lidarr => {
            return music(&lower)
                .or_else(|| Quality::from_name(n).filter(|q| q.family() == QualityFamily::Music));
        }
        _ => {}
    }
    let aliased = match lower.as_str() {
        "bluray-1080p remux" | "remux-1080p" => "Remux-1080p",
        "bluray-2160p remux" | "remux-2160p" => "Remux-2160p",
        "web 480p" | "webdl-480p" => "WEBDL-480p",
        "rawhd" | "raw-hd" => "Raw-HD",
        "brdisk" | "br-disk" => "BR-DISK",
        _ => n,
    };
    Quality::from_name(aliased).filter(|q| q.family() == QualityFamily::Video)
}

fn music(lower: &str) -> Option<Quality> {
    let bitrate = |prefix: &str| {
        lower
            .strip_prefix(prefix)
            .and_then(|r| r.trim().parse::<u32>().ok())
    };
    if let Some(b) = bitrate("mp3-") {
        return Some(match b {
            320.. => Quality::Mp3_320,
            256.. => Quality::Mp3_256,
            192.. => Quality::Mp3_192,
            _ => Quality::Mp3_128,
        });
    }
    if let Some(b) = bitrate("aac-") {
        return Some(match b {
            320.. => Quality::Aac320,
            256.. => Quality::Aac256,
            _ => Quality::AacVbr,
        });
    }
    if lower.starts_with("ogg vorbis") || lower.starts_with("vorbis") {
        return Some(Quality::Vorbis);
    }
    Some(match lower {
        "mp3-vbr-v0" | "mp3 vbr v0" => Quality::Mp3VbrV0,
        "mp3-vbr-v2" | "mp3 vbr v2" => Quality::Mp3VbrV2,
        "aac-vbr" => Quality::AacVbr,
        "wma" => Quality::Wma,
        "opus" | "ogg opus" => Quality::Opus,
        "wav" => Quality::Wav,
        "alac" => Quality::Alac,
        "alac 24bit" => Quality::Alac24,
        "flac" | "ape" | "wavpack" => Quality::Flac,
        "flac 24bit" => Quality::Flac24,
        "unknown" => Quality::UnknownMusic,
        _ => return None,
    })
}

/// The family a profile from this source belongs to (Readarr: by its qualities).
pub fn family(kind: SourceKind, p: &source::QualityProfile) -> QualityFamily {
    match kind {
        SourceKind::Lidarr => QualityFamily::Music,
        SourceKind::Readarr => {
            let mapped: Vec<Quality> = p
                .allowed_qualities()
                .iter()
                .filter_map(|n| map(kind, n))
                .collect();
            let audio = mapped
                .iter()
                .filter(|q| q.family() == QualityFamily::Audiobook)
                .count();
            if audio > 0 && audio * 2 >= mapped.len() {
                QualityFamily::Audiobook
            } else {
                QualityFamily::Ebook
            }
        }
        _ => QualityFamily::Video,
    }
}

/// A Reelix profile equivalent to `p`, plus the names of qualities with no equivalent.
pub fn convert(
    kind: SourceKind,
    p: &source::QualityProfile,
) -> Result<(QualityProfile, Vec<String>), String> {
    let family = family(kind, p);
    let mut allowed: Vec<Quality> = Vec::new();
    let mut unmapped = Vec::new();
    for name in p.allowed_qualities() {
        match map(kind, &name).filter(|q| q.family() == family) {
            Some(q) if !allowed.contains(&q) => allowed.push(q),
            Some(_) => {}
            None => unmapped.push(name),
        }
    }
    let Some(&best) = allowed.last() else {
        return Err("none of its qualities exist in Reelix".into());
    };
    let cutoff = p
        .cutoff_name()
        .and_then(|n| map(kind, &n))
        .filter(|q| allowed.contains(q))
        .unwrap_or(best);
    let mut profile = if family == QualityFamily::Video {
        QualityProfile::with_allowed(&p.name, &allowed, cutoff)
    } else {
        QualityProfile::for_family(&p.name, family, &allowed, cutoff)
    };
    profile.upgrade_allowed = p.upgrade_allowed;
    Ok((profile, unmapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_names_per_app() {
        assert_eq!(
            map(SourceKind::Sonarr, "Bluray-1080p Remux"),
            Some(Quality::Remux1080p)
        );
        assert_eq!(map(SourceKind::Radarr, "WEBDL-2160p"), Some(Quality::WebDl2160p));
        assert_eq!(map(SourceKind::Radarr, "EPUB"), None, "wrong family");
        assert_eq!(map(SourceKind::Readarr, "M4B"), Some(Quality::AudiobookM4b));
        assert_eq!(map(SourceKind::Readarr, "EPUB"), Some(Quality::Epub));
        assert_eq!(map(SourceKind::Lidarr, "MP3-224"), Some(Quality::Mp3_192));
        assert_eq!(map(SourceKind::Lidarr, "FLAC 24bit"), Some(Quality::Flac24));
        assert_eq!(map(SourceKind::Lidarr, "OGG Vorbis Q9"), Some(Quality::Vorbis));
        assert_eq!(map(SourceKind::Lidarr, "Bluray-1080p"), None);
    }
}
