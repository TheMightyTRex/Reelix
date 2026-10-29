//! Music release titles (albums, discographies) and per-file audio quality.
//!
//! Handles tracker style (`Artist - Album (2024) [FLAC 24-96]`, `Artist - 2019 - Album [MP3 V0]`)
//! and scene style (`Artist-Album-(CAT123)-WEB-FLAC-2024-GROUP`, `Artist_-_Album-2CD-2005-GRP`).
//!
//! Like [`parse`](crate::parse) this is pure and must never panic.

use serde::{Deserialize, Serialize};

use crate::book::text::{self, Piece};
use crate::model::{Language, ParsedRelease, Quality, QualityModel};

/// Where a music release was ripped from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MusicSource {
    Cd,
    Web,
    Vinyl,
    Cassette,
    Dvd,
    Sacd,
    #[default]
    Unknown,
}

/// Everything extracted from a music release title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedMusic {
    pub raw_title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    /// Music family.
    pub quality: QualityModel,
    pub bitrate_kbps: Option<u32>,
    pub bit_depth: Option<u8>,
    pub sample_rate_hz: Option<u32>,
    pub source: MusicSource,
    pub is_discography: bool,
    pub discography_years: Option<(u16, u16)>,
    pub release_group: Option<String>,
    pub catalog_number: Option<String>,
    /// "Deluxe Edition", "Remastered 2011".
    pub edition: Option<String>,
    pub languages: Vec<Language>,
}

