//! Decision context building: everything [`reelix_decision::evaluate`] needs for one target
//! (profile, formats, definitions, delay/release profiles, tags, language, existing files,
//! blocklist, queue and availability).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto};
use reelix_books::BookTarget;
use reelix_core::{BookFormat, ContentKind, Protocol};
use reelix_decision::{
    CustomFormat, Decision, DecisionContext, DelayProfile, ExistingFile, QualityDefinition, QualityProfile,
    RankedRelease, Rejection, RejectionKind, ReleaseCandidate, ReleaseProfile, SearchKind, prioritize,
    size_runtime_minutes,
};
use reelix_import::matching::ExpectedEpisode;
use reelix_indexers::ReleaseInfo;
use reelix_music::AlbumTarget;
use reelix_parser::{Language, QualityFamily};
use reelix_providers::ProviderService;

use crate::store::ContentRef;
use crate::store::blocklist::{self, BlocklistEntry};
use crate::store::tracked::{self, TrackedDownload};

use super::mapping::expected_episodes;

/// Reelix-level settings of one indexer row.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexerInfo {
    pub id: i64,
    pub name: String,
    pub priority: i32,
    pub tags: Vec<i64>,
    pub protocol: Protocol,
    pub enable_rss: bool,
    pub minimum_seeders: u32,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<u32>,
}

/// Configuration and queue snapshot shared by every evaluation in one run.
pub struct Shared {
    pub profiles: HashMap<i64, QualityProfile>,
    pub formats: Vec<CustomFormat>,
    pub definitions: Vec<QualityDefinition>,
    pub delay_profiles: Vec<DelayProfile>,
    pub release_profiles: Vec<ReleaseProfile>,
    pub indexers: HashMap<i64, IndexerInfo>,
    /// Active tracked downloads ("already grabbed" checks).
    pub queue: Vec<TrackedDownload>,
    pub now: DateTime<Utc>,
    pub today: NaiveDate,
    /// Book and music profiles (every family; loaded by [`Shared::load_content`]).
    pub content_profiles: HashMap<i64, QualityProfile>,
    /// Ebook / audiobook / music quality definitions.
    pub content_definitions: HashMap<QualityFamily, Vec<QualityDefinition>>,
    content_loaded: bool,
}

impl Shared {
    pub async fn load(
        db: &reelix_db::Db,
        providers: &ProviderService,
        today: NaiveDate,
    ) -> reelix_db::Result<Self> {
        let indexers = providers
            .indexers()
            .into_iter()
            .map(|(row, ix)| {
                let info = IndexerInfo {
                    id: row.id,
                    name: row.name.clone(),
                    priority: row.priority,
                    tags: row.tags.clone(),
                    protocol: ix.protocol(),
                    enable_rss: row.setting_bool("enable_rss", true),
                    minimum_seeders: row.setting_u64("minimum_seeders").unwrap_or(0) as u32,
                    seed_ratio: row.settings.get("seed_ratio").and_then(|v| {
                        v.as_f64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    }),
                    seed_time_minutes: row.setting_u64("seed_time_minutes").map(|v| v as u32),
                };
                (row.id, info)
            })
            .collect();
        Ok(Self {
            profiles: db
                .quality_profiles()
                .list()
                .await?
                .into_iter()
                .map(|p| (p.id, p))
                .collect(),
            formats: db.custom_formats().list().await?,
            definitions: db.quality_definitions().list().await?,
            delay_profiles: db.delay_profiles().list().await?,
            release_profiles: db.release_profiles().list().await?,
            indexers,
            queue: tracked::active(db).await?,
            now: Utc::now(),
            today,
            content_profiles: HashMap::new(),
            content_definitions: HashMap::new(),
            content_loaded: false,
        })
    }

    /// Loads book/music profiles and definitions (seeding their defaults on first use). Only
    /// book/album searches call it, so movie/TV runs never touch the other families.
    pub async fn load_content(&mut self, db: &reelix_db::Db) -> reelix_db::Result<()> {
        if self.content_loaded {
            return Ok(());
        }
        for f in [
            QualityFamily::Ebook,
            QualityFamily::Audiobook,
            QualityFamily::Music,
        ] {
            for p in db.quality_profiles().list_family(f).await? {
                self.content_profiles.insert(p.id, p);
            }
            self.content_definitions
                .insert(f, db.quality_definitions().list_family(f).await?);
        }
        self.content_loaded = true;
        Ok(())
    }

    pub fn priority(&self, indexer_id: i64) -> i32 {
        self.indexers.get(&indexer_id).map_or(25, |i| i.priority)
    }

