//! Which subtitle to take: made for this file (a file-hash match) first, then the same film or
//! episode (IMDb or TMDb id, with season and episode), then a release name that matches.

use serde::{Deserialize, Serialize};

use crate::naming::Variant;

/// How well a subtitle matches the file, worst first so that `>` means better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchLevel {
    /// Only the release name matches.
    Release,
    /// The same film or episode, by its IMDb or TMDb id (with season and episode).
    Id,
    /// Made for this exact file: the OpenSubtitles file hash matches.
    Hash,
}

impl MatchLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Id => "id",
            Self::Hash => "hash",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "release" => Some(Self::Release),
            "id" => Some(Self::Id),
            "hash" => Some(Self::Hash),
            _ => None,
        }
    }
}

/// Where a candidate comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// OpenSubtitles; `source_id` is the file id to download.
    OpenSubtitles,
    /// A Stremio subtitle add-on (its position in the settings); `source_id` is the address.
    Stremio(usize),
}

impl Source {
    pub fn name(&self) -> &'static str {
        match self {
            Self::OpenSubtitles => "opensubtitles",
            Self::Stremio(_) => "stremio",
        }
    }

    /// Settings order: OpenSubtitles, then the add-ons as listed.
    fn order(&self) -> usize {
        match self {
            Self::OpenSubtitles => 0,
            Self::Stremio(i) => i + 1,
        }
    }
}

/// A subtitle a source offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub source: Source,
    pub source_id: String,
    /// The language code from the settings it was found for.
    pub language: String,
    pub variant: Variant,
    pub level: MatchLevel,
    pub release: Option<String>,
    pub downloads: u64,
    /// Machine or AI translated; taken only when nothing else is offered.
    pub machine_translated: bool,
}

/// The least release-name likeness for a release-only match.
pub const RELEASE_MIN_SIMILARITY: f64 = 0.5;

fn tokens(name: &str) -> std::collections::BTreeSet<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .filter(|t| !matches!(t.as_str(), "srt" | "sub" | "subs" | "mkv" | "mp4" | "avi"))
        .collect()
}

/// How alike two release names are, from 0 (nothing shared) to 1 (the same words).
pub fn release_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (tokens(a), tokens(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(&b).count() as f64;
    shared / a.union(&b).count() as f64
}

/// The level of a candidate a source found: a hash match is made for this file; one found by
/// id is the same film or episode; one found only by name counts when its release name is
/// close enough to the file's, otherwise it is left out (`None`).
pub fn level_of(
    hash_match: bool,
    found_by_id: bool,
    release: Option<&str>,
    file_release: Option<&str>,
) -> Option<MatchLevel> {
    if hash_match {
        return Some(MatchLevel::Hash);
    }
    if found_by_id {
        return Some(MatchLevel::Id);
    }
    match (release, file_release) {
        (Some(r), Some(f)) if release_similarity(r, f) >= RELEASE_MIN_SIMILARITY => Some(MatchLevel::Release),
        _ => None,
    }
}

/// Orders candidates best first: match level, then release likeness to the file, then human
/// translations, then downloads, then the order of the sources in the settings.
pub fn rank(candidates: &mut [Candidate], file_release: Option<&str>) {
    let likeness = |c: &Candidate| match (&c.release, file_release) {
        (Some(r), Some(f)) => release_similarity(r, f),
        _ => 0.0,
    };
    candidates.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then_with(|| likeness(b).total_cmp(&likeness(a)))
            .then_with(|| a.machine_translated.cmp(&b.machine_translated))
            .then_with(|| b.downloads.cmp(&a.downloads))
            .then_with(|| a.source.order().cmp(&b.source.order()))
    });
}

/// The candidate to fetch for one language and variant, from a ranked list: the best when there
/// is no subtitle yet; when there is one, only a hash match to replace a weaker match, and only
/// when upgrades are on.
pub fn pick<'a>(
    ranked: &'a [Candidate],
    language: &'a str,
    variant: Variant,
    current: Option<MatchLevel>,
    upgrade: bool,
) -> Option<&'a Candidate> {
    choices(ranked, language, variant, current, upgrade).next()
}

/// Every candidate worth trying for one language and variant, best first (the next is tried
/// when a download fails or a subtitle does not pass the checks).
pub fn choices<'a>(
    ranked: &'a [Candidate],
    language: &'a str,
    variant: Variant,
    current: Option<MatchLevel>,
    upgrade: bool,
) -> impl Iterator<Item = &'a Candidate> + 'a {
    ranked.iter().filter(move |c| {
        c.language == language
            && c.variant == variant
            && match current {
                None => true,
                Some(level) => upgrade && c.level == MatchLevel::Hash && level < MatchLevel::Hash,
            }
    })
}

