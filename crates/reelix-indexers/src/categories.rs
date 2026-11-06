//! The Newznab/Torznab standard category table.

use reelix_core::ContentKind;

use crate::model::{Category, ReleaseInfo, SearchQuery};

/// `(id, parent name, sub name)`; parent rows have `sub == ""`.
const TABLE: &[(u32, &str, &str)] = &[
    (1000, "Console", ""),
    (1010, "Console", "NDS"),
    (1020, "Console", "PSP"),
    (1030, "Console", "Wii"),
    (1040, "Console", "XBox"),
    (1050, "Console", "XBox 360"),
    (1060, "Console", "Wiiware"),
    (1070, "Console", "XBox 360 DLC"),
    (1080, "Console", "PS3"),
    (1090, "Console", "Other"),
    (1110, "Console", "3DS"),
    (1120, "Console", "PS Vita"),
    (1130, "Console", "WiiU"),
    (1140, "Console", "XBox One"),
    (1180, "Console", "PS4"),
    (2000, "Movies", ""),
    (2010, "Movies", "Foreign"),
    (2020, "Movies", "Other"),
    (2030, "Movies", "SD"),
    (2040, "Movies", "HD"),
    (2045, "Movies", "UHD"),
    (2050, "Movies", "BluRay"),
    (2060, "Movies", "3D"),
    (2070, "Movies", "DVD"),
    (2080, "Movies", "WEB-DL"),
    (3000, "Audio", ""),
    (3010, "Audio", "MP3"),
    (3020, "Audio", "Video"),
    (3030, "Audio", "Audiobook"),
    (3040, "Audio", "Lossless"),
    (3050, "Audio", "Other"),
    (3060, "Audio", "Foreign"),
    (4000, "PC", ""),
    (4010, "PC", "0day"),
    (4020, "PC", "ISO"),
    (4030, "PC", "Mac"),
    (4040, "PC", "Mobile-Other"),
    (4050, "PC", "Games"),
    (4060, "PC", "Mobile-iOS"),
    (4070, "PC", "Mobile-Android"),
    (5000, "TV", ""),
    (5010, "TV", "WEB-DL"),
    (5020, "TV", "Foreign"),
    (5030, "TV", "SD"),
    (5040, "TV", "HD"),
    (5045, "TV", "UHD"),
    (5050, "TV", "Other"),
    (5060, "TV", "Sport"),
    (5070, "TV", "Anime"),
    (5080, "TV", "Documentary"),
    (6000, "XXX", ""),
    (6010, "XXX", "DVD"),
    (6020, "XXX", "WMV"),
    (6030, "XXX", "XviD"),
    (6040, "XXX", "x264"),
    (6045, "XXX", "UHD"),
    (6050, "XXX", "Pack"),
    (6060, "XXX", "ImageSet"),
    (6070, "XXX", "Other"),
    (7000, "Books", ""),
    (7010, "Books", "Mags"),
    (7020, "Books", "EBook"),
    (7030, "Books", "Comics"),
    (7040, "Books", "Technical"),
    (7050, "Books", "Other"),
    (7060, "Books", "Foreign"),
    (8000, "Other", ""),
    (8010, "Other", "Misc"),
    (8020, "Other", "Hashed"),
];

/// Well-known parent ids.
pub const MOVIES: u32 = 2000;
pub const TV: u32 = 5000;
pub const TV_ANIME: u32 = 5070;
pub const AUDIO: u32 = 3000;
pub const AUDIO_MP3: u32 = 3010;
pub const AUDIO_VIDEO: u32 = 3020;
pub const AUDIO_AUDIOBOOK: u32 = 3030;
pub const AUDIO_LOSSLESS: u32 = 3040;
pub const AUDIO_OTHER: u32 = 3050;
pub const AUDIO_FOREIGN: u32 = 3060;
pub const BOOKS: u32 = 7000;
pub const BOOKS_MAGS: u32 = 7010;
pub const BOOKS_EBOOK: u32 = 7020;
pub const BOOKS_COMICS: u32 = 7030;

