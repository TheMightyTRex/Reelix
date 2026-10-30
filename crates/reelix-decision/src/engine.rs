//! The decision engine: runs [`Specification`]s against a [`ReleaseCandidate`] in a
//! [`DecisionContext`], producing a [`Decision`] with structured rejection reasons, and
//! orders accepted releases with [`prioritize`].

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use reelix_core::Protocol;
use reelix_parser::{Language, ParsedRelease, QualityModel};
use serde::{Deserialize, Serialize};

use crate::definition::QualityDefinition;
use crate::delay::DelayProfile;
pub use crate::format::IndexerFlag;
use crate::format::{CustomFormat, FormatInput, matching_formats};
use crate::profile::QualityProfile;
use crate::release_profile::ReleaseProfile;

/// A release found by an indexer (or a manual paste), ready for evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    pub parsed: ParsedRelease,
    pub title: String,
    /// Bytes; 0 = unknown.
    pub size: u64,
    pub protocol: Protocol,
    pub indexer_id: Option<i64>,
    pub indexer: String,
    /// Lower is preferred (1–50, default 25).
    pub indexer_priority: i32,
    pub seeders: Option<u32>,
    pub peers: Option<u32>,
    pub publish_date: Option<DateTime<Utc>>,
    pub indexer_flags: Vec<IndexerFlag>,
    pub download_url: String,
    pub guid: String,
    /// Opaque ids of the matched movie/series and episodes (filled by the caller).
    pub media_id: Option<i64>,
    pub episode_ids: Vec<i64>,
}

impl ReleaseCandidate {
    /// Parses `title` and fills defaults for everything else.
    pub fn new(title: &str, protocol: Protocol) -> Self {
        Self {
            parsed: reelix_parser::parse(title),
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

    /// Minutes since publication (0 when unknown or in the future).
    pub fn age_minutes(&self, now: DateTime<Utc>) -> i64 {
        self.publish_date.map_or(0, |d| (now - d).num_minutes().max(0))
    }

    pub fn format_input(&self, original_language: Option<Language>) -> FormatInput<'_> {
        FormatInput {
            parsed: &self.parsed,
            title: &self.title,
            size: (self.size > 0).then_some(self.size),
            indexer_flags: &self.indexer_flags,
            original_language,
        }
    }
}

/// A file already in the library for the same media.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExistingFile {
    pub quality: QualityModel,
    /// Ids of the custom formats the file matched.
    #[serde(default)]
    pub format_ids: Vec<i64>,
    #[serde(default)]
    pub languages: Vec<Language>,
}

/// What triggered the evaluation; delays only apply to automatic grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Rss,
    #[default]
    Automatic,
    Interactive,
}

/// Predicate over a candidate (blocklist / queue lookups supplied by the caller).
pub type Predicate<'a> = &'a dyn Fn(&ReleaseCandidate) -> bool;

/// Everything the specifications need besides the release itself.
pub struct DecisionContext<'a> {
    pub profile: &'a QualityProfile,
    pub formats: &'a [CustomFormat],
    pub definitions: &'a [QualityDefinition],
    pub existing: Vec<ExistingFile>,
    /// Runtime of the movie / episode; size checks are skipped when unknown.
    pub runtime_minutes: Option<u32>,
    /// Episodes covered by the release (season packs), multiplies size limits.
    pub episode_count: u32,
    pub release_profiles: &'a [ReleaseProfile],
    pub delay_profile: Option<&'a DelayProfile>,
    pub media_tags: Vec<i64>,
    pub original_language: Option<Language>,
    pub is_blocklisted: Option<Predicate<'a>>,
    pub is_queued: Option<Predicate<'a>>,
    /// `Some(reason)` when the media is not yet available (minimum availability / air date).
    pub unavailable_reason: Option<String>,
    pub minimum_seeders: u32,
    /// Usenet retention in days; `None` = unlimited.
    pub usenet_retention_days: Option<u32>,
    /// Usenet minimum age (minutes) before grabbing.
    pub minimum_age_minutes: u32,
    pub maximum_size_mb: Option<u64>,
    /// Indexers allowed by tag constraints; `None` = all.
    pub allowed_indexers: Option<Vec<i64>>,
    pub search_kind: SearchKind,
    pub now: DateTime<Utc>,
}