impl ParsedMusic {
    /// Fills a [`ParsedRelease`] for the decision engine: title = `"{artist} {album}"`, year,
    /// quality, languages, release group and edition.
    pub fn to_release(&self) -> ParsedRelease {
        let mut r = ParsedRelease::empty(&self.raw_title);
        r.title = [self.artist.as_deref(), self.album.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        r.year = self.year;
        r.quality = self.quality;
        r.languages = self.languages.clone();
        r.release_group = self.release_group.clone();
        r.edition = self.edition.clone();
        r
    }

    /// `true` for `VA` / `Various Artists` compilations.
    pub fn is_various_artists(&self) -> bool {
        self.artist.as_deref() == Some(VARIOUS)
    }
}

const VARIOUS: &str = "Various Artists";

/// Parses a music release title. Never panics.
pub fn parse_music(title: &str) -> ParsedMusic {
    let mut t = Tags::default();
    let mut s = text::normalize(title);
    re!(
        EXT,
        r"(?i)\.(flac|mp3|m4a|aac|ogg|opus|wav|zip|rar|7z|torrent|nzb)$"
    );
    if let Some(m) = EXT.find(&s)
        && m.start() > 0
    {
        let ext = s[m.start() + 1..].to_lowercase();
        if let Some(Tag::Codec(c)) = word_tag(&ext, true) {
            t.codec.get_or_insert(c);
        }
        s.truncate(m.start());
    }
    re!(DOTTED_SEP, r"\.-\.|\._-_\.");
    if !s.contains(' ') && DOTTED_SEP.is_match(&s) {
        s = text::undot(&s);
    }
    let (artist, album) = if !s.contains(' ') && s.split('-').filter(|f| !f.is_empty()).count() >= 2 {
        scene(&s, &mut t)
    } else {
        spaced(&s, &mut t)
    };
    finish(title, artist, album, t)
}

/// Quality of one audio file from its extension/codec plus stream properties (used at import
/// and scan). Unknown formats, and lossy files without a known bitrate, are `UnknownMusic`.
pub fn music_quality(ext: &str, bitrate_kbps: Option<u32>, bit_depth: Option<u8>, vbr: bool) -> Quality {
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    let hi_res = bit_depth.is_some_and(|b| b > 16);
    match ext.as_str() {
        "flac" => {
            if hi_res {
                Quality::Flac24
            } else {
                Quality::Flac
            }
        }
        "alac" => {
            if hi_res {
                Quality::Alac24
            } else {
                Quality::Alac
            }
        }
        "wav" | "wave" | "aiff" | "aif" => Quality::Wav,
        "ogg" | "oga" | "vorbis" => Quality::Vorbis,
        "opus" => Quality::Opus,
        "wma" => Quality::Wma,
        "mp3" => match (bitrate_kbps, vbr) {
            (Some(b), true) if b >= 220 => Quality::Mp3VbrV0,
            (Some(_), true) => Quality::Mp3VbrV2,
            (Some(b), false) if b >= 320 => Quality::Mp3_320,
            (Some(b), false) if b >= 256 => Quality::Mp3_256,
            (Some(b), false) if b >= 192 => Quality::Mp3_192,
            (Some(_), false) => Quality::Mp3_128,
            (None, _) => Quality::UnknownMusic,
        },
        "m4a" | "aac" | "mp4" => match (bitrate_kbps, vbr) {
            (Some(b), false) if b >= 320 => Quality::Aac320,
            (Some(b), false) if b >= 256 => Quality::Aac256,
            (Some(_), _) => Quality::AacVbr,
            (None, _) => Quality::UnknownMusic,
        },
        _ => Quality::UnknownMusic,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Flac,
    Alac,
    Mp3,
    Aac,
    Vorbis,
    Opus,
    Wav,
    Wma,
    /// APE, WavPack, DSD: lossless but without a quality of its own.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vbr {
    V0,
    V2,
    Any,
}

#[derive(Debug, Default)]
struct Tags {
    codec: Option<Codec>,
    lossless: bool,
    hi_res: bool,
    bitrate: Option<u32>,
    vbr: Option<Vbr>,
    bit_depth: Option<u8>,
    sample_rate: Option<u32>,
    source: Option<MusicSource>,
    year: Option<u16>,
    years: Option<(u16, u16)>,
    discography: bool,
    group: Option<String>,
    catalog: Option<String>,
    edition: Option<String>,
    languages: Vec<Language>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tag {
    Codec(Codec),
    Lossless,
    HiRes,
    Bitrate(u32),
    Vbr(Vbr),
    Depth(u8),
    Rate(u32),
    Source(MusicSource),
    Year(u16),
    Lang(Language),
    Discography,
    Noise,
}

impl Tags {
    fn apply(&mut self, tag: Tag) {
        match tag {
            Tag::Codec(c) => {
                self.codec.get_or_insert(c);
            }
            Tag::Lossless => self.lossless = true,
            Tag::HiRes => self.hi_res = true,
            Tag::Bitrate(b) => {
                self.bitrate.get_or_insert(b);
            }
            Tag::Vbr(v) => {
                if self.vbr.is_none_or(|old| old == Vbr::Any) {
                    self.vbr = Some(v);
                }
            }
            Tag::Depth(d) => {
                self.bit_depth.get_or_insert(d);
            }
            Tag::Rate(r) => {
                self.sample_rate.get_or_insert(r);
            }
            Tag::Source(s) => {
                self.source.get_or_insert(s);
            }
            Tag::Year(y) => {
                self.year.get_or_insert(y);
            }
            Tag::Lang(l) => {
                if !self.languages.contains(&l) {
                    self.languages.push(l);
                }
            }
            Tag::Discography => self.discography = true,
            Tag::Noise => {}
        }
    }

    fn set_edition(&mut self, e: &str) {
        let e = text::soften_caps(&text::tidy(e));
        if self.edition.is_none() && !e.is_empty() {
            self.edition = Some(e);
        }
    }
}

const BITRATES: &[u32] = &[64, 96, 112, 128, 160, 192, 224, 256, 320];

/// `44.1` → 44100, `96` → 96000.
fn sample_rate(khz: &str) -> Option<u32> {
    let (int, frac) = khz.split_once('.').unwrap_or((khz, ""));
    let int: u32 = int.parse().ok()?;
    let rate = match (int, frac) {
        (44, _) => 44_100,
        (88, _) => 88_200,
        (176, _) => 176_400,
        (352, _) => 352_800,
        (32 | 48 | 96 | 192 | 384, "" | "0") => int * 1000,
        _ => return None,
    };
    Some(rate)
}

/// Classifies one lower-cased word. `loose` also accepts short words that are only tags in a
/// tag context (bare bitrates, `lp`, language codes).
fn word_tag(w: &str, loose: bool) -> Option<Tag> {
    re!(KBPS, r"^@?(\d{2,3})(?:k|kbps|kbit|kbits|kb/s|kbit/s)$");
    re!(DEPTH, r"^(16|24|32)(?:bit|bits|b)$");
    re!(KHZ, r"^(\d{2,3}(?:\.\d)?)khz$");
    re!(DISCS, r"^\d{1,2}(?:cd|cds|xcd|lp|lps|xlp|discs?)$");
    let w = w.trim_matches(|c: char| {
        matches!(
            c,
            ',' | ';' | ':' | '!' | '*' | '.' | '\'' | '"' | '(' | ')' | '[' | ']'
        )
    });
    let tag = match w {
        "" => return None,
        "flac" | "flac16" | "flac24" | "webflac" => Tag::Codec(Codec::Flac),
        "alac" => Tag::Codec(Codec::Alac),
        "mp3" | "lame" => Tag::Codec(Codec::Mp3),
        "aac" | "m4a" => Tag::Codec(Codec::Aac),
        "ogg" | "vorbis" => Tag::Codec(Codec::Vorbis),
        "opus" => Tag::Codec(Codec::Opus),
        "wav" | "aiff" | "pcm" => Tag::Codec(Codec::Wav),
        "wma" => Tag::Codec(Codec::Wma),
        "ape" | "wv" | "wavpack" | "dsd" | "dsf" | "dff" | "dsd64" | "dsd128" | "dsd256" | "tta" => {
            Tag::Codec(Codec::Other)
        }
        "lossless" => Tag::Lossless,
        "hi-res" | "hires" | "hi_res" | "hdtracks" => Tag::HiRes,
        "v0" | "apx" => Tag::Vbr(Vbr::V0),
        "v2" | "aps" => Tag::Vbr(Vbr::V2),
        "vbr" => Tag::Vbr(Vbr::Any),
        "cbr" => Tag::Noise,
        "cd" | "cdm" | "cds" | "cdr" | "cda" | "cdep" | "cdrip" | "cd-rip" | "eac" | "xld" | "log"
        | "cue" | "logcue" => Tag::Source(MusicSource::Cd),
        "web" | "web-dl" | "webdl" | "webrip" | "digital" | "itunes" | "bandcamp" | "qobuz" | "tidal"
        | "deezer" | "beatport" | "7digital" | "amazon" | "spotify" | "juno" | "traxsource" => {
            Tag::Source(MusicSource::Web)
        }
        "vinyl" | "vinylrip" | "vls" | "vinyl-rip" | "lp-rip" | "lprip" => Tag::Source(MusicSource::Vinyl),
        "sacd" | "sacd-r" | "sacdrip" | "sacd-iso" => Tag::Source(MusicSource::Sacd),
        "cassette" | "tape" | "mc" | "tape-rip" => Tag::Source(MusicSource::Cassette),
        "dvd" | "dvda" | "dvd-a" | "dvd-audio" | "dvdrip" => Tag::Source(MusicSource::Dvd),
        "discography" | "discografia" | "discographie" | "diskografie" | "diskografia" | "discografie" => {
            Tag::Discography
        }
        "scene" | "retail" | "proper" | "repack" | "internal" | "readnfo" | "read_nfo" | "nfo" | "dirfix"
        | "nfofix" | "explicit" | "clean" | "promo" | "bootleg" | "single" | "ep" | "advance" | "100%"
        | "stereo" | "mono" | "joint" | "tracks" | "kbps" | "khz" | "bit" | "multichannel" | "5.1"
        | "iso" | "rip" | "audio" | "remux" | "mp4" | "lyrics" | "ost" | "cover" | "covers" | "scans"
        | "artwork" | "mag" | "sat" | "fm" | "dab" | "cable" | "live" => {
            if w == "live" && !loose {
                return None;
            }
            Tag::Noise
        }
        "lp" | "2lp" if loose => Tag::Source(MusicSource::Vinyl),
        _ => {
            if let Some(c) = KBPS.captures(w) {
                return c[1].parse().ok().map(Tag::Bitrate);
            }
            if let Some(c) = DEPTH.captures(w) {
                return c[1].parse().ok().map(Tag::Depth);
            }
            if let Some(c) = KHZ.captures(w) {
                return sample_rate(&c[1]).map(Tag::Rate);
            }
            if DISCS.is_match(w) {
                return Some(if w.contains("lp") {
                    Tag::Source(MusicSource::Vinyl)
                } else if w.contains("cd") {
                    Tag::Source(MusicSource::Cd)
                } else {
                    Tag::Noise
                });
            }
            if let Some(y) = text::year(w, 1900) {
                return Some(Tag::Year(y));
            }
            if loose {
                if let Ok(b) = w.trim_start_matches('@').parse::<u32>()
                    && BITRATES.contains(&b)
                {
                    return Some(Tag::Bitrate(b));
                }
                if let Some(l) = text::language(w, false) {
                    return Some(Tag::Lang(l));
                }
            }
            return None;
        }
    };
    Some(tag)
}

/// Words of a tag context. `24-96`, `24bit/96kHz` and `16-44.1` pairs are folded first.
fn all_tags(content: &str, loose: bool) -> Option<Vec<Tag>> {
    re!(
        PAIR,
        r"(?i)\b(16|24|32)\s*(?:bits?|b)?\s*[-/_ ]\s*(44\.1|44|48|88\.2|88|96|176\.4|176|192|352\.8|384)(?:\s*khz)?\b"
    );
    re!(
        SPACED,
        r"(?i)\b(\d{2,3}(?:\.\d)?)\s+(kbps|kbit/s|kb/s|k|khz)\b|\b(16|24|32)\s+(bits?)\b"
    );
    re!(
        ITUNES,
        r"(?i)\bitunes\s+plus\b|\bweb\s*-\s*dl\b|\bhi\s*-\s*res\b|\bvinyl\s*-\s*rip\b|\blog\s*\+\s*cue\b|\bs?a?cd\s*-\s*r\b"
    );
    let mut tags = Vec::new();
    let mut rest = String::with_capacity(content.len());
    let mut last = 0;
    for c in PAIR.captures_iter(content) {
        let Some(m) = c.get(0) else { continue };
        rest.push_str(content.get(last..m.start()).unwrap_or(""));
        rest.push(' ');
        last = m.end();
        if let Ok(d) = c[1].parse() {
            tags.push(Tag::Depth(d));
        }
        if let Some(r) = sample_rate(&c[2]) {
            tags.push(Tag::Rate(r));
        }
    }
    rest.push_str(content.get(last..).unwrap_or(""));
    let rest = SPACED.replace_all(&rest, "$1$2$3$4");
    let rest = ITUNES.replace_all(&rest, |c: &regex::Captures<'_>| {
        let l = c[0].to_lowercase();
        if l.starts_with("itunes") {
            "aac 256kbps itunes"
        } else if l.starts_with("web") {
            "web"
        } else if l.starts_with("hi") {
            "hires"
        } else if l.starts_with("vinyl") {
            "vinylrip"
        } else if l.starts_with("sacd") {
            "sacd"
        } else if l.starts_with("cd") {
            "cd"
        } else {
            "logcue"
        }
        .to_string()
    });
    let lower = rest.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | ';' | '/' | '+' | '|' | '@' | '&' | '-' | '_' | '~' | '(' | ')' | '[' | ']'
                )
        })
        .filter(|w| !w.is_empty())
        .collect();
    // A bare number next to a lossy codec is its bitrate: `[MP3 VBR 190]`.
    let lossy = words.iter().any(|w| {
        matches!(
            word_tag(w, false),
            Some(Tag::Codec(
                Codec::Mp3 | Codec::Aac | Codec::Opus | Codec::Vorbis | Codec::Wma
            )) | Some(Tag::Vbr(_))
        )
    });
    for w in words {
        match w {
            "flac24" => tags.extend([Tag::Codec(Codec::Flac), Tag::Depth(24)]),
            "webflac" => tags.extend([Tag::Codec(Codec::Flac), Tag::Source(MusicSource::Web)]),
            _ => match word_tag(w, loose) {
                Some(tag) => tags.push(tag),
                None => {
                    let rate: u32 = w.parse().ok().filter(|r| lossy && (32..=500).contains(r))?;
                    tags.push(Tag::Bitrate(rate));
                }
            },
        }
    }
    (!tags.is_empty()).then_some(tags)
}

