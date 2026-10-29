//! Book release titles (ebooks and audiobooks).
//!
//! Handles tracker style (`Author - Series 01 - Title (2006) [Narrator] [M4B 64kbps]`,
//! `Title (Series #3) by Author [EPUB]`), dotted scene style
//! (`Author.-.Title.2010.RETAIL.EPUB.eBook-GRP`) and underscore scene audiobooks
//! (`Author_-_Title-AUDIOBOOK-WEB-EN-2022-GRP`).
//!
//! Like [`parse`](crate::parse) this is pure and must never panic.

pub(crate) mod text;

use reelix_core::BookFormat;
use serde::{Deserialize, Serialize};

use self::text::Piece;
use crate::model::{Language, ParsedRelease, Quality, QualityFamily, QualityModel};

/// Everything extracted from an ebook or audiobook release title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedBook {
    pub raw_title: String,
    pub author: Option<String>,
    pub title: String,
    pub series: Option<String>,
    pub series_position: Option<String>,
    pub year: Option<u16>,
    /// From the extension, tags or the category hint.
    pub format: Option<BookFormat>,
    /// Ebook/audiobook family; `UnknownText` / `UnknownAudiobook` when unsure.
    pub quality: QualityModel,
    pub narrator: Option<String>,
    pub bitrate_kbps: Option<u32>,
    /// `Some(false)` = abridged.
    pub unabridged: Option<bool>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub release_group: Option<String>,
    pub languages: Vec<Language>,
    /// "Collection", "Books 1-5", "Complete Series".
    pub is_collection: bool,
}

impl ParsedBook {
    /// Fills a [`ParsedRelease`] for the decision engine: title = `"{author} {title}"`, year,
    /// quality, languages and release group; kind stays `Unknown`.
    pub fn to_release(&self) -> ParsedRelease {
        let mut r = ParsedRelease::empty(&self.raw_title);
        r.title = match &self.author {
            Some(a) if !a.trim().is_empty() => format!("{} {}", a.trim(), self.title.trim()),
            _ => self.title.trim().to_string(),
        };
        r.year = self.year;
        r.quality = self.quality;
        r.languages = self.languages.clone();
        r.release_group = self.release_group.clone();
        r
    }

    /// `true` when the release is tagged `RETAIL` (an unmodified store copy).
    pub fn is_retail(&self) -> bool {
        re!(RETAIL, r"(?i)(?:^|[^a-z])retail(?:$|[^a-z])");
        RETAIL.is_match(&self.raw_title)
    }
}