impl<'a> DecisionContext<'a> {
    /// A context with no files, formats, profiles or constraints beyond `profile`.
    pub fn new(profile: &'a QualityProfile, now: DateTime<Utc>) -> Self {
        Self {
            profile,
            formats: &[],
            definitions: &[],
            existing: Vec::new(),
            runtime_minutes: None,
            episode_count: 1,
            release_profiles: &[],
            delay_profile: None,
            media_tags: Vec::new(),
            original_language: None,
            is_blocklisted: None,
            is_queued: None,
            unavailable_reason: None,
            minimum_seeders: 0,
            usenet_retention_days: None,
            minimum_age_minutes: 0,
            maximum_size_mb: None,
            allowed_indexers: None,
            search_kind: SearchKind::Automatic,
            now,
        }
    }

    fn existing_score(&self, f: &ExistingFile) -> i32 {
        f.format_ids.iter().map(|id| self.profile.format_score(*id)).sum()
    }
}

/// Permanent rejections never change for this release; temporary ones may pass later
/// (delay, availability, queue).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionKind {
    Permanent,
    Temporary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub reason: String,
    pub kind: RejectionKind,
    /// Name of the specification that rejected.
    pub spec: String,
}

impl Rejection {
    fn permanent(spec: &str, reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            kind: RejectionKind::Permanent,
            spec: spec.to_string(),
        }
    }

    fn temporary(spec: &str, reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            kind: RejectionKind::Temporary,
            spec: spec.to_string(),
        }
    }
}

/// A matched custom format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatRef {
    pub id: i64,
    pub name: String,
    /// The score the profile gives it.
    pub score: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub accepted: bool,
    pub rejections: Vec<Rejection>,
    pub format_score: i32,
    pub matched_formats: Vec<FormatRef>,
    /// Row index in the profile (higher is better); `None` if the profile doesn't list it.
    pub quality_rank: Option<usize>,
}

impl Decision {
    /// True when only temporary rejections exist (worth retrying later).
    pub fn is_temporarily_rejected(&self) -> bool {
        !self.accepted && self.rejections.iter().all(|r| r.kind == RejectionKind::Temporary)
    }
}

/// Precomputed facts shared by all specifications.
#[derive(Debug, Clone)]
pub struct Facts {
    pub format_score: i32,
    pub matched_formats: Vec<FormatRef>,
    pub quality_rank: Option<usize>,
}

/// One rule of the decision engine.
pub trait Specification: Send + Sync {
    fn name(&self) -> &'static str;
    /// Rejections for `c` (empty = passes).
    fn check(&self, c: &ReleaseCandidate, ctx: &DecisionContext<'_>, facts: &Facts) -> Vec<Rejection>;
}

/// A quality + format score, for upgrade comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scored {
    pub quality: QualityModel,
    pub format_score: i32,
}

/// Whether `candidate` would upgrade `existing` under `profile`:
/// - same quality with a higher revision (PROPER/REPACK/REAL) is always an upgrade;
/// - otherwise upgrades must be allowed and `existing` must not have met the cutoff;
/// - a better-ranked quality upgrades; an equal rank upgrades when the format score improves
///   by at least `min_upgrade_format_score`.
pub fn is_upgrade(existing: &Scored, candidate: &Scored, profile: &QualityProfile) -> bool {
    upgrade_rejection(existing, candidate, profile).is_none()
}

fn revision_key(q: &QualityModel) -> (u8, u8) {
    (q.revision.version, q.revision.real)
}