/// `Deluxe Edition`, `Remastered 2011`, `2011 Remaster`, `25th Anniversary`.
fn is_edition(c: &str) -> bool {
    re!(
        EDITION,
        r"(?i)\b(?:deluxe|remaster(?:ed)?|remastering|expanded|anniversary|edition|reissue|re-issue|re-release|bonus\s+tracks?|legacy|definitive|collector'?s|limited|ltd\.?\s*ed|special|platinum|super\s+deluxe|version|mono\s+mix|stereo\s+mix|redux|uncut|\d{4}\s+(?:re)?mix)\b"
    );
    EDITION.is_match(c)
}

/// Uppercase catalog numbers (`WARPCD123`, `CAT 123`, `SRCS-1234`) and barcodes.
fn catalog(c: &str, strict: bool) -> Option<String> {
    re!(CAT, r"^[A-Z][A-Z0-9]*(?:[ ._-]?[A-Z0-9]+)*$");
    re!(BARCODE, r"^\d{6,13}$");
    let c = c.trim();
    let digits = c.chars().filter(char::is_ascii_digit).count();
    let letters = c.chars().filter(char::is_ascii_alphabetic).count();
    if c.len() > 20 || digits < 2 {
        return None;
    }
    let cat = (CAT.is_match(c) && letters >= 2 && !c.contains(' ')
        || CAT.is_match(c) && !strict && letters >= 2)
        || (!strict && BARCODE.is_match(c));
    if !cat || word_tag(&c.to_lowercase(), true).is_some() || all_tags(c, true).is_some() {
        return None;
    }
    Some(c.replace('_', " "))
}