/// Default ebook categories (Books, Books/EBook). Mags are only returned when selected.
pub const DEFAULT_EBOOK: &[u32] = &[BOOKS, BOOKS_EBOOK];
/// Default audiobook categories (Audio/Audiobook).
pub const DEFAULT_AUDIOBOOK: &[u32] = &[AUDIO_AUDIOBOOK];
/// Default book categories (ebook + audiobook).
pub const DEFAULT_BOOK: &[u32] = &[BOOKS, BOOKS_EBOOK, AUDIO_AUDIOBOOK];
/// Default music categories (Audio, MP3, Lossless). Audio/Video and audiobooks are excluded.
pub const DEFAULT_MUSIC: &[u32] = &[AUDIO, AUDIO_MP3, AUDIO_LOSSLESS];
/// Default radio categories. Radio shows and podcasts have no category of their own, so
/// indexers file them as Audio/Other, Audio/MP3 or Audio/Audiobook (MyAnonamouse's Radio
/// categories map to Audio/Audiobook in Prowlarr and Jackett). Releases still have to match a
/// wanted episode, so music and audiobooks found here are not grabbed as radio. The Audio parent
/// is left out: on many indexers it would also return Lossless and Audio/Video.
pub const DEFAULT_RADIO: &[u32] = &[AUDIO_MP3, AUDIO_AUDIOBOOK, AUDIO_OTHER];

/// Default search categories for a content kind (`None` = format-less book search).
pub fn defaults_for(kind: Option<ContentKind>) -> &'static [u32] {
    match kind {
        Some(ContentKind::Movie) => &[MOVIES],
        Some(ContentKind::Series) => &[TV],
        Some(ContentKind::Ebook) => DEFAULT_EBOOK,
        Some(ContentKind::Audiobook) => DEFAULT_AUDIOBOOK,
        None => DEFAULT_BOOK,
        Some(ContentKind::Music) => DEFAULT_MUSIC,
        Some(ContentKind::Radio) => DEFAULT_RADIO,
    }
}

/// Subcategories dropped from results of a content search unless they were explicitly asked for
/// (a parent such as `7000` or `3000` also returns them).
pub fn excluded_for(kind: Option<ContentKind>) -> &'static [u32] {
    match kind {
        Some(ContentKind::Ebook) | None => &[BOOKS_MAGS],
        Some(ContentKind::Audiobook) => &[],
        Some(ContentKind::Music) => &[AUDIO_VIDEO, AUDIO_AUDIOBOOK],
        Some(ContentKind::Radio) => &[AUDIO_VIDEO],
        Some(ContentKind::Movie | ContentKind::Series) => &[],
    }
}

/// True when every specific (non-parent) category of a release is excluded for `kind` and not
/// in `requested`. Releases without categories, or with only parent ids, are kept.
pub fn is_excluded(kind: Option<ContentKind>, requested: &[u32], release: &[u32]) -> bool {
    let excluded = excluded_for(kind);
    let specific: Vec<u32> = release
        .iter()
        .copied()
        .filter(|&c| c >= 100_000 || !c.is_multiple_of(1000))
        .collect();
    !specific.is_empty()
        && specific
            .iter()
            .all(|c| excluded.contains(c) && !requested.contains(c))
}

/// Drops results of a book/music query that only sit in excluded categories (Mags, Audio/Video,
/// audiobooks in a music search). Other query kinds are returned unchanged.
pub fn filter_excluded(q: &SearchQuery, requested: &[u32], releases: &mut Vec<ReleaseInfo>) {
    use crate::model::SearchKind;
    if !matches!(q.kind, SearchKind::Book | SearchKind::Music) {
        return;
    }
    let kind = q.content_kind();
    releases.retain(|r| !is_excluded(kind, requested, &r.categories));
}

/// The content a release's categories point at: Movies → movie, TV → series,
/// Audio/Audiobook → audiobook, Books (except Mags/Comics) → ebook, other Audio (except Video) →
/// music. Site-specific ids (≥ 100000) are ignored; specific subcategories win over parents.
pub fn content_kind_of(categories: &[u32]) -> Option<ContentKind> {
    let of = |c: u32| -> Option<ContentKind> {
        match c {
            AUDIO_AUDIOBOOK => Some(ContentKind::Audiobook),
            BOOKS_MAGS | BOOKS_COMICS | AUDIO_VIDEO => None,
            c if c >= 100_000 => None,
            c => match parent(c) {
                MOVIES => Some(ContentKind::Movie),
                TV => Some(ContentKind::Series),
                BOOKS => Some(ContentKind::Ebook),
                AUDIO => Some(ContentKind::Music),
                _ => None,
            },
        }
    };
    let specific = categories.iter().copied().filter(|c| !c.is_multiple_of(1000));
    specific
        .filter_map(of)
        .next()
        .or_else(|| categories.iter().copied().filter_map(of).next())
}

/// The full standard category tree.
pub fn standard() -> Vec<Category> {
    let mut out: Vec<Category> = Vec::new();
    for &(id, parent, sub) in TABLE {
        if sub.is_empty() {
            out.push(Category::new(id, parent));
        } else if let Some(p) = out.last_mut() {
            p.subcategories
                .push(Category::new(id, &format!("{parent}/{sub}")));
        }
    }
    out
}

