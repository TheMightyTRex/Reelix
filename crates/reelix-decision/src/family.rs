//! Book and music releases in the decision engine.
//!
//! The engine itself is family-agnostic: a book or album release is a [`ReleaseCandidate`]
//! whose `parsed.quality` is a quality of the target's family, evaluated against a profile of
//! that family with the definitions of that family. To evaluate one:
//!
//! ```
//! use reelix_core::{BookFormat, Protocol};
//! use reelix_decision::{DecisionContext, ReleaseCandidate, default_definitions_for, default_profiles_for,
//!     evaluate, size_runtime_minutes};
//! use reelix_parser::QualityFamily;
//!
//! let family = QualityFamily::Ebook;
//! let profile = &default_profiles_for(family)[0];
//! let defs = default_definitions_for(family);
//! let mut c = ReleaseCandidate::for_book("Author - Title (2020) [EPUB]", Protocol::Torrent, BookFormat::Ebook);
//! c.size = 3 * 1024 * 1024;
//! c.media_id = Some(42); // the book id
//! let mut ctx = DecisionContext::new(profile, chrono::Utc::now());
//! ctx.definitions = &defs;
//! ctx.runtime_minutes = size_runtime_minutes(family, None); // ebooks: Some(1), sizes are per book
//! assert!(evaluate(&c, &ctx).accepted);
//! ```
//!
//! - Quality allowed / cutoff / upgrade / custom-format score work unchanged; existing files
//!   are [`ExistingFile`](crate::ExistingFile)s with the stored book/track-file quality (for an
//!   album, the album's lowest track quality).
//! - Size: set `runtime_minutes` to [`size_runtime_minutes`]`(family, duration)`: an
//!   audiobook's duration, an album's summed track durations, `None` when unknown (skips the
//!   check). For ebooks it is always `Some(1)` and `episode_count` is the number of books in the
//!   release (1 unless it's a collection).
//! - Language: book/music default profiles have no language requirement; leave
//!   `original_language` unset unless the profile asks for one.

use reelix_core::{BookFormat, ContentKind, Protocol};
use reelix_parser::{ParsedRelease, Quality, QualityFamily};

use crate::definition::SizeBasis;
use crate::engine::ReleaseCandidate;

/// The quality family used for a content kind (Movie/Series → Video).
pub fn family_for(kind: ContentKind) -> QualityFamily {
    QualityFamily::of_content(kind)
}

/// The family's "unknown" quality, used when a release name carries no format token.
pub fn unknown_quality(family: QualityFamily) -> Quality {
    match family {
        QualityFamily::Video => Quality::Unknown,
        QualityFamily::Ebook => Quality::UnknownText,
        QualityFamily::Audiobook => Quality::UnknownAudiobook,
        QualityFamily::Music => Quality::UnknownMusic,
    }
}

/// Value for [`DecisionContext::runtime_minutes`](crate::DecisionContext) so the size check
/// uses the family's [`SizeBasis`]: the playing time (minutes, `None`/0 = unknown → no size
/// check) for per-minute families, and `Some(1)` for per-book ebooks.
pub fn size_runtime_minutes(family: QualityFamily, duration_minutes: Option<u32>) -> Option<u32> {
    match SizeBasis::of(family) {
        SizeBasis::PerBook => Some(1),
        SizeBasis::PerMinute => duration_minutes.filter(|m| *m > 0),
    }
}

/// Forces `parsed.quality` into `family` (a quality of another family becomes the family's
/// unknown quality, so the profile's own rules decide).
fn in_family(mut parsed: ParsedRelease, family: QualityFamily) -> ParsedRelease {
    if parsed.quality.quality.family() != family {
        parsed.quality.quality = unknown_quality(family);
    }
    parsed
}

impl ReleaseCandidate {
    /// A candidate whose `parsed` is filled in by the caller (no title parsing).
    pub fn with_parsed(title: &str, protocol: Protocol, parsed: ParsedRelease) -> Self {
        Self {
            parsed,
            title: title.to_string(),
            size: 0,
            protocol,
            indexer_id: None,
            indexer: String::new(),
            indexer_priority: 25,
            seeders: None,
            peers: None,
            publish_date: None,
            indexer_flags: Vec::new(),
            download_url: String::new(),
            guid: title.to_string(),
            media_id: None,
            episode_ids: Vec::new(),
        }
    }