/// Classifies one bracket group; `true` when it was consumed (not part of the album).
fn classify_bracket(open: char, c: &str, strict_catalog: bool, t: &mut Tags) -> bool {
    re!(RANGE, r"^((?:19|20)\d{2})\s*(?:-|to|~)\s*((?:19|20)\d{2})$");
    re!(
        DISCO,
        r"(?i)\b(?:discograph(?:y|ie)|discografia|diskografie|complete\s+(?:studio\s+)?albums|studio\s+albums|\d+\s+albums|albums\s+collection|(?:complete\s+)?studio\s+recordings|complete\s+recordings)\b"
    );
    let c = c.trim();
    if c.is_empty() {
        return true;
    }
    if let Some(cap) = RANGE.captures(c)
        && let (Some(a), Some(b)) = (text::year(&cap[1], 1900), text::year(&cap[2], 1900))
    {
        t.years.get_or_insert((a.min(b), a.max(b)));
        return true;
    }
    if DISCO.is_match(c) {
        t.discography = true;
        return true;
    }
    let has_codec = all_tags(c, true).is_some();
    if is_edition(c) && !has_codec {
        t.set_edition(c);
        return true;
    }
    if let Some(tags) = all_tags(c, true) {
        tags.into_iter().for_each(|tag| t.apply(tag));
        return true;
    }
    if let Some(cat) = catalog(c, strict_catalog) {
        t.catalog.get_or_insert(cat);
        return true;
    }
    if let Some(l) = text::language(c, false) {
        t.apply(Tag::Lang(l));
        return true;
    }
    // Mixed tag groups such as `[FLAC 24bit Remastered]`.
    if open != '(' && is_edition(c) {
        t.set_edition(c);
        return true;
    }
    false
}