/// Parses a book release title. `format_hint` comes from indexer categories (3030 → Audiobook,
/// 7xxx → Ebook) or the file kind. Never panics.
pub fn parse_book(title: &str, format_hint: Option<BookFormat>) -> ParsedBook {
    let mut t = Tags::default();
    let mut s = text::normalize(title);

    re!(
        EXT,
        r"(?i)\.(epub|azw3|azw|kfx|mobi|pdf|cbz|cbr|cb7|djvu|fb2|lit|rtf|m4b|m4a|aac|mp3|flac|ogg|opus|zip|rar|7z|nzb|torrent)$"
    );
    if let Some(c) = EXT.captures(&s) {
        if let Some(tag) = word_tag(&c[1], true) {
            t.apply(tag);
        }
        let start = c.get(0).map_or(s.len(), |m| m.start());
        s.truncate(start);
    }
    extract_ids(&mut s, &mut t);

    let body = if !s.contains(' ') && (s.contains('_') || scene_dashes(&s)) {
        scene_segments(&s, &mut t)
    } else {
        let dotted = !s.contains(' ') && s.contains('.');
        if dotted {
            s = text::undot(&s);
        }
        spaced_segments(&s, dotted, &mut t)
    };
    let names = assign(body, &mut t);
    re!(
        COLLECTION,
        r"(?i)\b(?:collection|omnibus|box(?:ed)?\s*set|complete\s+(?:series|collection|saga|works|novels)|trilogy|quartet|books?\s*\d+\s*(?:-|to)\s*\d+)\b|#\d+\s*-\s*\d+"
    );
    if COLLECTION.is_match(&names.title) || names.author.as_deref().is_some_and(|a| COLLECTION.is_match(a)) {
        t.collection = true;
    }

    // Format and quality.
    let ext_family = |q: &Quality| {
        if q.family() == QualityFamily::Audiobook {
            BookFormat::Audiobook
        } else {
            BookFormat::Ebook
        }
    };
    let evidence = t
        .qualities
        .first()
        .map(ext_family)
        .or(t.ebook_marker.then_some(BookFormat::Ebook))
        .or(
            (t.audio_marker || t.narrator.is_some() || t.unabridged.is_some() || t.bitrate.is_some())
                .then_some(BookFormat::Audiobook),
        );
    let format = format_hint.or(evidence);
    let quality = match format {
        Some(f) => {
            let family = QualityFamily::of_book(f);
            let ranked = family.qualities();
            t.qualities
                .iter()
                .copied()
                .filter(|q| q.family() == family)
                .max_by_key(|q| ranked.iter().position(|r| r == q))
                .unwrap_or(ranked[0])
        }
        None => Quality::UnknownText,
    };
    if format == Some(BookFormat::Audiobook) && t.narrator.is_none() {
        t.narrator = t.narrator_candidate.take().map(|(_, n)| n);
    }
    if format == Some(BookFormat::Ebook) {
        t.narrator = None;
    }

    ParsedBook {
        raw_title: title.to_string(),
        author: names.author,
        title: names.title,
        series: t.series,
        series_position: t.position,
        year: t.year,
        format,
        quality: QualityModel::new(quality),
        narrator: t.narrator,
        bitrate_kbps: t.bitrate,
        unabridged: t.unabridged,
        isbn: t.isbn,
        asin: t.asin,
        release_group: t.group,
        languages: t.languages,
        is_collection: t.collection,
    }
}

/// `epub|azw3|azw|mobi|pdf` → ebook qualities; `m4b|m4a|aac` → `AudiobookM4b`;
/// `mp3` → `AudiobookMp3`; `flac` → `AudiobookFlac`. Case-insensitive, leading dot allowed.
pub fn book_quality_from_extension(ext: &str) -> Option<Quality> {
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    Some(match ext.as_str() {
        "epub" => Quality::Epub,
        "azw3" | "azw" => Quality::Azw3,
        "mobi" => Quality::Mobi,
        "pdf" => Quality::Pdf,
        "m4b" | "m4a" | "aac" => Quality::AudiobookM4b,
        "mp3" => Quality::AudiobookMp3,
        "flac" => Quality::AudiobookFlac,
        _ => return None,
    })
}

/// What the title has told us so far.
#[derive(Debug, Default)]
struct Tags {
    qualities: Vec<Quality>,
    ebook_marker: bool,
    audio_marker: bool,
    year: Option<u16>,
    unabridged: Option<bool>,
    bitrate: Option<u32>,
    languages: Vec<Language>,
    narrator: Option<String>,
    /// Name-like bracket text; `[...]` (the usual place for narrators) beats `(...)`.
    narrator_candidate: Option<(bool, String)>,
    series: Option<String>,
    position: Option<String>,
    collection: bool,
    isbn: Option<String>,
    asin: Option<String>,
    group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tag {
    Fmt(Quality),
    Ebook,
    Audio,
    Year(u16),
    Retail,
    Abridged(bool),
    Bitrate(u32),
    Lang(Language),
    Collection,
    Noise,
}

impl Tag {
    /// Tags trusted at the end of free text (not inside brackets or scene fields).
    fn strong(self) -> bool {
        !matches!(self, Tag::Lang(_) | Tag::Noise | Tag::Collection)
    }
}

impl Tags {
    fn apply(&mut self, tag: Tag) {
        match tag {
            Tag::Fmt(q) => {
                if !self.qualities.contains(&q) {
                    self.qualities.push(q);
                }
            }
            Tag::Ebook => self.ebook_marker = true,
            Tag::Audio => self.audio_marker = true,
            Tag::Year(y) => {
                self.year.get_or_insert(y);
            }
            Tag::Abridged(u) => {
                self.unabridged.get_or_insert(u);
            }
            Tag::Bitrate(b) => {
                self.bitrate.get_or_insert(b);
            }
            Tag::Lang(l) => {
                if !self.languages.contains(&l) {
                    self.languages.push(l);
                }
            }
            Tag::Collection => self.collection = true,
            Tag::Retail | Tag::Noise => {}
        }
    }

