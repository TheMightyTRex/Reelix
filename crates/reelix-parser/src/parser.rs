//! The release-title parser: pre-processing, title/year/episode boundaries and tag extraction.

use crate::model::{ParsedRelease, Quality, QualityModel, ReleaseKind};
use crate::{episode, group, tags, title};

re!(
    EXTENSION,
    r"(?i)\.(?:mkv|mp4|avi|m4v|wmv|mov|mpg|mpeg|m2ts|iso|nzb|torrent|rar|webm|flv|divx|vob|ogm|strm|img|par2|zip|7z|srt|sub|idx)$"
);
re!(TS_EXTENSION, r"\.ts$");
re!(
    SITE_PREFIX,
    r"(?i)^(?:\[\s*(?:www\.)?[^\]\s]+\.(?:com|org|net|to|cd|me|io|tv|cc|xyz|re|ws|info|ru|pw|biz|nl|in|lol|club|site|top|vip|st|se|mx|lt|am|ag|li|is|fr|ch|eu)\s*\]|(?:www\.)[a-z0-9.\-]+\.(?:com|org|net|to|cd|me|io|tv|cc|xyz|re|ws|info|ru|pw|biz|nl|in|lol|club|site|top|vip|st|se|mx|lt|am|ag|li|is|fr|ch|eu)|[a-z0-9\-]+\.(?:com|org|net)(?:\s+-\s+|\s*\|\s*))\s*[-|]?\s*"
);
re!(
    JUNK_SUFFIX,
    r"(?i)[-.](?:Obfuscated|Scrambled|postbot|xpost|AsRequested|AlternativeToRequested|RP|WhiteRev|BUYMORE|Rakuv\d*|Chamele0n|4P|4Planet|AlteZachen|GEROV|Z0iDS3N)$"
);
re!(TRAILING_BRACKET, r"\s*\[([^\[\]]*)\]\s*$");
re!(SUBGROUP, r"^\[([^\]]+)\]\s*[-_.]?\s*");
re!(HASH_CRC, r"[\[(]([0-9A-Fa-f]{8})[\])]");
re!(HASH_SHA1, r"\b([0-9a-fA-F]{40})\b");
re!(YEAR, r"\b((?:19|20)\d{2})\b");
re!(
    STRONG_MARKER,
    r"(?i)\b(?:\d{3,4}[pi]|4K|UHD|\d{3,4}x\d{3,4}|WEB-?DL|WEB-?Rip|WEBDL|WEBRip|Blu-?Ray|BDRip|BRRip|BD-?Remux|Remux|HDTV|PDTV|SDTV|HDRip|DVDRip|DVD-?R|DVD5|DVD9|DVDSCR|HDCAM|CAMRip|TELESYNC|TELECINE|HDTS|x26[45]|h\.?26[45]|HEVC|XviD|DivX|10bit)\b"
);
re!(
    WEAK_MARKER,
    r"(?i)\b(?:WEB|DVD|TS|TC|CAM|R5|SCR|SCREENER|PROPER|REPACK|RERIP|iNTERNAL|LIMITED|COMPLETE|MULTi|DUAL|FRENCH|TRUEFRENCH|GERMAN|ITALIAN|SPANISH|VFF|VOSTFR|DUBBED|SUBBED|EXTENDED|UNRATED|UNCUT|DIRECTORS|THEATRICAL|REMASTERED|IMAX|3D|HDR|HDR10|DV|DoVi|AMZN|NF|DSNP|HMAX|ATVP|DDP5|DD5|AAC|DTS|TrueHD|Atmos|AVC)\b|[\[(]"
);

/// Removes the file extension, website prefixes, trailing site tags and junk suffixes.
fn preprocess(raw: &str) -> String {
    let mut s = raw.trim().replace('_', " ");
    for _ in 0..3 {
        let before = s.len();
        if let Some(m) = EXTENSION.find(&s).or_else(|| TS_EXTENSION.find(&s)) {
            s.truncate(m.start());
        }
        if let Some(m) = JUNK_SUFFIX.find(&s) {
            s.truncate(m.start());
        }
        if let Some(c) = TRAILING_BRACKET.captures(&s)
            && c.get(1).is_some_and(|m| group::is_site_tag(m.as_str()))
            && let Some(m) = c.get(0)
        {
            s.truncate(m.start());
        }
        if let Some(c) = SUBGROUP.captures(&s)
            && c.get(1).is_some_and(|m| group::is_site_tag(m.as_str()))
            && let Some(m) = c.get(0)
            && m.end() < s.len()
        {
            s = s[m.end()..].to_string();
        }
        if let Some(m) = SITE_PREFIX.find(&s)
            && m.end() < s.len()
        {
            s = s[m.end()..].to_string();
        }
        s = s.trim().to_string();
        if s.len() == before {
            break;
        }
    }
    s
}

/// First match of `re` in `s` that starts after byte 0 (a title is never empty).
fn first_marker(re: &regex::Regex, s: &str) -> Option<usize> {
    re.find_iter(s).map(|m| m.start()).find(|&p| p > 0)
}

