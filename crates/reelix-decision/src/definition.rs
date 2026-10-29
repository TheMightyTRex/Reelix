//! Quality definitions: display title and allowed size for each quality.
//!
//! # Size basis per family
//!
//! The engine's size check ([`SizeWithinLimits`](crate::engine)) always computes
//! `limit × runtime_minutes × episode_count` megabytes, so what a definition's numbers mean
//! depends on the `runtime_minutes` the caller supplies (see [`SizeBasis`] and
//! [`crate::size_runtime_minutes`]):
//!
//! - **Video, audiobook, music**: MB per minute of playing time. A movie/episode passes its
//!   runtime; an audiobook passes the edition's duration; an album passes the sum of its track
//!   durations (so one rule covers single tracks and whole albums). Unknown duration → the
//!   size check is skipped.
//! - **Ebook**: MB **per book**. Text has no runtime, so the caller passes
//!   `runtime_minutes = Some(1)` and the number of books in the release as `episode_count`
//!   (1 for a single book).
//!
//! The indexer-wide `maximum_size_mb` applies to every family regardless.

use reelix_parser::{Quality, QualityFamily};
use serde::{Deserialize, Serialize};

/// Size limits for one quality: megabytes per minute of runtime, or per book for ebooks
/// (see the [module docs](self)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityDefinition {
    pub quality: Quality,
    /// Display title (user-editable), defaults to the quality name.
    pub title: String,
    pub min_size: f64,
    /// Size the prioritiser prefers; `None` = no preference.
    #[serde(default)]
    pub preferred_size: Option<f64>,
    /// `None` = unlimited.
    #[serde(default)]
    pub max_size: Option<f64>,
}

impl QualityDefinition {
    pub fn new(quality: Quality, min: f64, preferred: Option<f64>, max: Option<f64>) -> Self {
        Self {
            quality,
            title: quality.name().to_string(),
            min_size: min,
            preferred_size: preferred,
            max_size: max,
        }
    }

    /// Allowed byte range for a runtime (in minutes); `None` max = unlimited.
    pub fn byte_limits(&self, runtime_minutes: u32) -> (u64, Option<u64>) {
        let mb = |v: f64| (v.max(0.0) * runtime_minutes as f64 * 1024.0 * 1024.0) as u64;
        (mb(self.min_size), self.max_size.map(mb))
    }

    /// Checks invariants (non-negative, min ≤ preferred ≤ max).
    pub fn validate(&self) -> Result<(), String> {
        let max = self.max_size.unwrap_or(f64::INFINITY);
        let pref = self.preferred_size.unwrap_or(self.min_size);
        if !(self.min_size >= 0.0 && self.min_size <= pref && pref <= max) {
            return Err(format!(
                "{}: sizes must satisfy 0 ≤ min ≤ preferred ≤ max",
                self.quality.name()
            ));
        }
        Ok(())
    }
}

/// The default definitions table, one row per [`Quality::ALL`] entry.
pub fn default_definitions() -> Vec<QualityDefinition> {
    use Quality as Q;
    let row = |q: Quality| -> (f64, Option<f64>, Option<f64>) {
        match q {
            Q::Unknown | Q::Workprint | Q::Cam | Q::Telesync | Q::Telecine | Q::Regional | Q::Dvdscr => {
                (0.0, None, None)
            }
            Q::Sdtv | Q::Dvd | Q::DvdR | Q::WebDl480p | Q::WebRip480p | Q::Bluray480p => {
                (2.0, Some(30.0), Some(100.0))
            }
            Q::Bluray576p => (3.0, Some(35.0), Some(100.0)),
            Q::Hdtv720p => (3.0, Some(50.0), Some(125.0)),
            Q::WebDl720p | Q::WebRip720p => (3.0, Some(50.0), Some(130.0)),
            Q::Bluray720p => (4.0, Some(60.0), Some(160.0)),
            Q::Hdtv1080p => (4.0, Some(70.0), Some(170.0)),
            Q::WebDl1080p | Q::WebRip1080p => (4.0, Some(70.0), Some(180.0)),
            Q::Bluray1080p => (5.0, Some(90.0), Some(250.0)),
            Q::Remux1080p => (15.0, Some(250.0), Some(400.0)),
            Q::Hdtv2160p => (15.0, Some(150.0), Some(400.0)),
            Q::WebDl2160p | Q::WebRip2160p => (15.0, Some(170.0), Some(400.0)),
            Q::Bluray2160p => (20.0, Some(250.0), Some(500.0)),
            Q::Remux2160p => (35.0, Some(600.0), None),
            Q::BrDisk => (20.0, None, None),
            Q::RawHd => (4.0, None, None),
            // Non-video families: see `default_definitions_for`.
            _ => (0.0, None, None),
        }
    };
    Quality::ALL
        .iter()
        .map(|q| {
            let (min, pref, max) = row(*q);
            QualityDefinition::new(*q, min, pref, max)
        })
        .collect()
}

/// How a family's definition sizes are measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeBasis {
    /// MB per minute of playing time (video, audiobook, music).
    PerMinute,
    /// MB per book (ebook).
    PerBook,
}

impl SizeBasis {
    pub fn of(family: QualityFamily) -> Self {
        match family {
            QualityFamily::Ebook => Self::PerBook,
            QualityFamily::Video | QualityFamily::Audiobook | QualityFamily::Music => Self::PerMinute,
        }
    }