/// Whether a subtitle at `current` still wants a search: missing, or (with upgrades on) not
/// yet made for the file.
pub fn wanted(current: Option<MatchLevel>, upgrade: bool) -> bool {
    match current {
        None => true,
        Some(level) => upgrade && level < MatchLevel::Hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(level: MatchLevel, release: &str, downloads: u64) -> Candidate {
        Candidate {
            source: Source::OpenSubtitles,
            source_id: release.into(),
            language: "en".into(),
            variant: Variant::Normal,
            level,
            release: Some(release.into()),
            downloads,
            machine_translated: false,
        }
    }

    const FILE: &str = "Heat.1995.1080p.BluRay.x264-GRP";

    #[test]
    fn hash_beats_id_beats_release() {
        let mut c = vec![
            cand(MatchLevel::Release, FILE, 10_000),
            cand(MatchLevel::Id, "Heat.1995.720p.WEB-OTHER", 5_000),
            cand(MatchLevel::Hash, "something else entirely", 1),
        ];
        rank(&mut c, Some(FILE));
        let levels: Vec<_> = c.iter().map(|c| c.level).collect();
        assert_eq!(levels, [MatchLevel::Hash, MatchLevel::Id, MatchLevel::Release]);
    }

    #[test]
    fn same_level_prefers_the_closer_release_then_downloads() {
        let mut c = vec![
            cand(MatchLevel::Id, "Heat.1995.720p.WEB-OTHER", 9_000),
            cand(MatchLevel::Id, "Heat.1995.1080p.BluRay.x264-GRP", 10),
            cand(MatchLevel::Id, "Heat.1995.720p.WEB-OTHER2", 20_000),
        ];
        rank(&mut c, Some(FILE));
        assert_eq!(c[0].release.as_deref(), Some(FILE));
        // the other two are as alike as each other: more downloads first
        assert_eq!(c[1].downloads, 20_000);
    }

    #[test]
    fn human_translations_come_before_machine_ones() {
        let mut machine = cand(MatchLevel::Id, "x", 50_000);
        machine.machine_translated = true;
        let mut c = vec![machine, cand(MatchLevel::Id, "x", 1)];
        rank(&mut c, None);
        assert!(!c[0].machine_translated);
    }

    #[test]
    fn levels_from_what_the_source_says() {
        assert_eq!(level_of(true, true, None, None), Some(MatchLevel::Hash));
        assert_eq!(level_of(false, true, None, None), Some(MatchLevel::Id));
        assert_eq!(
            level_of(false, false, Some("Heat.1995.1080p.BluRay.x264-GRP"), Some(FILE)),
            Some(MatchLevel::Release)
        );
        assert_eq!(
            level_of(false, false, Some("Ronin.1998.DVDRip"), Some(FILE)),
            None
        );
        assert_eq!(level_of(false, false, Some(FILE), None), None);
    }

    #[test]
    fn similarity_is_word_overlap() {
        assert_eq!(
            release_similarity(FILE, "heat 1995 1080p bluray x264 grp.srt"),
            1.0
        );
        assert_eq!(release_similarity(FILE, ""), 0.0);
        assert!(release_similarity(FILE, "Heat.1995.720p.WEB-OTHER") < RELEASE_MIN_SIMILARITY);
    }

    #[test]
    fn pick_fills_missing_and_upgrades_only_to_a_hash_match() {
        let ranked = vec![cand(MatchLevel::Id, FILE, 1)];
        assert!(pick(&ranked, "en", Variant::Normal, None, true).is_some());
        assert!(pick(&ranked, "en", Variant::Sdh, None, true).is_none());
        assert!(pick(&ranked, "fr", Variant::Normal, None, true).is_none());
        // an id match does not replace a release match: only a hash match is an upgrade
        assert!(pick(&ranked, "en", Variant::Normal, Some(MatchLevel::Release), true).is_none());
        let hashed = vec![cand(MatchLevel::Hash, FILE, 1)];
        assert!(pick(&hashed, "en", Variant::Normal, Some(MatchLevel::Id), true).is_some());
        assert!(pick(&hashed, "en", Variant::Normal, Some(MatchLevel::Id), false).is_none());
        assert!(pick(&hashed, "en", Variant::Normal, Some(MatchLevel::Hash), true).is_none());
    }
}
