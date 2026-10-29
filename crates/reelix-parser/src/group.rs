//! Release-group detection (`-GROUP` suffixes, trailing `[GROUP]` tags and `(… GROUP)` blocks).

re!(
    TECH_TOKEN,
    r"(?i)^(?:dl|rip|hd|sd|uhd|fhd|web|webdl|webrip|hdtv|bluray|bdrip|brrip|remux|dvd|dvdrip|x26[45]|h\.?26[45]|hevc|avc|xvid|divx|aac|ac3|eac3|ddp?|dd\+|dts|ma|x|hdr|hdr10|sdr|dv|cam|ts|tc|scr|r5|\d{3,4}[pi]|\d+|\d+bit|10bit|8bit|multi|dual|audio|subs?|subbed|dubbed|eng|english|proper|repack|internal|limited|complete|s\d+e\d+|s\d+|e\d{2,4}|mkv|mp4|avi|sample|atmos|truehd|flac|opus|mp3|hc|3d|sbs|4k|imax|\d\.\d|dovi|hybrid)$"
);
re!(DOTTED_GROUP, r"^[A-Za-z0-9]+\.[A-Za-z]{2,3}$");
re!(
    BEFORE_GROUP,
    r"(?i)\b(?:\d{3,4}[pi]|4K|UHD|Blu-?Ray|BDRip|BRRip|BD|WEB|WEB-?DL|WEB-?Rip|HDTV|PDTV|SDTV|DVDRip|DVDR?|NTSC|PAL|x26[45]|h\.?26[45]|HEVC|AVC|XviD|DivX|S\d{1,4}E\d{1,4}|S\d{1,2}|\d{1,2}x\d{2,3}|(?:19|20)\d{2}|AAC|AC3|DTS|DDP?|FLAC|Atmos|REMUX|CAM|TS|TC|HDRip|HDCAM|HDTS|SCR|DVDSCR|R5|PROPER|REPACK|iNTERNAL|INTERNAL|MULTi|10bit|HDR|DV|COMPLETE)\b|[0-9]\.[0-9]"
);
re!(
    HAS_TECH,
    r"(?i)\b(?:\d{3,4}[pi]|BluRay|Blu-ray|WEB-?DL|WEBRip|HDTV|x26[45]|HEVC|AVC|BDRip|10bit)\b"
);

/// Bracketed suffixes that are site tags rather than groups.
const SITE_TAGS: &[&str] = &[
    "rarbg",
    "rartv",
    "eztv",
    "eztv.re",
    "eztvx.to",
    "eztv.io",
    "ettv",
    "tgx",
    "publichd",
    "glodls",
    "ethd",
    "tjet",
    "1337x",
    "mkvcinemas",
    "torrentgalaxy",
    "tgx.to",
    "rarbg.to",
    "rarbgx",
    "ion10",
    "sev",
];

pub(crate) fn is_site_tag(content: &str) -> bool {
    let c = content.trim().to_ascii_lowercase();
    SITE_TAGS.contains(&c.as_str())
}

fn is_tech(s: &str) -> bool {
    TECH_TOKEN.is_match(s)
}

fn valid_candidate(c: &str) -> bool {
    if c.is_empty()
        || c.len() > 40
        || c.contains(char::is_whitespace)
        || is_tech(c)
        || BEFORE_GROUP.is_match(c)
    {
        return false;
    }
    if !c
        .chars()
        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '@' | '&' | '!' | '$' | '-' | '+'))
    {
        return false;
    }
    if c.contains('.') {
        return DOTTED_GROUP.is_match(c) && !c.split('.').any(is_tech);
    }
    c.chars().any(|ch| ch.is_alphanumeric())
}

/// Splits trailing `[...]` / `(...)` blocks off `s`; returns the remaining prefix and the
/// blocks (outermost-last order reversed: last block first) with their start offsets.
fn trailing_blocks(s: &str) -> (&str, Vec<(usize, char, &str)>) {
    let mut rest = s.trim_end();
    let mut blocks = Vec::new();
    loop {
        let (open, close) = match rest.chars().last() {
            Some(']') => ('[', ']'),
            Some(')') => ('(', ')'),
            _ => break,
        };
        let Some(start) = rest.rfind(open) else { break };
        let inner = rest.get(start + 1..rest.len() - close.len_utf8()).unwrap_or("");
        blocks.push((start, open, inner));
        rest = rest.get(..start).unwrap_or("").trim_end();
    }
    (rest, blocks)
}

/// Finds the release group in `body`; returns the byte offset where it (and its delimiter) start.
pub(crate) fn find(body: &str) -> Option<(usize, String)> {
    let (prefix, blocks) = trailing_blocks(body);

    // 1. `...-GROUP` at the end of the bracket-free prefix.
    if let Some(dash) = prefix.rfind('-') {
        let cand = prefix.get(dash + 1..).unwrap_or("").trim();
        if valid_candidate(cand) && BEFORE_GROUP.is_match(prefix.get(..dash).unwrap_or("")) {
            let before = prefix.get(..dash).unwrap_or("");
            // Groups that themselves contain a dash, like "D-Z0N3": a short non-tech segment
            // directly preceded by another dash.
            if let Some(prev_dash) = before.rfind('-') {
                let seg = before.get(prev_dash + 1..).unwrap_or("");
                if (1..=2).contains(&seg.len())
                    && seg.chars().all(|c| c.is_ascii_alphanumeric())
                    && !is_tech(seg)
                {
                    return Some((prev_dash, format!("{seg}-{cand}")));
                }
            }
            // `-de[42]`: a short bracketed suffix glued to the group name.
            let glued = body.get(prefix.len()..).unwrap_or("");
            if let Some(&(_, '[', inner)) = blocks.last()
                && glued.starts_with('[')
                && (1..=4).contains(&inner.len())
                && inner.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Some((dash, format!("{cand}[{inner}]")));
            }
            return Some((dash, cand.to_string()));
        }
    }

    // 2. The last bracket block, e.g. `[YTS.MX]` or `(… x265 10bit Tigole)`.
    let &(start, open, inner) = blocks.first()?;
    let inner = inner.trim();
    if valid_candidate(inner) && !is_site_tag(inner) {
        return Some((start, inner.to_string()));
    }
    if open == '(' && HAS_TECH.is_match(inner) {
        let last = inner.split_whitespace().last()?;
        if valid_candidate(last) && last.chars().all(|c| c.is_ascii_alphanumeric()) && !is_tech(last) {
            return Some((start + 1 + inner.rfind(last)?, last.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> Option<String> {
        find(s).map(|(_, g)| g)
    }

    #[test]
    fn groups() {
        assert_eq!(
            g("Show.S01E01.720p.HDTV.x264-KILLERS").as_deref(),
            Some("KILLERS")
        );
        assert_eq!(g("Movie.2020.1080p.WEB-DL.DD5.1.H.264").as_deref(), None);
        assert_eq!(
            g("Movie.2010.1080p.BluRay.x264-D-Z0N3").as_deref(),
            Some("D-Z0N3")
        );
        assert_eq!(
            g("Movie (2019) [1080p] [BluRay] [YTS.MX]").as_deref(),
            Some("YTS.MX")
        );
        assert_eq!(
            g("Movie (2019) (1080p BluRay x265 10bit AAC 5.1 Tigole)").as_deref(),
            Some("Tigole")
        );
        assert_eq!(g("Show.S01E01.1080p.WEB.H264-GRP [eztv]").as_deref(), Some("GRP"));
    }
}