/// Removes classified groups. Unclassified `(...)` stays (it is part of the album name, like
/// `(Live)` or `(feat. X)`); unclassified `[...]`/`{...}` go, the last single-word one becoming
/// the release group (`[PMEDIA]`).
fn take_brackets(s: &str, strict_catalog: bool, t: &mut Tags) -> String {
    let mut out = String::with_capacity(s.len());
    let mut group = None;
    for piece in text::pieces(s) {
        match piece {
            Piece::Text(x) => out.push_str(x),
            Piece::Group { open, inner } => {
                if classify_bracket(open, inner, strict_catalog, t) {
                    out.push(' ');
                } else if open == '(' {
                    out.push('(');
                    out.push_str(inner.trim());
                    out.push(')');
                } else {
                    let inner = inner.trim();
                    if inner.len() <= 20 && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                        group = Some(inner.to_string());
                    }
                    out.push(' ');
                }
            }
        }
    }
    if t.group.is_none() {
        t.group = group;
    }
    out
}

/// Tracker names: `Artist - Album (2019) [FLAC 24-96]`.
fn spaced(s: &str, t: &mut Tags) -> (Option<String>, Option<String>) {
    re!(GROUP, r"([\])}])\s*-\s*([A-Za-z0-9]{2,20})$");
    let mut s = s.to_string();
    if let Some(c) = GROUP.captures(&s)
        && let (Some(m), Some(g)) = (c.get(1), c.get(2))
    {
        t.group = Some(g.as_str().to_string());
        s.truncate(m.end());
    }
    let rest = take_brackets(&s, true, t);
    let mut segs = text::segments(&rest);
    // Whole-segment tags and years (`Artist - 2019 - Album`, `Artist - Album - FLAC`).
    let mut i = 0;
    while i < segs.len() && segs.len() > 2 {
        if i == 0 {
            i += 1;
            continue;
        }
        if let Some(tags) = all_tags(&segs[i], true) {
            tags.into_iter().for_each(|tag| t.apply(tag));
            segs.remove(i);
            continue;
        }
        i += 1;
    }
    if segs.len() >= 2 {
        // `2019 - Artist - Album`
        if segs.len() >= 3
            && let Some(y) = text::year(&segs[0], 1900)
        {
            t.apply(Tag::Year(y));
            segs.remove(0);
        }
    }
    if let Some(last) = segs.last_mut() {
        *last = strip_tail(last, t);
    }
    if segs.len() == 2 {
        // `Artist - Album - 2019` collapsed above; `Artist - Album` trailing year handled in strip_tail.
        if let Some(tags) = all_tags(&segs[1], false)
            && segs[1].chars().any(|c| !c.is_ascii_digit())
        {
            tags.into_iter().for_each(|tag| t.apply(tag));
            segs.truncate(1);
        }
    }
    let mut it = segs.into_iter();
    let artist = it.next();
    let album: Vec<String> = it.collect();
    let album = (!album.is_empty()).then(|| album.join(" - "));
    match (artist, album) {
        // A lone segment is the album of an unknown artist only if it was tagged; keep it as album.
        (Some(a), None) => {
            if t.discography {
                (Some(a), None)
            } else {
                (None, Some(a))
            }
        }
        other => other,
    }
}