/// Parses a release title or file name. Never panics; unknown input yields
/// [`ParsedRelease::empty`]-like output with whatever could be recognised.
pub fn parse(raw: &str) -> ParsedRelease {
    let mut out = ParsedRelease::empty(raw);
    // Pathologically long inputs are not release names; cap the work.
    let limited: String = raw.chars().take(512).collect();
    let pre = preprocess(&limited);
    if pre.is_empty() {
        return out;
    }

    out.hash = HASH_CRC
        .captures_iter(&pre)
        .filter_map(|c| c.get(1))
        .find(|m| !m.as_str().chars().all(|c| c.is_ascii_digit()))
        .or_else(|| HASH_SHA1.captures(&pre).and_then(|c| c.get(1)))
        .map(|m| m.as_str().to_string());

    // `[Group] Title - 01 [1080p]` style (anime fansubs).
    let mut subgroup = None;
    let mut body: &str = &pre;
    if let Some(c) = SUBGROUP.captures(&pre)
        && let (Some(all), Some(g)) = (c.get(0), c.get(1))
    {
        let g = g.as_str().trim();
        let looks_technical = tags::resolution(g) != crate::model::Resolution::Unknown;
        if !looks_technical && all.end() < pre.len() {
            subgroup = Some(g.to_string());
            body = &pre[all.end()..];
        }
    }
    let anime = subgroup.is_some();

    let found_group = if anime { None } else { group::find(body) };
    let tech_end = found_group.as_ref().map_or(body.len(), |(p, _)| *p);
    out.release_group = subgroup.or(found_group.map(|(_, g)| g));
    let scan = body.get(..tech_end).unwrap_or(body);

    // Title boundary: episode marker, release year, or the first technical tag.
    let strong = first_marker(&STRONG_MARKER, scan);
    let year_before = |bound: Option<usize>| -> Option<(usize, u16)> {
        YEAR.captures_iter(scan)
            .filter_map(|c| c.get(1))
            .filter(|m| m.start() > 0 && bound.is_none_or(|b| m.start() < b))
            .filter_map(|m| Some((m.start(), m.as_str().parse::<u16>().ok()?)))
            .last()
    };
    let provisional_year = year_before(strong);
    let ep = episode::detect(scan, anime, provisional_year.is_none());
    let bound = match (ep.as_ref().map(|e| e.start), strong) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let year = year_before(bound);

    let title_end = match (&ep, year) {
        (Some(e), Some((y, _))) => e.start.min(y),
        (Some(e), None) => e.start,
        (None, Some((y, _))) => y,
        (None, None) => {
            let weak = first_marker(&WEAK_MARKER, scan);
            match (strong, weak) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b).unwrap_or(scan.len()),
            }
        }
    };
    out.title = title::display_title(scan.get(..title_end).unwrap_or(""));
    out.year = year.map(|(_, y)| y);

    let tail = scan.get(title_end..).unwrap_or("");
    out.streaming_service = tags::streaming_service(tail);
    let quality = tags::quality(tail, out.streaming_service.is_some());
    out.quality = QualityModel {
        quality,
        revision: tags::revision(tail),
    };
    out.languages = tags::languages(tail);
    out.edition = tags::edition(tail);
    out.hdr = tags::hdr(tail);
    out.video_codec = tags::video_codec(tail);
    let (audio, channels) = tags::audio(tail);
    out.audio_codecs = audio;
    out.audio_channels = channels;
    out.is_3d = tags::is_3d(tail);
    out.is_hardcoded_subs = tags::hardcoded_subs(tail);

    out.kind = match ep {
        Some(e) => ReleaseKind::Episode(e.info),
        None if !out.title.is_empty()
            && (out.year.is_some() || quality != Quality::Unknown || strong.is_some()) =>
        {
            ReleaseKind::Movie
        }
        None => ReleaseKind::Unknown,
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioCodec, Language, VideoCodec};

    #[test]
    fn readme_example() {
        let p = parse("Show.Name.S02E05.1080p.WEB-DL.DDP5.1.Atmos.H.264-GROUP");
        assert_eq!(p.title, "Show Name");
        let e = p.episode().unwrap();
        assert_eq!(e.season, Some(2));
        assert_eq!(e.episodes, vec![5]);
        assert_eq!(p.quality.quality, Quality::WebDl1080p);
        assert_eq!(p.languages, vec![Language::English]);
        assert_eq!(p.release_group.as_deref(), Some("GROUP"));
        assert_eq!(p.video_codec, Some(VideoCodec::H264));
        assert_eq!(p.audio_codecs, vec![AudioCodec::Eac3, AudioCodec::Atmos]);
        assert_eq!(p.audio_channels, Some(5.1));
    }

    #[test]
    fn movie_with_year_in_title() {
        let p = parse("Blade.Runner.2049.2017.2160p.UHD.BluRay.x265-GRP");
        assert_eq!(p.title, "Blade Runner 2049");
        assert_eq!(p.year, Some(2017));
        assert_eq!(p.quality.quality, Quality::Bluray2160p);
        assert_eq!(p.kind, ReleaseKind::Movie);
    }

    #[test]
    fn anime_absolute() {
        let p = parse("[SubsPlease] One Piece - 1052 (1080p) [ABCD1234].mkv");
        assert_eq!(p.title, "One Piece");
        assert_eq!(p.release_group.as_deref(), Some("SubsPlease"));
        assert_eq!(p.episode().unwrap().absolute_episodes, vec![1052]);
        assert_eq!(p.hash.as_deref(), Some("ABCD1234"));
    }

    #[test]
    fn garbage_is_safe() {
        for s in [
            "",
            " ",
            "-",
            "[",
            "]",
            "[]",
            "()",
            "S",
            "S01E",
            "....",
            "-[-]-",
            "é",
            "[ä] - 99999999999",
        ] {
            let _ = parse(s);
        }
    }
}