    pub fn delay_profile(&self, tags: &[i64]) -> Option<&DelayProfile> {
        DelayProfile::select(&self.delay_profiles, tags)
    }

    /// Indexers usable for media with `tags` (untagged indexers, or a shared tag).
    pub fn allowed_indexers(&self, tags: &[i64]) -> Vec<i64> {
        self.indexers
            .values()
            .filter(|i| i.tags.is_empty() || i.tags.iter().any(|t| tags.contains(t)))
            .map(|i| i.id)
            .collect()
    }
}

/// What a search is for.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Target {
    Movie(MovieDto),
    Series {
        series: SeriesDto,
        /// Every episode of the series (with files), for matching.
        episodes: Vec<EpisodeDto>,
        expected: Vec<ExpectedEpisode>,
    },
    /// One format of a book.
    Book(Box<BookTarget>),
    Album(Box<AlbumTarget>),
}

impl Target {
    pub fn series(series: SeriesDto, episodes: Vec<EpisodeDto>) -> Self {
        let expected = expected_episodes(&episodes);
        Self::Series {
            series,
            episodes,
            expected,
        }
    }

    pub fn tags(&self) -> &[i64] {
        match self {
            Self::Movie(m) => &m.tags,
            Self::Series { series, .. } => &series.tags,
            Self::Book(b) => &b.tags,
            Self::Album(a) => &a.tags,
        }
    }

    pub fn profile_id(&self) -> i64 {
        match self {
            Self::Movie(m) => m.quality_profile_id,
            Self::Series { series, .. } => series.quality_profile_id,
            Self::Book(b) => b.quality_profile_id,
            Self::Album(a) => a.quality_profile_id,
        }
    }

    pub fn media_id(&self) -> i64 {
        match self {
            Self::Movie(m) => m.id,
            Self::Series { series, .. } => series.id,
            Self::Book(b) => b.book.id,
            Self::Album(a) => a.album.id,
        }
    }

    pub fn movie_id(&self) -> Option<i64> {
        match self {
            Self::Movie(m) => Some(m.id),
            _ => None,
        }
    }

    pub fn series_id(&self) -> Option<i64> {
        match self {
            Self::Series { series, .. } => Some(series.id),
            _ => None,
        }
    }

    /// Book (format) / album reference (empty for movies and series).
    pub fn content(&self) -> ContentRef {
        match self {
            Self::Book(b) => ContentRef::book(b.book.id, b.format),
            Self::Album(a) => ContentRef::album(a.album.id),
            _ => ContentRef::default(),
        }
    }

    pub fn is_content(&self) -> bool {
        matches!(self, Self::Book(_) | Self::Album(_))
    }

    /// Ebook / audiobook / music for content targets.
    pub fn content_kind(&self) -> Option<ContentKind> {
        match self {
            Self::Book(b) => Some(b.format.content_kind()),
            Self::Album(_) => Some(ContentKind::Music),
            _ => None,
        }
    }

    /// Quality family of the target's profile.
    pub fn family(&self) -> QualityFamily {
        match self {
            Self::Book(b) => QualityFamily::of_book(b.format),
            Self::Album(_) => QualityFamily::Music,
            _ => QualityFamily::Video,
        }
    }

    pub fn book_format(&self) -> Option<BookFormat> {
        match self {
            Self::Book(b) => Some(b.format),
            _ => None,
        }
    }

    /// Whether the media (and, for books, the format) is monitored.
    pub fn monitored(&self) -> bool {
        match self {
            Self::Movie(m) => m.monitored,
            Self::Series { series, .. } => series.monitored,
            Self::Book(b) => {
                b.author.monitored
                    && match b.format {
                        BookFormat::Ebook => b.book.monitored_ebook,
                        BookFormat::Audiobook => b.book.monitored_audiobook,
                    }
            }
            Self::Album(a) => a.album.monitored && a.artist.monitored,
        }
    }

    pub fn episodes(&self) -> &[EpisodeDto] {
        match self {
            Self::Series { episodes, .. } => episodes,
            _ => &[],
        }
    }

    pub fn episodes_by_id(&self, ids: &[i64]) -> Vec<EpisodeDto> {
        ids.iter()
            .filter_map(|id| self.episodes().iter().find(|e| e.id == *id).cloned())
            .collect()
    }

    fn original_language(&self) -> Option<Language> {
        let code = match self {
            Self::Movie(m) => m.original_language.as_deref(),
            Self::Series { series, .. } => series.original_language.as_deref(),
            // Book/music profiles have no language requirement (see reelix_decision::family).
            Self::Book(_) | Self::Album(_) => None,
        }?;
        Language::from_iso639_1(code)
    }
}