    fn set_series(&mut self, name: Option<&str>, pos: Option<String>) {
        if self.series.is_none() && self.position.is_none() {
            self.series = name.map(text::tidy).filter(|n| !n.is_empty());
            self.position = pos;
        }
    }
}

/// Classifies one word. `loose` also accepts short language codes and noise words, which are
/// only trusted inside brackets and scene fields.
fn word_tag(w: &str, loose: bool) -> Option<Tag> {
    re!(BITRATE, r"(?i)^@?(\d{2,3})\s*(?:k|kbps|kbit|kbits|kb/s|kbit/s)$");
    let w = w.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '.' | '!' | '*'));
    if w.is_empty() {
        return None;
    }
    let lower = w.to_lowercase();
    if let Some(q) = book_quality_from_extension(&lower) {
        return Some(Tag::Fmt(q));
    }
    let tag = match lower.as_str() {
        "kf8" | "kfx" => Tag::Fmt(Quality::Azw3),
        "cbz" | "cbr" | "cb7" | "djvu" | "fb2" | "lit" | "rtf" | "ebook" | "ebooks" | "e-book" | "kindle" => {
            Tag::Ebook
        }
        "audiobook" | "audiobooks" | "audiobk" | "abook" | "ogg" | "opus" | "wma" | "audible"
        | "dramatized" | "dramatised" | "hoerbuch" | "hörbuch" | "hoerspiel" | "hörspiel" => Tag::Audio,
        "retail" => Tag::Retail,
        "unabridged" | "ungekuerzt" | "ungekürzt" | "integral" | "integrale" => Tag::Abridged(true),
        "abridged" | "gekuerzt" | "gekürzt" => Tag::Abridged(false),
        "collection" | "omnibus" | "boxset" => Tag::Collection,
        _ => {
            if let Some(c) = BITRATE.captures(&lower) {
                return c[1].parse().ok().map(Tag::Bitrate);
            }
            if let Some(y) = text::year(&lower, 1500) {
                return Some(Tag::Year(y));
            }
            if !loose {
                return None;
            }
            if let Some(l) = text::language(&lower, true) {
                return Some(Tag::Lang(l));
            }
            let noise = matches!(
                lower.as_str(),
                "web"
                    | "webrip"
                    | "internal"
                    | "proper"
                    | "repack"
                    | "readnfo"
                    | "read" // READ.NFO
                    | "nfo"
                    | "nfofix"
                    | "dirfix"
                    | "true"
                    | "hq"
                    | "lq"
                    | "ocr"
                    | "scan"
                    | "scanned"
                    | "converted"
                    | "vbr"
                    | "mono"
                    | "stereo"
                    | "chapterized"
                    | "chaptered"
                    | "multiformat"
                    | "mp3cd"
            ) || is_version(&lower);
            return noise.then_some(Tag::Noise);
        }
    };
    Some(tag)
}