/// Strips tag words trailing the album: `Album 2019 FLAC 24bit`, `Album WEB 320`.
fn strip_tail(seg: &str, t: &mut Tags) -> String {
    re!(
        PAIR_TAIL,
        r"(?i)\s(16|24|32)\s*(?:bits?|b)?\s*[-/]\s*(44\.1|44|48|88\.2|88|96|176\.4|192)(?:\s*khz)?$"
    );
    re!(
        SPACED,
        r"(?i)\b(\d{2,3}(?:\.\d)?)\s+(kbps|kbit/s|kb/s|khz)\b|\b(16|24|32)\s+(bits?)\b"
    );
    let mut seg = SPACED.replace_all(seg, "$1$2$3$4").into_owned();
    let mut found = Vec::new();
    loop {
        if let Some(c) = PAIR_TAIL.captures(&seg)
            && let (Some(m), Ok(d)) = (c.get(0), c[1].parse())
        {
            found.push(Tag::Depth(d));
            if let Some(r) = sample_rate(&c[2]) {
                found.push(Tag::Rate(r));
            }
            seg.truncate(m.start());
            continue;
        }
        let Some((head, last)) = seg.rsplit_once(' ') else {
            break;
        };
        if head.trim().is_empty() {
            break;
        }
        let lower = last.to_lowercase();
        if lower == "lp" {
            break;
        }
        let codec_seen = found.iter().any(|f| matches!(f, Tag::Codec(_)))
            || t.codec.is_some()
            || lower
                .split(['-', '/', '@', '_'])
                .any(|w| matches!(word_tag(w, false), Some(Tag::Codec(_))));
        let tags: Option<Vec<Tag>> = lower
            .split(['-', '/', '@', '_', '+', ','])
            .filter(|w| !w.is_empty())
            .map(|w| word_tag(w, codec_seen))
            .collect();
        match tags {
            Some(tags) if !tags.is_empty() => {
                // `OK Computer OKNOTOK 1997 2017 (2017)`: a bracketed year wins, bare years stay.
                let year_known = t.year.is_some() || found.iter().any(|f| matches!(f, Tag::Year(_)));
                if year_known && tags.iter().any(|f| matches!(f, Tag::Year(_))) {
                    break;
                }
                found.extend(tags);
                let keep = head.trim_end().len();
                seg.truncate(keep);
            }
            _ => break,
        }
    }
    found.into_iter().for_each(|tag| t.apply(tag));
    text::tidy(&seg)
}