/// Display name for a standard id, e.g. `2040` → `Movies/HD`.
pub fn name(id: u32) -> Option<String> {
    TABLE.iter().find(|r| r.0 == id).map(|&(_, p, s)| {
        if s.is_empty() {
            p.to_string()
        } else {
            format!("{p}/{s}")
        }
    })
}

/// Resolves a `Parent/Sub` (or `Parent`) name, case-insensitively, to its standard id.
pub fn by_name(name: &str) -> Option<u32> {
    let (p, s) = name.split_once('/').unwrap_or((name, ""));
    TABLE
        .iter()
        .find(|r| r.1.eq_ignore_ascii_case(p.trim()) && r.2.eq_ignore_ascii_case(s.trim()))
        .map(|r| r.0)
}

/// Parent of a standard (or custom ≥ 100000) id: `2040` → `2000`.
pub fn parent(id: u32) -> u32 {
    if id >= 100_000 { id } else { id / 1000 * 1000 }
}

/// True when `id` equals `wanted` or is one of its subcategories.
pub fn matches(wanted: u32, id: u32) -> bool {
    wanted == id || (wanted.is_multiple_of(1000) && wanted < 100_000 && parent(id) == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lookups() {
        assert_eq!(name(2040).as_deref(), Some("Movies/HD"));
        assert_eq!(by_name("tv/anime"), Some(5070));
        assert_eq!(by_name("Movies"), Some(2000));
        assert!(matches(5000, 5040) && !matches(5040, 5000) && !matches(2000, 5040));
        let tree = standard();
        assert_eq!(tree.len(), 8);
        assert!(tree[1].subcategories.iter().any(|c| c.id == 2045));
        assert_eq!(by_name("Books/EBook"), Some(BOOKS_EBOOK));
        assert_eq!(by_name("Audio/Audiobook"), Some(AUDIO_AUDIOBOOK));
        assert_eq!(name(AUDIO_LOSSLESS).as_deref(), Some("Audio/Lossless"));
    }

    #[test]
    fn book_and_music_mapping() {
        use ContentKind::*;
        assert_eq!(defaults_for(Some(Ebook)), [7000, 7020]);
        assert_eq!(defaults_for(Some(Audiobook)), [3030]);
        assert_eq!(defaults_for(Some(Music)), [3000, 3010, 3040]);
        assert!(!defaults_for(Some(Music)).contains(&AUDIO_VIDEO));
        // Mags are excluded from book searches unless requested.
        assert!(is_excluded(Some(Ebook), DEFAULT_EBOOK, &[7000, 7010]));
        assert!(!is_excluded(Some(Ebook), &[7000, 7010], &[7010]));
        assert!(!is_excluded(Some(Ebook), DEFAULT_EBOOK, &[7000, 7020]));
        assert!(!is_excluded(Some(Ebook), DEFAULT_EBOOK, &[7000]));
        assert!(!is_excluded(Some(Ebook), DEFAULT_EBOOK, &[]));
        assert!(is_excluded(Some(Music), DEFAULT_MUSIC, &[3020]));
        assert!(is_excluded(Some(Music), DEFAULT_MUSIC, &[3000, 3030]));
        assert!(!is_excluded(Some(Music), DEFAULT_MUSIC, &[3040]));
        assert!(!is_excluded(Some(Audiobook), DEFAULT_AUDIOBOOK, &[3030]));
        assert!(!is_excluded(Some(Movie), &[], &[7010]));

        // Radio: Audio/MP3, Audio/Audiobook and Audio/Other; Audio/Video stays out.
        assert_eq!(defaults_for(Some(Radio)), [3010, 3030, 3050]);
        assert!(!defaults_for(Some(Radio)).contains(&AUDIO_VIDEO));
        assert!(is_excluded(Some(Radio), DEFAULT_RADIO, &[3020]));
        assert!(is_excluded(Some(Radio), DEFAULT_RADIO, &[3000, 3020]));
        for cat in [3010, 3030, 3050] {
            assert!(!is_excluded(Some(Radio), DEFAULT_RADIO, &[3000, cat]), "{cat}");
        }

        assert_eq!(content_kind_of(&[3000, 3030]), Some(Audiobook));
        assert_eq!(content_kind_of(&[7000, 7020]), Some(Ebook));
        assert_eq!(content_kind_of(&[7000]), Some(Ebook));
        assert_eq!(content_kind_of(&[7010]), None);
        assert_eq!(content_kind_of(&[3040, 100_123]), Some(Music));
        assert_eq!(content_kind_of(&[3020]), None);
        assert_eq!(content_kind_of(&[2000, 2040]), Some(Movie));
        assert_eq!(content_kind_of(&[5070]), Some(Series));
        assert_eq!(content_kind_of(&[]), None);
    }
}