fn upgrade_rejection(existing: &Scored, candidate: &Scored, profile: &QualityProfile) -> Option<String> {
    let er = profile.rank(existing.quality.quality);
    let cr = profile.rank(candidate.quality.quality);
    let same_quality = existing.quality.quality == candidate.quality.quality;
    if same_quality && revision_key(&candidate.quality) > revision_key(&existing.quality) {
        return None;
    }
    if !profile.upgrade_allowed {
        return Some("Existing file exists and upgrades are disabled in the profile".into());
    }
    if profile.cutoff_met(existing.quality.quality, existing.format_score) {
        return Some(format!(
            "Existing file ({}, score {}) already meets the profile cutoff",
            existing.quality.quality.name(),
            existing.format_score
        ));
    }
    match (cr, er) {
        (Some(c), Some(e)) if c > e => None,
        (Some(_), None) => None,
        (Some(c), Some(e)) if c == e => {
            if revision_key(&candidate.quality) < revision_key(&existing.quality) {
                return Some("Existing file has a newer revision".into());
            }
            let gain = candidate.format_score - existing.format_score;
            if gain > 0 && gain >= profile.min_upgrade_format_score {
                None
            } else {
                Some(format!(
                    "Not an upgrade: same quality ({}) and custom format score {} vs existing {}",
                    candidate.quality.quality.name(),
                    candidate.format_score,
                    existing.format_score
                ))
            }
        }
        _ => Some(format!(
            "Not an upgrade: {} is not better than existing {}",
            candidate.quality.quality.name(),
            existing.quality.quality.name()
        )),
    }
}

// ---------------- specifications ----------------

macro_rules! spec {
    ($ty:ident, $name:literal, |$c:ident, $ctx:ident, $facts:ident| $body:block) => {
        #[doc = concat!("Specification `", $name, "`.")]
        pub struct $ty;
        impl Specification for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            #[allow(unused_variables)]
            fn check(
                &self,
                $c: &ReleaseCandidate,
                $ctx: &DecisionContext<'_>,
                $facts: &Facts,
            ) -> Vec<Rejection> {
                $body
            }
        }
    };
}