/// Scene names: `Artist-Album-(CAT)-WEB-FLAC-2024-GROUP`.
fn scene(s: &str, t: &mut Tags) -> (Option<String>, Option<String>) {
    let s = s.replace("_-_", "-").replace("_--_", "-");
    let mut fields: Vec<&str> = s
        .split('-')
        .map(|f| f.trim_matches('_'))
        .filter(|f| !f.is_empty())
        .collect();
    if fields.len() >= 3
        && let Some(g) = fields.last().filter(|g| looks_like_group(g))
    {
        t.group = Some((*g).to_string());
        fields.pop();
    }
    let mut artist = None;
    let mut album: Vec<String> = Vec::new();
    let mut in_tags = false;
    for (i, f) in fields.iter().enumerate() {
        let spaced = f.replace('_', " ");
        if i == 0 {
            artist = Some(text::tidy(&take_brackets(&spaced, false, t)));
            continue;
        }
        let whole_group =
            spaced.starts_with('(') && spaced.ends_with(')') && text::pieces(&spaced).len() == 1;
        if whole_group {
            let inner = &spaced[1..spaced.len() - 1];
            if classify_bracket('(', inner, false, t) {
                in_tags |= i > 1;
                continue;
            }
            if !in_tags {
                album.push(spaced);
            }
            continue;
        }
        if i == 1 && !matches!(all_tags(&spaced, false).as_deref(), Some([Tag::Discography])) {
            album.push(text::tidy(&take_brackets(&spaced, false, t)));
            continue;
        }
        if let Some(tags) = all_tags(&spaced, true) {
            tags.into_iter().for_each(|tag| t.apply(tag));
            in_tags = true;
            continue;
        }
        if is_edition(&spaced) {
            t.set_edition(&spaced);
            in_tags = true;
            continue;
        }
        if let Some(l) = text::language(&spaced, true) {
            t.apply(Tag::Lang(l));
            in_tags = true;
            continue;
        }
        if !in_tags {
            album.push(text::tidy(&take_brackets(&spaced, false, t)));
        }
    }
    album.retain(|a| !a.is_empty());
    let album = (!album.is_empty()).then(|| album.join(" - "));
    (artist.filter(|a| !a.is_empty()), album)
}

fn looks_like_group(g: &str) -> bool {
    g.len() <= 24
        && g.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(
            word_tag(&g.to_lowercase(), false),
            Some(Tag::Codec(_) | Tag::Year(_) | Tag::Source(_) | Tag::Depth(_) | Tag::Bitrate(_))
        )
}