fn is_version(w: &str) -> bool {
    w.strip_prefix('v').is_some_and(|r| {
        !r.is_empty()
            && r.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Classifies all words of a bracket or scene field; `None` unless every word is a tag.
fn all_tags(content: &str, loose: bool) -> Option<Vec<Tag>> {
    re!(SPACED_RATE, r"(?i)(\d{2,3})\s+(kbps|kbit|kb/s|k)\b");
    re!(AUDIO_BOOK, r"(?i)\baudio\s*books?\b|\be-?\s?books?\b");
    let joined = SPACED_RATE.replace_all(content, "$1$2");
    let joined = AUDIO_BOOK.replace_all(&joined, |c: &regex::Captures<'_>| {
        if c[0].to_lowercase().starts_with('a') {
            "audiobook"
        } else {
            "ebook"
        }
        .to_string()
    });
    let words: Vec<&str> = joined
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ';' | '/' | '+' | '|' | '@' | '&' | '-' | '_')
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    let tags: Vec<Option<Tag>> = words.iter().map(|w| word_tag(w, loose)).collect();
    // A bare bitrate next to an audio format: `[mp3 128]`.
    let audio = tags
        .iter()
        .any(|t| matches!(t, Some(Tag::Fmt(q)) if q.family() == QualityFamily::Audiobook));
    words
        .iter()
        .zip(tags)
        .map(|(w, tag)| {
            tag.or_else(|| {
                let rate: u32 = w.parse().ok().filter(|_| audio)?;
                [32, 48, 56, 62, 64, 96, 112, 128, 160, 192, 256, 320]
                    .contains(&rate)
                    .then_some(Tag::Bitrate(rate))
            })
        })
        .collect()
}

/// Strips ASIN and ISBN anywhere in the title.
fn extract_ids(s: &mut String, t: &mut Tags) {
    re!(ASIN, r"(?i)\basin[\s:#=-]*([a-z0-9]{10})\b|\b(B0[0-9A-Z]{8})\b");
    re!(
        ISBN,
        r"(?i)(?:\bisbn(?:-?1[03])?[\s:#=-]*)?\b(97[89](?:[\s-]?\d){10}|\d(?:[\s-]?\d){8}[\s-]?[\dx])\b"
    );
    if let Some(c) = ASIN.captures(s) {
        let id = c.get(1).or(c.get(2)).map(|m| m.as_str().to_ascii_uppercase());
        let range = c.get(0).map(|m| m.range());
        if let (Some(id), Some(range)) = (id, range) {
            t.asin = Some(id);
            s.replace_range(range, " ");
        }
    }
    let mut found = None;
    for c in ISBN.captures_iter(s) {
        let digits: String = c[1].chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if isbn_valid(&digits) {
            found = c.get(0).map(|m| (m.range(), digits.to_ascii_uppercase()));
            break;
        }
    }
    if let Some((range, isbn)) = found {
        t.isbn = Some(isbn);
        s.replace_range(range, " ");
    }
}

fn isbn_valid(d: &str) -> bool {
    let b = d.as_bytes();
    let digit = |c: u8| u32::from(c.wrapping_sub(b'0'));
    match b.len() {
        13 if b.iter().all(u8::is_ascii_digit) => {
            let sum: u32 = b
                .iter()
                .enumerate()
                .map(|(i, &c)| digit(c) * if i % 2 == 0 { 1 } else { 3 })
                .sum();
            sum.is_multiple_of(10)
        }
        10 if b[..9].iter().all(u8::is_ascii_digit)
            && (b[9].is_ascii_digit() || b[9] == b'X' || b[9] == b'x') =>
        {
            let sum: u32 = b
                .iter()
                .enumerate()
                .map(|(i, &c)| {
                    let v = if c.is_ascii_digit() { digit(c) } else { 10 };
                    v * (10 - i as u32)
                })
                .sum();
            sum.is_multiple_of(11)
        }
        _ => false,
    }
}

/// Dash-separated scene name: at least three fields and something that looks like a tag.
fn scene_dashes(s: &str) -> bool {
    let fields: Vec<&str> = s.split('-').filter(|f| !f.is_empty()).collect();
    fields.len() >= 3 && fields.iter().any(|f| word_tag(f, false).is_some())
}

/// `Author_-_Title-AUDIOBOOK-WEB-EN-2022-GROUP` style names.
fn scene_segments(s: &str, t: &mut Tags) -> Vec<String> {
    let s = s.replace("_-_", "-");
    let mut fields: Vec<&str> = s
        .split('-')
        .map(|f| f.trim_matches('_'))
        .filter(|f| !f.is_empty())
        .collect();
    if fields.len() >= 3
        && let Some(last) = fields.last().filter(|g| looks_like_group(g))
    {
        t.group = Some((*last).to_string());
        fields.pop();
    }
    let mut body = Vec::new();
    for f in fields {
        let spaced = text::undot(&f.replace('_', " "));
        if let Some(tags) = all_tags(&spaced, true) {
            tags.into_iter().for_each(|tag| t.apply(tag));
            continue;
        }
        let rest = take_brackets(&spaced, t);
        let rest = text::tidy(&rest);
        if !rest.is_empty() {
            body.push(rest);
        }
    }
    body
}

fn looks_like_group(g: &str) -> bool {
    !g.is_empty()
        && g.len() <= 24
        && g.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(
            word_tag(g, false),
            Some(Tag::Fmt(_) | Tag::Year(_) | Tag::Ebook | Tag::Audio)
        )
}

/// Spaced (or undotted) names: `Author - Series 01 - Title (2006) [EPUB]-GRP`.
fn spaced_segments(s: &str, scene: bool, t: &mut Tags) -> Vec<String> {
    re!(GROUP, r"([^\s-])-([A-Za-z0-9]{2,20})$");
    let mut s = s.to_string();
    if let Some(c) = GROUP.captures(&s) {
        let (Some(m), Some(g)) = (c.get(0), c.get(2)) else {
            return Vec::new();
        };
        let before = &s[..m.start() + c[1].len()];
        let last_word = before.rsplit([' ', '.']).next().unwrap_or("");
        if before.ends_with([']', ')', '}']) || word_tag(last_word, true).is_some() {
            t.group = Some(g.as_str().to_string());
            s.truncate(before.len());
        }
    }
    let rest = take_brackets(&s, t);
    let mut segs = text::segments(&rest);
    // Whole segments of tags (`Author - Title - EPUB`), keeping at least author and title
    // (`George Orwell - 1984`).
    let mut i = 0;
    while i < segs.len() {
        if segs.len() > 2
            && i > 0
            && let Some(tags) = all_tags(&segs[i], true)
        {
            tags.into_iter().for_each(|tag| t.apply(tag));
            segs.remove(i);
            continue;
        }
        if let Some(n) = narrator_of(&segs[i])
            && segs.len() > 1
        {
            t.narrator.get_or_insert(n);
            segs.remove(i);
            continue;
        }
        i += 1;
    }
    // Tags trailing the last segment: `Title 2010 RETAIL EPUB eBook`.
    if let Some(last) = segs.last_mut() {
        *last = strip_tail(last, scene, t);
    }
    // Inline `... narrated by X` / `... read by X`.
    for seg in &mut segs {
        re!(INLINE_NARR, r"(?i)\s+(?:narrated|read|performed)\s+by\s+(.+)$");
        if let Some(c) = INLINE_NARR.captures(seg) {
            let name = text::tidy(&c[1]);
            let start = c.get(0).map_or(seg.len(), |m| m.start());
            if !name.is_empty() && start > 0 {
                t.narrator.get_or_insert(name);
                seg.truncate(start);
            }
        }
    }
    segs.retain(|s| !s.is_empty());
    segs
}

fn strip_tail(seg: &str, scene: bool, t: &mut Tags) -> String {
    re!(SPACED_RATE, r"(?i)(\d{2,3})\s+(kbps|kbit|kb/s)\b");
    let seg = SPACED_RATE.replace_all(seg, "$1$2");
    let mut words: Vec<&str> = seg.split(' ').collect();
    while words.len() > 1 {
        let Some(last) = words.last() else { break };
        let tags: Option<Vec<Tag>> = last
            .split(['/', '+', ','])
            .filter(|w| !w.is_empty())
            .map(|w| word_tag(w, scene).filter(|tag| scene || tag.strong()))
            .collect();
        match tags {
            Some(tags) if !tags.is_empty() => {
                tags.into_iter().for_each(|tag| t.apply(tag));
                words.pop();
            }
            _ => break,
        }
    }
    text::tidy(&words.join(" "))
}

/// Classifies every bracket group (removing it) and returns the remaining text.
fn take_brackets(s: &str, t: &mut Tags) -> String {
    let mut out = String::with_capacity(s.len());
    for piece in text::pieces(s) {
        match piece {
            Piece::Text(x) => out.push_str(x),
            Piece::Group { open, inner } => {
                classify_bracket(open, inner.trim(), t);
                out.push(' ');
            }
        }
    }
    out
}

fn narrator_of(c: &str) -> Option<String> {
    re!(
        NARR_BY,
        r"(?i)^(?:narrated|read|performed|narration|narr\.?)\s+by\s*:?\s*(.+)$"
    );
    re!(
        NARR_PREFIX,
        r"(?i)^(?:narrator|narrators|narr|narr\.|nar\.|reader)\s*[:\-]?\s*(.+)$"
    );
    re!(NARR_SUFFIX, r"(?i)^(.+?)\s*[(\-,]\s*(?:narrator|narr\.?)\)?$");
    let c = c.trim();
    [&NARR_BY, &NARR_PREFIX, &NARR_SUFFIX]
        .iter()
        .find_map(|r| r.captures(c))
        .map(|cap| text::tidy(&cap[1]))
        .filter(|n| !n.is_empty())
}

/// Series position: leading zeros dropped (`01` → `1`, `02.5` → `2.5`).
fn position(p: &str) -> String {
    let (int, frac) = p.split_once('.').unwrap_or((p, ""));
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    }
}