/// An indexer result with its decision.
#[derive(Debug, Clone)]
pub struct Eval {
    pub info: ReleaseInfo,
    pub ranked: RankedRelease,
}

impl Eval {
    pub fn decision(&self) -> &Decision {
        &self.ranked.decision
    }

    pub fn candidate(&self) -> &ReleaseCandidate {
        &self.ranked.candidate
    }

    /// Rejected only because of the delay profile.
    pub fn is_delayed(&self) -> bool {
        let d = self.decision();
        !d.accepted && !d.rejections.is_empty() && d.rejections.iter().all(|r| r.spec == "delay")
    }
}

pub fn rejection(spec: &str, reason: impl Into<String>) -> Rejection {
    Rejection {
        reason: reason.into(),
        kind: RejectionKind::Permanent,
        spec: spec.into(),
    }
}

/// Sorts best-first with [`prioritize`].
pub fn rank(evals: Vec<Eval>, preferred: Option<Protocol>) -> Vec<Eval> {
    let mut ranked: Vec<RankedRelease> = evals.iter().map(|e| e.ranked.clone()).collect();
    prioritize(&mut ranked, preferred);
    let mut infos: HashMap<(Option<i64>, String), ReleaseInfo> = evals
        .into_iter()
        .map(|e| {
            (
                (e.ranked.candidate.indexer_id, e.ranked.candidate.guid.clone()),
                e.info,
            )
        })
        .collect();
    ranked
        .into_iter()
        .filter_map(|r| {
            let info = infos.remove(&(r.candidate.indexer_id, r.candidate.guid.clone()))?;
            Some(Eval { info, ranked: r })
        })
        .collect()
}

fn existing_of(f: &reelix_api_types::library::MediaFileDto) -> ExistingFile {
    ExistingFile {
        quality: f.quality,
        format_ids: f.custom_format_ids.clone(),
        languages: f.languages.clone(),
    }
}

/// Evaluates candidates for one target.
pub struct Evaluator<'a> {
    pub shared: &'a Shared,
    pub target: &'a Target,
    pub blocklist: Vec<BlocklistEntry>,
}

impl<'a> Evaluator<'a> {
    pub async fn new(db: &reelix_db::Db, shared: &'a Shared, target: &'a Target) -> reelix_db::Result<Self> {
        let blocklist = if target.is_content() {
            blocklist::for_content(db, &target.content()).await?
        } else {
            blocklist::for_media(db, target.movie_id(), target.series_id()).await?
        };
        Ok(Self {
            shared,
            target,
            blocklist,
        })
    }

    fn unavailable(&self, c: &ReleaseCandidate) -> Option<String> {
        match self.target {
            Target::Movie(m) => (!m.is_available).then(|| {
                format!(
                    "Movie is not available yet (minimum availability: {})",
                    serde_json::to_value(m.minimum_availability)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                        .replace('_', " ")
                )
            }),
            Target::Series { episodes, .. } => episodes
                .iter()
                .filter(|e| c.episode_ids.contains(&e.id))
                .find(|e| e.air_date.is_none_or(|d| d > self.shared.today))
                .map(|e| {
                    format!(
                        "S{:02}E{:02} has not aired yet",
                        e.season_number, e.episode_number
                    )
                }),
            Target::Book(b) => b
                .book
                .release_date
                .filter(|d| *d > self.shared.today)
                .map(|d| format!("Book is not released yet ({d})")),
            Target::Album(a) => a
                .album
                .release_date
                .filter(|d| *d > self.shared.today)
                .map(|d| format!("Album is not released yet ({d})")),
        }
    }