fn finish(raw: &str, artist: Option<String>, album: Option<String>, mut t: Tags) -> ParsedMusic {
    re!(
        DISCO_TEXT,
        r"(?i)\b(?:discograph(?:y|ie)|discografia|diskografie|complete\s+(?:studio\s+)?albums|studio\s+albums|\d+\s+albums|albums\s+collection|(?:complete\s+)?studio\s+recordings|complete\s+recordings)\b"
    );
    re!(
        RANGE_TEXT,
        r"(?:^|[\s(\[_-])((?:19|20)\d{2})\s*(?:-|to|~)\s*((?:19|20)\d{2})(?:$|[\s)\]_-])"
    );
    let mut album = album.map(|a| text::tidy(&a)).filter(|a| !a.is_empty());
    if album.as_deref().is_some_and(|a| DISCO_TEXT.is_match(a)) || DISCO_TEXT.is_match(raw) {
        t.discography = true;
    }
    if t.discography {
        if t.years.is_none()
            && let Some(c) = RANGE_TEXT.captures(raw)
            && let (Some(a), Some(b)) = (text::year(&c[1], 1900), text::year(&c[2], 1900))
        {
            t.years = Some((a.min(b), a.max(b)));
        }
        album = None;
        t.year = None;
    }
    let artist = artist.map(|a| text::tidy(&a)).filter(|a| !a.is_empty()).map(|a| {
        if matches!(
            a.to_lowercase().as_str(),
            "va" | "v.a." | "v.a" | "v/a" | "various" | "various artists"
        ) {
            VARIOUS.to_string()
        } else {
            a
        }
    });

    // Quality.
    let sr_hi = t.sample_rate.is_some_and(|r| r > 48_000);
    let codec = t.codec.or_else(|| {
        if t.lossless || t.hi_res || t.bit_depth.is_some() || t.sample_rate.is_some() {
            Some(Codec::Flac)
        } else if t.vbr.is_some_and(|v| v != Vbr::Any) || t.bitrate.is_some() {
            Some(Codec::Mp3)
        } else {
            None
        }
    });
    let depth = t.bit_depth.or(if t.hi_res || sr_hi { Some(24) } else { None });
    let quality = match codec {
        Some(Codec::Flac) => music_quality("flac", None, depth, false),
        Some(Codec::Alac) => music_quality("alac", None, depth, false),
        Some(Codec::Mp3) => match t.vbr {
            Some(Vbr::V0) => Quality::Mp3VbrV0,
            Some(Vbr::V2) => Quality::Mp3VbrV2,
            Some(Vbr::Any) => music_quality("mp3", t.bitrate, None, true),
            None => music_quality("mp3", t.bitrate, None, false),
        },
        Some(Codec::Aac) => music_quality("aac", t.bitrate, None, t.vbr.is_some()),
        Some(Codec::Vorbis) => Quality::Vorbis,
        Some(Codec::Opus) => Quality::Opus,
        Some(Codec::Wav) => Quality::Wav,
        Some(Codec::Wma) => Quality::Wma,
        Some(Codec::Other) | None => Quality::UnknownMusic,
    };

    ParsedMusic {
        raw_title: raw.to_string(),
        artist,
        album,
        year: t.year,
        quality: QualityModel::new(quality),
        bitrate_kbps: t.bitrate,
        bit_depth: t.bit_depth,
        sample_rate_hz: t.sample_rate,
        source: t.source.unwrap_or_default(),
        is_discography: t.discography,
        discography_years: if t.discography { t.years } else { None },
        release_group: t.group,
        catalog_number: t.catalog,
        edition: t.edition,
        languages: t.languages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_qualities() {
        assert_eq!(music_quality("FLAC", None, Some(24), false), Quality::Flac24);
        assert_eq!(music_quality(".flac", None, Some(16), false), Quality::Flac);
        assert_eq!(music_quality("mp3", Some(320), None, false), Quality::Mp3_320);
        assert_eq!(music_quality("mp3", Some(245), None, true), Quality::Mp3VbrV0);
        assert_eq!(music_quality("mp3", None, None, false), Quality::UnknownMusic);
        assert_eq!(music_quality("mkv", None, None, false), Quality::UnknownMusic);
    }

    #[test]
    fn tracker_style() {
        let m = parse_music("Radiohead - OK Computer (Remastered 2017) (1997) [FLAC 24-96]");
        assert_eq!(m.artist.as_deref(), Some("Radiohead"));
        assert_eq!(m.album.as_deref(), Some("OK Computer"));
        assert_eq!(m.year, Some(1997));
        assert_eq!(m.edition.as_deref(), Some("Remastered 2017"));
        assert_eq!(m.quality.quality, Quality::Flac24);
        assert_eq!(m.bit_depth, Some(24));
        assert_eq!(m.sample_rate_hz, Some(96_000));
        assert_eq!(parse_music("whatever").quality.quality, Quality::UnknownMusic);
    }

    #[test]
    fn scene_style() {
        let m = parse_music("Daft_Punk-Random_Access_Memories-(88883716861)-WEB-FLAC-2013-GROUP");
        assert_eq!(m.artist.as_deref(), Some("Daft Punk"));
        assert_eq!(m.album.as_deref(), Some("Random Access Memories"));
        assert_eq!(m.catalog_number.as_deref(), Some("88883716861"));
        assert_eq!(m.source, MusicSource::Web);
        assert_eq!(m.quality.quality, Quality::Flac);
        assert_eq!(m.year, Some(2013));
        assert_eq!(m.release_group.as_deref(), Some("GROUP"));
    }

    #[test]
    fn to_release_title() {
        let r = ParsedMusic {
            artist: Some("Radiohead".into()),
            album: Some("OK Computer".into()),
            edition: Some("OKNOTOK".into()),
            ..parse_music("x")
        }
        .to_release();
        assert_eq!(r.title, "Radiohead OK Computer");
        assert_eq!(r.edition.as_deref(), Some("OKNOTOK"));
    }
}