fn classify_bracket(open: char, c: &str, t: &mut Tags) {
    re!(
        RANGE,
        r"(?i)^(?:(.+?)[,:\s]+)?(?:books?|bks?\.?|vol(?:ume)?s?\.?|#|novels?|parts?)\s*(\d{1,3})\s*(?:-|to|&|thru|through)\s*#?(\d{1,3})$"
    );
    re!(
        COLLECTION,
        r"(?i)\b(?:complete\s+(?:series|collection|saga|trilogy|works|novels|set)|box(?:ed)?\s*set|collection|omnibus|series\s+\d+\s*-\s*\d+)\b"
    );
    re!(
        SERIES,
        r"(?i)^(?:(.+?)[,:\s]*)?(?:#|book\s*|bk\.?\s*|vol(?:ume)?\.?\s*|no\.?\s*|n°\s*)(\d{1,4}(?:\.\d{1,2})?)$"
    );
    re!(
        SERIES_BARE,
        r"^(\p{L}[^\d()\[\]]*?)[,\s]+(\d{1,3}(?:\.\d{1,2})?)$"
    );
    if c.is_empty() {
        return;
    }
    if let Some(n) = narrator_of(c) {
        t.narrator.get_or_insert(n);
        return;
    }
    if let Some(cap) = RANGE.captures(c) {
        t.collection = true;
        let pos = format!("{}-{}", position(&cap[2]), position(&cap[3]));
        t.set_series(cap.get(1).map(|m| m.as_str()), Some(pos));
        return;
    }
    if let Some(tags) = all_tags(c, true) {
        tags.into_iter().for_each(|tag| t.apply(tag));
        return;
    }
    if COLLECTION.is_match(c) {
        t.collection = true;
        return;
    }
    if let Some(cap) = SERIES.captures(c) {
        t.set_series(cap.get(1).map(|m| m.as_str()), Some(position(&cap[2])));
        return;
    }
    if let Some(cap) = SERIES_BARE.captures(c) {
        t.set_series(Some(&cap[1]), Some(position(&cap[2])));
        return;
    }
    let square = open == '[';
    if text::name_like(c, 8) && t.narrator_candidate.as_ref().is_none_or(|(sq, _)| square && !sq) {
        t.narrator_candidate = Some((square, text::tidy(c)));
    }
}