    /// Unit label, e.g. for table headers: `MB/min` or `MB/book`.
    pub fn unit(self) -> &'static str {
        match self {
            Self::PerMinute => "MB/min",
            Self::PerBook => "MB/book",
        }
    }
}

/// Default definitions of one family (Video = [`default_definitions`]), in the family's
/// ranking order. Sizes follow [`SizeBasis::of`]`(family)`:
///
/// - Ebook (MB per book): formats are tiny, so only generous maxima guard against
///   mislabelled comics/scans and packs.
/// - Audiobook (MB per minute): speech is encoded at 24–128 kbps (≈ 0.2–0.9 MB/min);
///   MP3/M4B allow up to 320 kbps, FLAC 2–15.
/// - Music (MB per minute): each lossy bitrate maps to a narrow band around its nominal
///   size (128 kbps ≈ 0.94 MB/min, 320 kbps ≈ 2.34 MB/min) with headroom for artwork;
///   16-bit lossless 1.5–15 and 24-bit lossless unlimited (24/192 runs ≈ 60 MB/min).
///
/// Unknown qualities never have limits.
pub fn default_definitions_for(family: QualityFamily) -> Vec<QualityDefinition> {
    use Quality as Q;
    let row = |q: Quality| -> (f64, Option<f64>, Option<f64>) {
        match q {
            // Ebook: MB per book.
            Q::Pdf => (0.0, Some(10.0), Some(500.0)),
            Q::Mobi | Q::Azw3 | Q::Epub => (0.0, Some(5.0), Some(200.0)),
            // Audiobook: MB per minute.
            Q::AudiobookMp3 | Q::AudiobookM4b => (0.1, Some(0.5), Some(3.0)),
            Q::AudiobookFlac => (2.0, Some(5.0), Some(15.0)),
            // Music: MB per minute.
            Q::Wma => (0.4, Some(1.0), Some(2.5)),
            Q::Mp3_128 => (0.8, Some(1.0), Some(1.2)),
            Q::Mp3_192 => (1.2, Some(1.4), Some(1.7)),
            Q::AacVbr | Q::Vorbis | Q::Mp3VbrV2 => (0.5, Some(1.4), Some(3.0)),
            Q::Opus => (0.4, Some(1.0), Some(3.0)),
            Q::Mp3_256 => (1.6, Some(1.9), Some(2.4)),
            Q::Aac256 | Q::Mp3VbrV0 => (1.2, Some(1.9), Some(3.0)),
            Q::Mp3_320 | Q::Aac320 => (2.0, Some(2.4), Some(3.0)),
            Q::Wav => (5.0, Some(10.0), Some(40.0)),
            Q::Alac | Q::Flac => (1.5, Some(6.0), Some(15.0)),
            Q::Alac24 | Q::Flac24 => (4.0, Some(15.0), None),
            // Unknown qualities (and video, handled by `default_definitions`).
            _ => (0.0, None, None),
        }
    };
    match family {
        QualityFamily::Video => default_definitions(),
        QualityFamily::Ebook | QualityFamily::Audiobook | QualityFamily::Music => family
            .qualities()
            .iter()
            .map(|q| {
                let (min, pref, max) = row(*q);
                QualityDefinition::new(*q, min, pref, max)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_quality_and_validate() {
        let defs = default_definitions();
        assert_eq!(defs.len(), Quality::ALL.len());
        for d in &defs {
            d.validate().unwrap();
        }
        let bluray = defs.iter().find(|d| d.quality == Quality::Bluray1080p).unwrap();
        let (min, max) = bluray.byte_limits(100);
        assert_eq!(min, 500 * 1024 * 1024);
        assert_eq!(max, Some(25_000 * 1024 * 1024));
    }

    #[test]
    fn family_defaults_cover_their_family() {
        for f in QualityFamily::ALL {
            let defs = default_definitions_for(f);
            assert_eq!(
                defs.iter().map(|d| d.quality).collect::<Vec<_>>(),
                f.qualities(),
                "{f:?}"
            );
            for d in &defs {
                d.validate().unwrap();
                if d.quality.name().starts_with("Unknown") {
                    assert_eq!((d.min_size, d.max_size), (0.0, None));
                }
            }
        }
        assert_eq!(
            default_definitions_for(QualityFamily::Video),
            default_definitions()
        );
        assert_eq!(SizeBasis::of(QualityFamily::Ebook).unit(), "MB/book");
        assert_eq!(SizeBasis::of(QualityFamily::Music), SizeBasis::PerMinute);
    }

    #[test]
    fn music_bands_fit_nominal_bitrates() {
        let defs = default_definitions_for(QualityFamily::Music);
        let mb_per_min = |kbps: f64| kbps * 1000.0 * 60.0 / 8.0 / (1024.0 * 1024.0);
        let fits = |q: Quality, v: f64| {
            let d = defs.iter().find(|d| d.quality == q).unwrap();
            d.min_size <= v && d.max_size.is_none_or(|m| v <= m)
        };
        assert!(fits(Quality::Mp3_128, mb_per_min(128.0)));
        assert!(fits(Quality::Mp3_192, mb_per_min(192.0)));
        assert!(fits(Quality::Mp3_256, mb_per_min(256.0)));
        assert!(fits(Quality::Mp3_320, mb_per_min(320.0)));
        assert!(!fits(Quality::Mp3_320, mb_per_min(128.0)));
        assert!(fits(Quality::Flac, mb_per_min(900.0)));
        assert!(fits(Quality::Flac24, mb_per_min(4608.0)));
    }
}