    /// A book release, parsed with [`reelix_parser::parse_book`]; its quality is always of the
    /// `format`'s family.
    pub fn for_book(title: &str, protocol: Protocol, format: BookFormat) -> Self {
        let parsed = reelix_parser::parse_book(title, Some(format)).to_release();
        Self::with_parsed(title, protocol, in_family(parsed, QualityFamily::of_book(format)))
    }

    /// A music (album) release, parsed with [`reelix_parser::parse_music`]; its quality is
    /// always a music quality.
    pub fn for_album(title: &str, protocol: Protocol) -> Self {
        let parsed = reelix_parser::parse_music(title).to_release();
        Self::with_parsed(title, protocol, in_family(parsed, QualityFamily::Music))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::default_definitions_for;
    use crate::engine::{DecisionContext, ExistingFile, evaluate};
    use crate::format::{Condition, CustomFormat, FormatSpecification};
    use crate::profile::{FormatItem, default_profiles_for};
    use reelix_parser::QualityModel;

    const MB: u64 = 1024 * 1024;

    fn specs(d: &crate::Decision) -> Vec<&str> {
        d.rejections.iter().map(|r| r.spec.as_str()).collect()
    }

    fn book(title: &str, format: BookFormat, size_mb: u64) -> ReleaseCandidate {
        let mut c = ReleaseCandidate::for_book(title, Protocol::Torrent, format);
        c.size = size_mb * MB;
        c.media_id = Some(1);
        c
    }

    #[test]
    fn helpers() {
        assert_eq!(family_for(ContentKind::Series), QualityFamily::Video);
        assert_eq!(family_for(ContentKind::Audiobook), QualityFamily::Audiobook);
        assert_eq!(size_runtime_minutes(QualityFamily::Ebook, None), Some(1));
        assert_eq!(size_runtime_minutes(QualityFamily::Music, Some(0)), None);
        assert_eq!(size_runtime_minutes(QualityFamily::Music, Some(45)), Some(45));
        for f in QualityFamily::ALL {
            assert_eq!(unknown_quality(f).family(), f);
        }
        // Unrecognised names get the family's unknown quality.
        let c = ReleaseCandidate::for_book("Some Book", Protocol::Usenet, BookFormat::Audiobook);
        assert_eq!(c.parsed.quality.quality, Quality::UnknownAudiobook);
        let c = ReleaseCandidate::for_album("Artist - Album", Protocol::Usenet);
        assert_eq!(c.parsed.quality.quality, Quality::UnknownMusic);
    }

    #[test]
    fn ebook_quality_and_size_per_book() {
        let profiles = default_profiles_for(QualityFamily::Ebook);
        let epub_only = &profiles[1];
        let defs = default_definitions_for(QualityFamily::Ebook);
        let mut ctx = DecisionContext::new(epub_only, chrono::Utc::now());
        ctx.definitions = &defs;
        ctx.runtime_minutes = size_runtime_minutes(QualityFamily::Ebook, None);

        let ok = book("Author - Title [EPUB]", BookFormat::Ebook, 3);
        assert!(
            evaluate(&ok, &ctx).accepted,
            "{:?}",
            evaluate(&ok, &ctx).rejections
        );
        assert_eq!(
            specs(&evaluate(
                &book("Author - Title [PDF]", BookFormat::Ebook, 3),
                &ctx
            )),
            ["quality"]
        );
        assert_eq!(
            specs(&evaluate(
                &book("Author - Title [EPUB]", BookFormat::Ebook, 900),
                &ctx
            )),
            ["size"]
        );
        // A five-book collection may be five times as large.
        ctx.episode_count = 5;
        assert!(evaluate(&book("Author - Books 1-5 [EPUB]", BookFormat::Ebook, 900), &ctx).accepted);
    }

    #[test]
    fn ebook_upgrade_and_cutoff() {
        let any = &default_profiles_for(QualityFamily::Ebook)[0];
        let mut ctx = DecisionContext::new(any, chrono::Utc::now());
        ctx.existing = vec![ExistingFile {
            quality: QualityModel::new(Quality::Mobi),
            format_ids: vec![],
            languages: vec![],
        }];
        assert!(evaluate(&book("A - T [EPUB]", BookFormat::Ebook, 2), &ctx).accepted);
        assert_eq!(
            specs(&evaluate(&book("A - T [PDF]", BookFormat::Ebook, 2), &ctx)),
            ["upgrade"]
        );
        ctx.existing[0].quality = QualityModel::new(Quality::Epub);
        assert_eq!(
            specs(&evaluate(&book("A - T [EPUB]", BookFormat::Ebook, 2), &ctx)),
            ["upgrade"]
        );
    }

    #[test]
    fn audiobook_size_per_minute() {
        let any = &default_profiles_for(QualityFamily::Audiobook)[0];
        let defs = default_definitions_for(QualityFamily::Audiobook);
        let mut ctx = DecisionContext::new(any, chrono::Utc::now());
        ctx.definitions = &defs;
        ctx.runtime_minutes = size_runtime_minutes(QualityFamily::Audiobook, Some(600));
        // 10 h at 64 kbps ≈ 275 MB.
        assert!(evaluate(&book("A - T [M4B]", BookFormat::Audiobook, 275), &ctx).accepted);
        assert_eq!(
            specs(&evaluate(&book("A - T [M4B]", BookFormat::Audiobook, 20), &ctx)),
            ["size"]
        );
        // Unknown duration: no size check.
        ctx.runtime_minutes = size_runtime_minutes(QualityFamily::Audiobook, None);
        assert!(evaluate(&book("A - T [M4B]", BookFormat::Audiobook, 20), &ctx).accepted);
        // An ebook release can never satisfy an audiobook profile.
        let c = ReleaseCandidate::for_book("A - T [EPUB]", Protocol::Torrent, BookFormat::Audiobook);
        assert_eq!(c.parsed.quality.quality, Quality::UnknownAudiobook);
    }

    #[test]
    fn album_quality_size_and_format_score() {
        let profiles = default_profiles_for(QualityFamily::Music);
        let mut lossless = profiles
            .iter()
            .find(|p| p.name == "Music: Lossless")
            .unwrap()
            .clone();
        let defs = default_definitions_for(QualityFamily::Music);
        let formats = vec![CustomFormat {
            id: 7,
            name: "WEB".into(),
            include_in_rename: false,
            specifications: vec![FormatSpecification {
                name: "web".into(),
                condition: Condition::ReleaseTitle {
                    value: crate::format::Pattern::new(r"\bWEB\b"),
                },
                negate: false,
                required: true,
            }],
        }];
        lossless.format_items = vec![FormatItem {
            format_id: 7,
            score: 10,
        }];
        lossless.min_format_score = 5;
        let mut ctx = DecisionContext::new(&lossless, chrono::Utc::now());
        ctx.definitions = &defs;
        ctx.formats = &formats;
        // 45-minute album.
        ctx.runtime_minutes = size_runtime_minutes(QualityFamily::Music, Some(45));
        let album = |title: &str, size_mb: u64| {
            let mut c = ReleaseCandidate::for_album(title, Protocol::Torrent);
            c.size = size_mb * MB;
            c.media_id = Some(9);
            c
        };
        let d = evaluate(&album("Artist - Album (2020) [FLAC] WEB", 280), &ctx);
        assert!(d.accepted, "{:?}", d.rejections);
        assert_eq!(d.format_score, 10);
        assert_eq!(
            specs(&evaluate(&album("Artist - Album (2020) [FLAC]", 280), &ctx)),
            ["custom_format_score"]
        );
        assert_eq!(
            specs(&evaluate(&album("Artist - Album (2020) [MP3] WEB", 280), &ctx)),
            ["quality"]
        );
        // 30 MB for 45 minutes of FLAC is far too small (a mislabelled lossy rip).
        assert_eq!(
            specs(&evaluate(&album("Artist - Album (2020) [FLAC] WEB", 30), &ctx)),
            ["size"]
        );
        // Existing FLAC meets the Lossless cutoff: no upgrade to Hi-Res.
        ctx.existing = vec![ExistingFile {
            quality: QualityModel::new(Quality::Flac),
            format_ids: vec![7],
            languages: vec![],
        }];
        assert_eq!(
            specs(&evaluate(&album("Artist - Album (2020) [FLAC] WEB", 280), &ctx)),
            ["upgrade"]
        );
    }

    #[test]
    fn video_profiles_reject_book_qualities() {
        let video = &crate::default_profiles()[0];
        let ctx = DecisionContext::new(video, chrono::Utc::now());
        let d = evaluate(&book("A - T [EPUB]", BookFormat::Ebook, 2), &ctx);
        assert!(specs(&d).contains(&"quality"));
    }
}