fn one(r: Option<Rejection>) -> Vec<Rejection> {
    r.into_iter().collect()
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

spec!(NotBlocklisted, "blocklist", |c, ctx, facts| {
    one(ctx
        .is_blocklisted
        .is_some_and(|f| f(c))
        .then(|| Rejection::permanent("blocklist", "Release is blocklisted")))
});

spec!(QualityAllowed, "quality", |c, ctx, facts| {
    let q = c.parsed.quality.quality;
    one((!ctx.profile.is_allowed(q)).then(|| {
        Rejection::permanent(
            "quality",
            format!(
                "Quality {} is not wanted in profile {}",
                q.name(),
                ctx.profile.name
            ),
        )
    }))
});

spec!(SizeWithinLimits, "size", |c, ctx, facts| {
    let mut out = Vec::new();
    if c.size == 0 {
        return out;
    }
    if let Some(max) = ctx.maximum_size_mb
        && c.size > max * 1024 * 1024
    {
        out.push(Rejection::permanent(
            "size",
            format!("{} is larger than the maximum size {max} MB", mb(c.size)),
        ));
    }
    // The profile's limits, per episode for multi-episode releases.
    let episodes = u64::from(ctx.episode_count.max(1));
    if let Some(min) = ctx.profile.min_size_mb
        && c.size < min.saturating_mul(1024 * 1024).saturating_mul(episodes)
    {
        out.push(Rejection::permanent(
            "size",
            format!(
                "{} is smaller than the profile's minimum of {}",
                mb(c.size),
                mb(min.saturating_mul(1024 * 1024).saturating_mul(episodes))
            ),
        ));
    }
    if let Some(max) = ctx.profile.max_size_mb
        && c.size > max.saturating_mul(1024 * 1024).saturating_mul(episodes)
    {
        out.push(Rejection::permanent(
            "size",
            format!(
                "{} is larger than the profile's maximum of {}",
                mb(c.size),
                mb(max.saturating_mul(1024 * 1024).saturating_mul(episodes))
            ),
        ));
    }
    let Some(runtime) = ctx.runtime_minutes.filter(|r| *r > 0) else {
        return out;
    };
    let Some(def) = ctx
        .definitions
        .iter()
        .find(|d| d.quality == c.parsed.quality.quality)
    else {
        return out;
    };
    let (min, max) = def.byte_limits(runtime.saturating_mul(ctx.episode_count.max(1)));
    if c.size < min {
        out.push(Rejection::permanent(
            "size",
            format!("{} is smaller than the minimum {}", mb(c.size), mb(min)),
        ));
    }
    if let Some(max) = max
        && c.size > max
    {
        out.push(Rejection::permanent(
            "size",
            format!("{} is larger than the maximum {}", mb(c.size), mb(max)),
        ));
    }
    out
});

spec!(MinimumFormatScore, "custom_format_score", |c, ctx, facts| {
    one((facts.format_score < ctx.profile.min_format_score).then(|| {
        Rejection::permanent(
            "custom_format_score",
            format!(
                "Custom format score {} is below the profile minimum {}",
                facts.format_score, ctx.profile.min_format_score
            ),
        )
    }))
});

spec!(UpgradeOverExisting, "upgrade", |c, ctx, facts| {
    let candidate = Scored {
        quality: c.parsed.quality,
        format_score: facts.format_score,
    };
    ctx.existing
        .iter()
        .filter_map(|f| {
            let existing = Scored {
                quality: f.quality,
                format_score: ctx.existing_score(f),
            };
            upgrade_rejection(&existing, &candidate, ctx.profile).map(|r| Rejection::permanent("upgrade", r))
        })
        .take(1)
        .collect()
});

spec!(ReleaseRestrictions, "release_profile", |c, ctx, facts| {
    ctx.release_profiles
        .iter()
        .filter(|p| p.applies(c.indexer_id, &ctx.media_tags))
        .flat_map(|p| p.violations(&c.title))
        .map(|r| Rejection::permanent("release_profile", r))
        .collect()
});

spec!(NotInQueue, "queue", |c, ctx, facts| {
    one(ctx.is_queued.is_some_and(|f| f(c)).then(|| {
        Rejection::temporary(
            "queue",
            "Release (or one at least as good) is already in the download queue",
        )
    }))
});

spec!(Availability, "availability", |c, ctx, facts| {
    one(ctx
        .unavailable_reason
        .as_ref()
        .map(|r| Rejection::temporary("availability", r.clone())))
});

spec!(TorrentSeeders, "seeders", |c, ctx, facts| {
    let seeders = c.seeders.unwrap_or(0);
    one(
        (c.protocol == Protocol::Torrent && ctx.minimum_seeders > 0 && seeders < ctx.minimum_seeders).then(
            || {
                Rejection::permanent(
                    "seeders",
                    format!("Not enough seeders: {seeders} (minimum {})", ctx.minimum_seeders),
                )
            },
        ),
    )
});

spec!(UsenetRetention, "retention", |c, ctx, facts| {
    let days = c.age_minutes(ctx.now) / (60 * 24);
    one(match ctx.usenet_retention_days {
        Some(max) if c.protocol == Protocol::Usenet && max > 0 && days > max as i64 => {
            Some(Rejection::permanent(
                "retention",
                format!("Older than usenet retention: {days} days (retention {max})"),
            ))
        }
        _ => None,
    })
});

spec!(MinimumAge, "minimum_age", |c, ctx, facts| {
    let age = c.age_minutes(ctx.now);
    one(
        (c.protocol == Protocol::Usenet && c.publish_date.is_some() && age < ctx.minimum_age_minutes as i64)
            .then(|| {
                Rejection::temporary(
                    "minimum_age",
                    format!(
                        "Only {age} minutes old (minimum age {} minutes)",
                        ctx.minimum_age_minutes
                    ),
                )
            }),
    )
});

spec!(ProtocolAllowed, "protocol", |c, ctx, facts| {
    one(ctx.delay_profile.filter(|d| !d.allows(c.protocol)).map(|_| {
        Rejection::permanent(
            "protocol",
            format!("{:?} is disabled in the delay profile", c.protocol),
        )
    }))
});

spec!(DelayProfileSpec, "delay", |c, ctx, facts| {
    let Some(d) = ctx.delay_profile else {
        return Vec::new();
    };
    if ctx.search_kind == SearchKind::Interactive {
        return Vec::new();
    }
    let delay = d.delay_for(c.protocol) as i64;
    if delay == 0 || c.publish_date.is_none() {
        return Vec::new();
    }
    if d.bypass_if_highest_quality
        && facts.quality_rank.is_some()
        && facts.quality_rank == ctx.profile.best_allowed_rank()
    {
        return Vec::new();
    }
    if d.bypass_if_above_custom_format_score && facts.format_score >= d.minimum_custom_format_score {
        return Vec::new();
    }
    // Revision-only upgrades (PROPER/REPACK of the file we have) are not delayed.
    let q = c.parsed.quality;
    if ctx
        .existing
        .iter()
        .any(|f| f.quality.quality == q.quality && revision_key(&q) > revision_key(&f.quality))
    {
        return Vec::new();
    }
    let age = c.age_minutes(ctx.now);
    one((age < delay).then(|| {
        Rejection::temporary(
            "delay",
            format!("Waiting for a better release: delay is {delay} minutes, release is {age} minutes old"),
        )
    }))
});

spec!(LanguageWanted, "language", |c, ctx, facts| {
    let wanted = match ctx.profile.language {
        None => return Vec::new(),
        Some(Language::Original) => match ctx.original_language {
            Some(l) => l,
            None => return Vec::new(),
        },
        Some(l) => l,
    };
    one((!c.parsed.languages.contains(&wanted)).then(|| {
        let got: Vec<&str> = c.parsed.languages.iter().map(|l| l.name()).collect();
        Rejection::permanent(
            "language",
            format!(
                "Language {} is not wanted (profile wants {})",
                got.join(", "),
                wanted.name()
            ),
        )
    }))
});

spec!(IndexerAllowed, "indexer", |c, ctx, facts| {
    one(match (&ctx.allowed_indexers, c.indexer_id) {
        (Some(allowed), Some(id)) if !allowed.contains(&id) => Some(Rejection::permanent(
            "indexer",
            format!(
                "Indexer {} is not allowed for this media (tag restrictions)",
                c.indexer
            ),
        )),
        _ => None,
    })
});

spec!(Parseable, "parse", |c, ctx, facts| {
    one(
        (c.parsed.title.is_empty() && c.media_id.is_none() && c.parsed.episode().is_none())
            .then(|| Rejection::permanent("parse", "Unable to parse the release title")),
    )
});

/// All built-in specifications, in evaluation order.
pub fn default_specifications() -> Vec<Box<dyn Specification>> {
    vec![
        Box::new(Parseable),
        Box::new(NotBlocklisted),
        Box::new(QualityAllowed),
        Box::new(LanguageWanted),
        Box::new(SizeWithinLimits),
        Box::new(MinimumFormatScore),
        Box::new(UpgradeOverExisting),
        Box::new(ReleaseRestrictions),
        Box::new(NotInQueue),
        Box::new(Availability),
        Box::new(TorrentSeeders),
        Box::new(UsenetRetention),
        Box::new(MinimumAge),
        Box::new(ProtocolAllowed),
        Box::new(DelayProfileSpec),
        Box::new(IndexerAllowed),
    ]
}

/// Computes matched formats, score and rank for `c`.
pub fn facts(c: &ReleaseCandidate, ctx: &DecisionContext<'_>) -> Facts {
    let input = c.format_input(ctx.original_language);
    let matched: Vec<FormatRef> = matching_formats(ctx.formats, &input)
        .into_iter()
        .map(|f| FormatRef {
            id: f.id,
            name: f.name.clone(),
            score: ctx.profile.format_score(f.id),
        })
        .collect();
    Facts {
        format_score: matched.iter().map(|f| f.score).sum(),
        matched_formats: matched,
        quality_rank: ctx.profile.rank(c.parsed.quality.quality),
    }
}

/// Runs `specs` against `c`.
pub fn evaluate_with(
    specs: &[Box<dyn Specification>],
    c: &ReleaseCandidate,
    ctx: &DecisionContext<'_>,
) -> Decision {
    let facts = facts(c, ctx);
    let rejections: Vec<Rejection> = specs.iter().flat_map(|s| s.check(c, ctx, &facts)).collect();
    Decision {
        accepted: rejections.is_empty(),
        rejections,
        format_score: facts.format_score,
        matched_formats: facts.matched_formats,
        quality_rank: facts.quality_rank,
    }
}

/// Runs the [`default_specifications`] against `c`.
pub fn evaluate(c: &ReleaseCandidate, ctx: &DecisionContext<'_>) -> Decision {
    evaluate_with(&default_specifications(), c, ctx)
}

/// A candidate with its decision, as sorted by [`prioritize`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedRelease {
    pub candidate: ReleaseCandidate,
    pub decision: Decision,
}