/// Author, title and series from the body segments.
struct Names {
    author: Option<String>,
    title: String,
}

/// `Series 01`, `Series #1`, `Series, Book 1`, `Book 1`; `marker` requires `#`/`Book`/`Vol`.
fn series_of(seg: &str, marker: bool) -> Option<(Option<String>, String)> {
    re!(
        MARKED,
        r"(?i)^(.*?)[,:\s]*(?:#|book\s*|bk\.?\s*|vol(?:ume)?\.?\s*|no\.?\s*|part\s+)(\d{1,3}(?:\.\d{1,2})?)$"
    );
    re!(
        BARE,
        r"^(.*?)[,:\s]+(\d{1,3}(?:\.\d{1,2})?)$|^()#?(\d{1,3}(?:\.\d{1,2})?)$"
    );
    let cap = MARKED
        .captures(seg)
        .or_else(|| if marker { None } else { BARE.captures(seg) })?;
    let name = cap
        .get(1)
        .or(cap.get(3))
        .map(|m| text::tidy(m.as_str()))
        .filter(|n| !n.is_empty());
    let num = cap.get(2).or(cap.get(4))?;
    Some((name, position(num.as_str())))
}

fn assign(mut segs: Vec<String>, t: &mut Tags) -> Names {
    let mut author = None;
    // `Title by Author`.
    if let Some(last) = segs.last_mut() {
        let lower = last.to_lowercase();
        if let Some(i) = lower.rfind(" by ") {
            let (left, right) = (last.get(..i).unwrap_or(""), last.get(i + 4..).unwrap_or(""));
            let right = text::tidy(right);
            if !left.trim().is_empty() && text::name_like(&right, 7) && right.split_whitespace().count() >= 2
            {
                author = Some(right);
                *last = text::tidy(left);
            }
        }
    }
    let title = if author.is_some() {
        match segs.len() {
            0 => String::new(),
            1 => segs.remove(0),
            _ => {
                if let Some((name, pos)) = series_of(&segs[0], false) {
                    t.set_series(name.as_deref(), Some(pos));
                } else {
                    t.set_series(Some(&segs[0]), None);
                }
                segs[1..].join(" - ")
            }
        }
    } else {
        match segs.len() {
            0 => String::new(),
            1 => segs.remove(0),
            2 => {
                if let Some((Some(name), pos)) = series_of(&segs[0], false) {
                    t.set_series(Some(&name), Some(pos));
                } else {
                    author = Some(segs[0].clone());
                }
                segs[1].clone()
            }
            n => {
                author = Some(segs[0].clone());
                if let Some((name, pos)) = series_of(&segs[1], false) {
                    t.set_series(name.as_deref(), Some(pos));
                    segs[2..].join(" - ")
                } else if n >= 4 && series_of(&segs[2], false).is_some_and(|(name, _)| name.is_none()) {
                    let pos = series_of(&segs[2], false).map(|(_, p)| p);
                    t.set_series(Some(&segs[1]), pos);
                    segs[3..].join(" - ")
                } else if let Some((name, pos)) = series_of(&segs[n - 1], false) {
                    t.set_series(name.as_deref(), Some(pos));
                    segs[1..n - 1].join(" - ")
                } else {
                    t.set_series(Some(&segs[1]), None);
                    segs[2..].join(" - ")
                }
            }
        }
    };
    Names {
        author: author.map(|a| flip_author(&a)).filter(|a| !a.is_empty()),
        title: text::tidy(&title),
    }
}