    /// The target's quality profile (video profiles for movies/series, family profiles otherwise).
    fn profile(&self) -> Option<&'a QualityProfile> {
        let id = self.target.profile_id();
        if self.target.is_content() {
            self.shared.content_profiles.get(&id)
        } else {
            self.shared.profiles.get(&id)
        }
    }

    fn definitions(&self) -> &'a [QualityDefinition] {
        if self.target.is_content() {
            self.shared
                .content_definitions
                .get(&self.target.family())
                .map_or(&[], Vec::as_slice)
        } else {
            &self.shared.definitions
        }
    }

    fn existing(&self, c: &ReleaseCandidate) -> Vec<ExistingFile> {
        match self.target {
            Target::Movie(m) => m.movie_file.iter().map(|f| existing_of(&f.file)).collect(),
            Target::Series { episodes, .. } => {
                let mut seen = HashSet::new();
                episodes
                    .iter()
                    .filter(|e| c.episode_ids.contains(&e.id))
                    .filter_map(|e| e.episode_file.as_ref())
                    .filter(|f| seen.insert(f.file.id))
                    .map(|f| existing_of(&f.file))
                    .collect()
            }
            Target::Book(b) => b
                .existing
                .iter()
                .map(|f| ExistingFile {
                    quality: f.quality,
                    format_ids: f.custom_format_ids.clone(),
                    languages: Vec::new(),
                })
                .collect(),
            // The album counts as its lowest-quality track.
            Target::Album(a) => {
                let profile = self.profile();
                let rank =
                    |q: reelix_parser::Quality| profile.and_then(|p| p.rank(q)).map_or(-1, |r| r as i64);
                a.existing
                    .iter()
                    .min_by_key(|f| rank(f.quality.quality))
                    .map(|f| ExistingFile {
                        quality: f.quality,
                        format_ids: f.custom_format_ids.clone(),
                        languages: Vec::new(),
                    })
                    .into_iter()
                    .collect()
            }
        }
    }

    fn runtime(&self) -> Option<u32> {
        match self.target {
            Target::Movie(m) => m.runtime_minutes,
            Target::Series { series, episodes, .. } => series
                .runtime_minutes
                .or_else(|| episodes.iter().find_map(|e| e.runtime_minutes)),
            // Ebooks: Some(1) (per-book sizes); audiobook: edition duration; album: track minutes.
            Target::Book(b) => size_runtime_minutes(self.target.family(), b.runtime_minutes),
            Target::Album(a) => size_runtime_minutes(QualityFamily::Music, a.runtime_minutes),
        }
    }

    fn queued(&self, c: &ReleaseCandidate, profile: &QualityProfile) -> bool {
        let rank = profile.rank(c.parsed.quality.quality);
        self.shared.queue.iter().any(|t| {
            let covers = match self.target {
                Target::Movie(m) => t.movie_id == Some(m.id),
                Target::Series { series, .. } => {
                    t.series_id == Some(series.id) && t.episode_ids.iter().any(|e| c.episode_ids.contains(e))
                }
                Target::Book(_) | Target::Album(_) => t.content == self.target.content(),
            };
            covers
                && t.release
                    .as_ref()
                    .is_none_or(|r| profile.rank(r.quality.quality) >= rank)
        })
    }

    /// Runs the decision engine; `extra` rejections (mapping, monitoring) are merged in.
    pub fn evaluate(
        &self,
        c: &ReleaseCandidate,
        info_hash: Option<&str>,
        kind: SearchKind,
        extra: Vec<Rejection>,
    ) -> Decision {
        let Some(profile) = self.profile() else {
            return Decision {
                accepted: false,
                rejections: vec![rejection(
                    "quality",
                    "The media's quality profile no longer exists",
                )],
                format_score: 0,
                matched_formats: Vec::new(),
                quality_rank: None,
            };
        };
        let tags = self.target.tags();
        let blocked = |c: &ReleaseCandidate| {
            blocklist::is_blocked(&self.blocklist, &c.title, info_hash.or(c.parsed.hash.as_deref()))
        };
        let queued = |c: &ReleaseCandidate| self.queued(c, profile);
        let mut ctx = DecisionContext::new(profile, self.shared.now);
        ctx.formats = &self.shared.formats;
        ctx.definitions = self.definitions();
        ctx.existing = self.existing(c);
        ctx.runtime_minutes = self.runtime();
        ctx.episode_count = (c.episode_ids.len() as u32).max(1);
        ctx.release_profiles = &self.shared.release_profiles;
        ctx.delay_profile = self.shared.delay_profile(tags);
        ctx.media_tags = tags.to_vec();
        ctx.original_language = self.target.original_language();
        ctx.is_blocklisted = Some(&blocked);
        ctx.is_queued = Some(&queued);
        ctx.unavailable_reason = self.unavailable(c);
        ctx.minimum_seeders = c
            .indexer_id
            .and_then(|id| self.shared.indexers.get(&id))
            .map_or(0, |i| i.minimum_seeders);
        ctx.allowed_indexers = Some(self.shared.allowed_indexers(tags));
        ctx.search_kind = kind;
        let mut d = reelix_decision::evaluate(c, &ctx);
        if !extra.is_empty() {
            d.accepted = false;
            let mut all = extra;
            all.append(&mut d.rejections);
            d.rejections = all;
        }
        d
    }
}