/// Sorts best-first: accepted ▸ quality rank (+ revision) ▸ custom-format score ▸ preferred
/// protocol ▸ indexer priority ▸ seeders (torrent) / newest (usenet) ▸ smaller size.
pub fn prioritize(items: &mut [RankedRelease], preferred_protocol: Option<Protocol>) {
    items.sort_by(|a, b| compare(a, b, preferred_protocol));
}

fn compare(a: &RankedRelease, b: &RankedRelease, preferred: Option<Protocol>) -> Ordering {
    let (ca, cb) = (&a.candidate, &b.candidate);
    let (da, db) = (&a.decision, &b.decision);
    let rank = |d: &Decision| d.quality_rank.map_or(-1, |r| r as i64);
    let pref = |c: &ReleaseCandidate| preferred.is_some_and(|p| p == c.protocol);
    let size_bucket = |c: &ReleaseCandidate| c.size / (200 * 1024 * 1024);
    db.accepted
        .cmp(&da.accepted)
        .then_with(|| rank(db).cmp(&rank(da)))
        .then_with(|| revision_key(&cb.parsed.quality).cmp(&revision_key(&ca.parsed.quality)))
        .then_with(|| db.format_score.cmp(&da.format_score))
        .then_with(|| pref(cb).cmp(&pref(ca)))
        .then_with(|| ca.indexer_priority.cmp(&cb.indexer_priority))
        .then_with(|| match (ca.protocol, cb.protocol) {
            (Protocol::Torrent, Protocol::Torrent) => cb.seeders.unwrap_or(0).cmp(&ca.seeders.unwrap_or(0)),
            (Protocol::Usenet, Protocol::Usenet) => cb.publish_date.cmp(&ca.publish_date),
            _ => Ordering::Equal,
        })
        .then_with(|| size_bucket(ca).cmp(&size_bucket(cb)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::default_definitions;
    use crate::format::{Condition, FormatSpecification, Pattern};
    use crate::profile::{FormatItem, default_profiles};
    use chrono::Duration;
    use reelix_parser::Quality;

    fn hd1080() -> QualityProfile {
        default_profiles().remove(3)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn cand(title: &str) -> ReleaseCandidate {
        ReleaseCandidate::new(title, Protocol::Torrent)
    }

    fn specs(d: &Decision) -> Vec<&str> {
        d.rejections.iter().map(|r| r.spec.as_str()).collect()
    }

    #[test]
    fn accepts_allowed_quality_and_rejects_others() {
        let p = hd1080();
        let ctx = DecisionContext::new(&p, now());
        assert!(evaluate(&cand("Movie.2020.1080p.BluRay.x264-GRP"), &ctx).accepted);
        let d = evaluate(&cand("Movie.2020.720p.BluRay.x264-GRP"), &ctx);
        assert!(!d.accepted);
        assert_eq!(specs(&d), vec!["quality"]);
        assert_eq!(d.rejections[0].kind, RejectionKind::Permanent);
    }

    #[test]
    fn custom_format_scores_and_minimum() {
        let mut p = hd1080();
        p.format_items = vec![FormatItem {
            format_id: 7,
            score: -100,
        }];
        p.min_format_score = 0;
        let formats = vec![CustomFormat {
            id: 7,
            name: "x265 (HD)".into(),
            include_in_rename: false,
            specifications: vec![FormatSpecification::new(
                "x265",
                Condition::ReleaseTitle {
                    value: Pattern::new(r"[xh][ .]?265|HEVC"),
                },
            )],
        }];
        let mut ctx = DecisionContext::new(&p, now());
        ctx.formats = &formats;
        let d = evaluate(&cand("Movie.2020.1080p.BluRay.x265-GRP"), &ctx);
        assert_eq!(d.format_score, -100);
        assert_eq!(d.matched_formats[0].name, "x265 (HD)");
        assert_eq!(specs(&d), vec!["custom_format_score"]);
    }

    #[test]
    fn profile_size_limits_scale_with_episodes() {
        let mut profile = hd1080();
        profile.min_size_mb = Some(500);
        profile.max_size_mb = Some(4096);
        let ctx = DecisionContext::new(&profile, now());
        let mut c = cand("Movie.2020.1080p.BluRay.x264-GRP");
        c.size = 2 * 1024 * 1024 * 1024;
        assert!(specs(&evaluate(&c, &ctx)).is_empty());
        c.size = 100 * 1024 * 1024;
        assert_eq!(specs(&evaluate(&c, &ctx)), vec!["size"]);
        c.size = 5 * 1024 * 1024 * 1024;
        assert_eq!(specs(&evaluate(&c, &ctx)), vec!["size"]);
        // A 10-episode pack may be ten times larger.
        let mut pack = DecisionContext::new(&profile, now());
        pack.episode_count = 10;
        c.size = 30 * 1024 * 1024 * 1024;
        assert!(specs(&evaluate(&c, &pack)).is_empty());
        // Unknown size is not rejected.
        c.size = 0;
        assert!(specs(&evaluate(&c, &ctx)).is_empty());
    }

    #[test]
    fn size_limits_use_runtime() {
        let p = hd1080();
        let defs = default_definitions();
        let mut ctx = DecisionContext::new(&p, now());
        ctx.definitions = &defs;
        ctx.runtime_minutes = Some(100);
        let mut c = cand("Movie.2020.1080p.BluRay.x264-GRP");
        c.size = 100 * 1024 * 1024; // 1 MB/min < 5 MB/min
        assert_eq!(specs(&evaluate(&c, &ctx)), vec!["size"]);
        c.size = 8 * 1024 * 1024 * 1024;
        assert!(evaluate(&c, &ctx).accepted);
        c.size = 40 * 1024 * 1024 * 1024;
        assert_eq!(specs(&evaluate(&c, &ctx)), vec!["size"]);
    }

    #[test]
    fn upgrades() {
        let p = hd1080();
        let s = |q: Quality, version: u8, score: i32| Scored {
            quality: QualityModel {
                quality: q,
                revision: reelix_parser::Revision {
                    version,
                    ..Default::default()
                },
            },
            format_score: score,
        };
        assert!(is_upgrade(
            &s(Quality::WebDl1080p, 1, 0),
            &s(Quality::Bluray1080p, 1, 0),
            &p
        ));
        assert!(!is_upgrade(
            &s(Quality::Bluray1080p, 1, 0),
            &s(Quality::Remux1080p, 1, 0),
            &p
        )); // cutoff met
        assert!(is_upgrade(
            &s(Quality::Bluray1080p, 1, 0),
            &s(Quality::Bluray1080p, 2, 0),
            &p
        )); // proper
        assert!(!is_upgrade(
            &s(Quality::WebDl1080p, 1, 0),
            &s(Quality::WebRip1080p, 1, 0),
            &p
        )); // same group
        assert!(is_upgrade(
            &s(Quality::WebDl1080p, 1, 0),
            &s(Quality::WebRip1080p, 1, 10),
            &p
        )); // CF gain
        assert!(!is_upgrade(
            &s(Quality::Bluray1080p, 1, 0),
            &s(Quality::WebDl1080p, 1, 0),
            &p
        ));
        let mut locked = p.clone();
        locked.upgrade_allowed = false;
        assert!(!is_upgrade(
            &s(Quality::WebDl1080p, 1, 0),
            &s(Quality::Bluray1080p, 1, 0),
            &locked
        ));

        let mut ctx = DecisionContext::new(&p, now());
        ctx.existing = vec![ExistingFile {
            quality: QualityModel::new(Quality::Bluray1080p),
            format_ids: vec![],
            languages: vec![],
        }];
        let d = evaluate(&cand("Movie.2020.1080p.WEB-DL.x264-GRP"), &ctx);
        assert_eq!(specs(&d), vec!["upgrade"]);
    }

    #[test]
    fn delay_profile_temporary_and_bypass() {
        let p = hd1080();
        let delay = DelayProfile {
            torrent_delay: 120,
            bypass_if_highest_quality: true,
            ..Default::default()
        };
        let mut ctx = DecisionContext::new(&p, now());
        ctx.delay_profile = Some(&delay);
        let mut c = cand("Movie.2020.1080p.WEB-DL.x264-GRP");
        c.publish_date = Some(now() - Duration::minutes(30));
        let d = evaluate(&c, &ctx);
        assert_eq!(specs(&d), vec!["delay"]);
        assert!(d.is_temporarily_rejected());
        c.publish_date = Some(now() - Duration::minutes(180));
        assert!(evaluate(&c, &ctx).accepted);
        // Best allowed quality bypasses the delay.
        let mut remux = cand("Movie.2020.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-GRP");
        remux.publish_date = Some(now() - Duration::minutes(5));
        assert!(evaluate(&remux, &ctx).accepted);
        ctx.search_kind = SearchKind::Interactive;
        c.publish_date = Some(now());
        assert!(evaluate(&c, &ctx).accepted);
    }

    #[test]
    fn misc_specs() {
        let p = hd1080();
        let rp = vec![ReleaseProfile {
            id: 1,
            name: "no cams".into(),
            enabled: true,
            required: vec![],
            ignored: vec!["/\\bYIFY\\b/i".into()],
            indexer_id: None,
            tags: vec![],
        }];
        let block = |c: &ReleaseCandidate| c.guid == "blocked";
        let queued = |c: &ReleaseCandidate| c.title.contains("QUEUED");
        let mut ctx = DecisionContext::new(&p, now());
        ctx.release_profiles = &rp;
        ctx.is_blocklisted = Some(&block);
        ctx.is_queued = Some(&queued);
        ctx.minimum_seeders = 5;
        ctx.allowed_indexers = Some(vec![1]);
        ctx.unavailable_reason = Some("Movie is not released yet".into());
        let mut c = cand("Movie.2020.FRENCH.1080p.BluRay.x264-YIFY.QUEUED");
        c.guid = "blocked".into();
        c.seeders = Some(1);
        c.indexer_id = Some(2);
        let d = evaluate(&c, &ctx);
        let s = specs(&d);
        for want in [
            "blocklist",
            "language",
            "release_profile",
            "queue",
            "availability",
            "seeders",
            "indexer",
        ] {
            assert!(s.contains(&want), "{want} missing from {s:?}");
        }
        let mut u = ReleaseCandidate::new("Movie.2020.1080p.BluRay.x264-GRP", Protocol::Usenet);
        let mut ctx = DecisionContext::new(&p, now());
        ctx.usenet_retention_days = Some(10);
        ctx.minimum_age_minutes = 30;
        u.publish_date = Some(now() - Duration::days(20));
        assert_eq!(specs(&evaluate(&u, &ctx)), vec!["retention"]);
        u.publish_date = Some(now() - Duration::minutes(5));
        assert_eq!(specs(&evaluate(&u, &ctx)), vec!["minimum_age"]);
        assert_eq!(specs(&evaluate(&cand("..."), &ctx)), vec!["parse", "quality"]);
    }

    #[test]
    fn prioritization_order() {
        let p = hd1080();
        let ctx = DecisionContext::new(&p, now());
        let mk = |title: &str, proto: Protocol, prio: i32, seeders: u32| {
            let mut c = ReleaseCandidate::new(title, proto);
            c.indexer_priority = prio;
            c.seeders = Some(seeders);
            c.size = 5 * 1024 * 1024 * 1024;
            let decision = evaluate(&c, &ctx);
            RankedRelease {
                candidate: c,
                decision,
            }
        };
        let mut items = vec![
            mk("Movie.2020.720p.BluRay.x264-REJ", Protocol::Torrent, 1, 100),
            mk("Movie.2020.1080p.WEB-DL.x264-WEBGRP", Protocol::Torrent, 1, 100),
            mk("Movie.2020.1080p.BluRay.x264-LOWSEED", Protocol::Torrent, 25, 3),
            mk("Movie.2020.1080p.BluRay.x264-HISEED", Protocol::Torrent, 25, 50),
            mk("Movie.2020.1080p.BluRay.x264-NZB", Protocol::Usenet, 25, 0),
            mk(
                "Movie.2020.1080p.BluRay.PROPER.x264-PROPERGRP",
                Protocol::Torrent,
                25,
                1,
            ),
        ];
        prioritize(&mut items, Some(Protocol::Usenet));
        let groups: Vec<&str> = items
            .iter()
            .map(|i| i.candidate.parsed.release_group.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(
            groups,
            vec!["PROPERGRP", "NZB", "HISEED", "LOWSEED", "WEBGRP", "REJ"]
        );
    }
}