/// `Sanderson, Brandon` → `Brandon Sanderson`; lists of authors are left alone.
fn flip_author(a: &str) -> String {
    let a = text::tidy(a);
    let Some((last, first)) = a.split_once(", ") else {
        return a;
    };
    let last_words: Vec<&str> = last.split(' ').collect();
    let particle = |w: &str| {
        matches!(
            w.to_lowercase().as_str(),
            "le" | "de" | "van" | "von" | "del" | "di" | "da" | "du" | "la"
        )
    };
    let surname = last_words.len() == 1 || (last_words.len() == 2 && particle(last_words[0]));
    if surname && !first.contains([',', '&']) && first.split(' ').count() <= 3 && !first.contains(" and ") {
        format!("{first} {last}")
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_qualities() {
        assert_eq!(book_quality_from_extension(".EPUB"), Some(Quality::Epub));
        assert_eq!(book_quality_from_extension("m4a"), Some(Quality::AudiobookM4b));
        assert_eq!(book_quality_from_extension("mkv"), None);
    }

    #[test]
    fn tracker_style() {
        let b = parse_book(
            "  Brandon Sanderson - Mistborn 01 - The Final Empire (2006) [EPUB] ",
            None,
        );
        assert_eq!(b.author.as_deref(), Some("Brandon Sanderson"));
        assert_eq!(b.title, "The Final Empire");
        assert_eq!(b.series.as_deref(), Some("Mistborn"));
        assert_eq!(b.series_position.as_deref(), Some("1"));
        assert_eq!(b.year, Some(2006));
        assert_eq!(b.quality.quality, Quality::Epub);
        assert_eq!(b.format, Some(BookFormat::Ebook));
        let a = parse_book("Dune (m4b)", None);
        assert_eq!(a.format, Some(BookFormat::Audiobook));
        let u = parse_book("Dune", Some(BookFormat::Audiobook));
        assert_eq!(u.quality.quality, Quality::UnknownAudiobook);
        assert_eq!(parse_book("Dune", None).quality.quality, Quality::UnknownText);
        // A hint from the other family wins over the tag.
        let m = parse_book("Dune [mp3]", Some(BookFormat::Ebook));
        assert_eq!(m.quality.quality, Quality::UnknownText);
    }

    #[test]
    fn audiobook_fields() {
        let b = parse_book(
            "Andy Weir - Project Hail Mary (2021) [Ray Porter] [Unabridged] [ASIN B08G9PRS1K] [M4B 64kbps]",
            None,
        );
        assert_eq!(b.narrator.as_deref(), Some("Ray Porter"));
        assert_eq!(b.unabridged, Some(true));
        assert_eq!(b.asin.as_deref(), Some("B08G9PRS1K"));
        assert_eq!(b.bitrate_kbps, Some(64));
        assert_eq!(b.quality.quality, Quality::AudiobookM4b);
        assert_eq!(b.title, "Project Hail Mary");
    }

    #[test]
    fn isbn_checksum_and_retail() {
        assert!(isbn_valid("9780441013593"));
        assert!(!isbn_valid("9780441013594"));
        assert!(isbn_valid("044101359X") || !isbn_valid("0441013590"));
        let b = parse_book("Frank Herbert - Dune [9780441013593] (Retail).epub", None);
        assert_eq!(b.isbn.as_deref(), Some("9780441013593"));
        assert!(b.is_retail());
        assert_eq!(b.title, "Dune");
    }

    #[test]
    fn to_release_title() {
        let r = parse_book("Frank Herbert - Dune [epub]", None).to_release();
        assert_eq!(r.title, "Frank Herbert Dune");
        assert_eq!(r.quality.quality, Quality::Epub);
    }
}
