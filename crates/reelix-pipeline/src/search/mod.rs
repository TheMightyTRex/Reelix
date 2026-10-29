//! Search → decision → grab (README "Core Pipelines" §2): automatic searches (job `search`),
//! interactive search + grab, RSS sync, delay-profile holds (pending releases) and the
//! blocklist / pending APIs.
//!
//! [`SearchService`] is registered as an `AppState` extension; [`jobs::register`] adds the
//! `search`, `rss_sync` and `process_pending` handlers and tasks.

pub mod context;
pub mod dto;
mod grab;
pub mod jobs;
pub mod mapping;
mod pending;
pub mod query;
mod rss;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reelix_api_types::library::{
    CalendarKind, EpisodeDto, LibrarySearchKind, MovieDto, SearchRequest, SeriesDto, WantedQuery,
};
use reelix_api_types::pipeline::{GrabRequest, GrabResponse, ReleaseDto, ReleaseQuery};
use reelix_books::BooksService;
use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_decision::{RankedRelease, SearchKind};
use reelix_indexers::{ReleaseInfo, SearchQuery};
use reelix_library::{LibraryError, LibraryService};
use reelix_music::MusicService;
use reelix_providers::ProviderService;

pub use context::{Eval, Evaluator, IndexerInfo, Shared, Target};
pub use grab::GrabTarget;

use crate::GrabbedRelease;

/// Errors surfaced to the API / jobs.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Validation(String),
    /// No usable download client / indexer, or the client refused the release.
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Db(#[from] reelix_db::DbError),
    #[error(transparent)]
    Library(#[from] LibraryError),
}

pub type Result<T, E = SearchError> = std::result::Result<T, E>;

/// Tunables.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Pause between items when walking the wanted lists (`missing` / `cutoff` searches).
    pub throttle: Duration,
    /// How long interactive-search results stay grabbable.
    pub cache_ttl: Duration,
    /// Per-indexer RSS timeout.
    pub rss_timeout: Duration,
    /// Accepted releases tried per item when a grab fails.
    pub max_grab_attempts: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            throttle: Duration::from_secs(2),
            cache_ttl: Duration::from_secs(30 * 60),
            rss_timeout: Duration::from_secs(60),
            max_grab_attempts: 3,
        }
    }
}

/// An interactive-search result remembered for grabbing.
#[derive(Debug, Clone)]
struct Cached {
    release: GrabbedRelease,
    movie_id: Option<i64>,
    episode_ids: Vec<i64>,
    rejections: Vec<String>,
    at: Instant,
}

/// What a search run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchSummary {
    pub searched: usize,
    pub grabbed: usize,
    pub pending: usize,
}

/// Searches indexers, decides and grabs.
pub struct SearchService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) providers: Arc<ProviderService>,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) opts: SearchOptions,
    cache: Mutex<HashMap<(i64, String), Cached>>,
    pub(crate) rss_last: Mutex<Option<Instant>>,
    /// Book and album searches (set by the server after construction).
    #[allow(dead_code)] // read by the book/album search paths
    pub(crate) books: OnceLock<Arc<BooksService>>,
    #[allow(dead_code)] // read by the book/album search paths
    pub(crate) music: OnceLock<Arc<MusicService>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// An episode worth searching for: monitored (with its series), aired, and missing or below cutoff.
pub fn episode_wanted(s: &SeriesDto, e: &EpisodeDto, today: chrono::NaiveDate) -> bool {
    s.monitored
        && e.monitored
        && e.season_number > 0
        && e.air_date.is_some_and(|d| d <= today)
        && e.episode_file.as_ref().is_none_or(|f| !f.file.cutoff_met)
        && (e.episode_file.is_some() || !e.has_file)
}

impl SearchService {
    pub fn new(
        db: Db,
        events: EventBus,
        providers: Arc<ProviderService>,
        library: Arc<LibraryService>,
        opts: SearchOptions,
    ) -> Self {
        Self {
            db,
            events,
            providers,
            library,
            opts,
            cache: Mutex::new(HashMap::new()),
            rss_last: Mutex::new(None),
            books: OnceLock::new(),
            music: OnceLock::new(),
        }
    }

    /// Connects the books and music services (server build phase). Later calls are ignored.
    pub fn set_content(&self, books: Arc<BooksService>, music: Arc<MusicService>) {
        let _ = self.books.set(books);
        let _ = self.music.set(music);
    }

    pub(crate) fn emit(&self, entity: &str, action: &str, id: i64) {
        self.events.publish(DomainEvent::EntityChanged {
            entity: entity.into(),
            action: action.into(),
            id,
            user_id: None,
        });
    }

    pub(crate) async fn shared(&self) -> Result<Shared> {
        Ok(Shared::load(&self.db, &self.providers, self.library.search_api().today()).await?)
    }

    // ------------------------------------------------------------------
    // Targets
    // ------------------------------------------------------------------

    pub(crate) async fn movie(&self, id: i64) -> Result<MovieDto> {
        self.library
            .search_api()
            .movies(Some(&[id]))
            .await?
            .pop()
            .ok_or_else(|| SearchError::NotFound(format!("movie {id} not found")))
    }

    pub(crate) async fn series_target(&self, id: i64) -> Result<Target> {
        let api = self.library.search_api();
        let s = api
            .series(Some(&[id]))
            .await?
            .pop()
            .ok_or_else(|| SearchError::NotFound(format!("series {id} not found")))?;
        let eps = api.episodes_of(&s, None).await?;
        Ok(Target::series(s, eps))
    }

    // ------------------------------------------------------------------
    // Indexer queries + evaluation
    // ------------------------------------------------------------------

    /// Runs `queries` in order against every indexer with the matching search flag; results
    /// are deduplicated by (indexer, guid).
    async fn query_all(&self, queries: &[SearchQuery], interactive: bool) -> Vec<ReleaseInfo> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for q in queries {
            for r in self.providers.search(q, None, interactive).await {
                match r.result {
                    Ok(items) => {
                        for i in items {
                            if seen.insert((i.indexer_id, i.guid.clone())) {
                                out.push(i);
                            }
                        }
                    }
                    Err(e) => tracing::warn!(indexer = %r.name, error = %e, "indexer search failed"),
                }
            }
        }
        out
    }

    /// Indexer results for a target (movie: ids/title, then alternative titles; series:
    /// a season pack query or per-episode queries).
    async fn fetch(
        &self,
        t: &Target,
        season: Option<u32>,
        episodes: &[EpisodeDto],
        interactive: bool,
    ) -> Vec<ReleaseInfo> {
        match t {
            Target::Movie(m) => {
                let found = self.query_all(&[query::movie(m)], interactive).await;
                if found.is_empty() {
                    let fb = query::movie_fallbacks(m);
                    if !fb.is_empty() {
                        return self.query_all(&fb, interactive).await;
                    }
                }
                found
            }
            Target::Series {
                series,
                episodes: all,
                ..
            } => {
                let mut queries = Vec::new();
                if let Some(n) = season {
                    queries.push(query::season(series, n, all));
                }
                for e in episodes {
                    queries.extend(query::episode(series, e));
                }
                self.query_all(&queries, interactive).await
            }
        }
    }

    /// Maps and evaluates releases for `t`. Unmappable releases are dropped unless
    /// `interactive` (then rejected with the mapping reason).
    pub(crate) fn evaluate_all(
        &self,
        ev: &Evaluator<'_>,
        infos: Vec<ReleaseInfo>,
        kind: SearchKind,
        wanted: Option<&[i64]>,
    ) -> Vec<Eval> {
        let interactive = kind == SearchKind::Interactive;
        let mut out = Vec::new();
        for info in infos {
            let mut c = mapping::candidate(&info, ev.shared.priority(info.indexer_id));
            let mut extra = Vec::new();
            match ev.target {
                Target::Movie(m) => {
                    c.media_id = Some(m.id);
                    if let Err(reason) = mapping::map_to_movie(&info, &c.parsed, m) {
                        if !interactive {
                            continue;
                        }
                        extra.push(context::rejection("mapping", reason));
                    }
                }
                Target::Series {
                    series,
                    episodes,
                    expected,
                } => {
                    c.media_id = Some(series.id);
                    match mapping::map_to_series(&info, &c.parsed, series, expected) {
                        Ok(ids) => {
                            if let Some(w) = wanted
                                && !interactive
                                && !ids.iter().any(|id| w.contains(id))
                            {
                                continue;
                            }
                            if !interactive {
                                let unmonitored: Vec<String> = episodes
                                    .iter()
                                    .filter(|e| ids.contains(&e.id) && !(e.monitored && series.monitored))
                                    .map(|e| format!("S{:02}E{:02}", e.season_number, e.episode_number))
                                    .collect();
                                if !unmonitored.is_empty() {
                                    extra.push(context::rejection(
                                        "monitored",
                                        format!("Episode(s) not monitored: {}", unmonitored.join(", ")),
                                    ));
                                }
                            }
                            c.episode_ids = ids;
                        }
                        Err(reason) => {
                            if !interactive {
                                continue;
                            }
                            extra.push(context::rejection("mapping", reason));
                        }
                    }
                }
            }
            let decision = ev.evaluate(&c, info.info_hash.as_deref(), kind, extra);
            out.push(Eval {
                info,
                ranked: RankedRelease {
                    candidate: c,
                    decision,
                },
            });
        }
        let preferred = ev
            .shared
            .delay_profile(ev.target.tags())
            .map(|d| d.preferred_protocol);
        context::rank(out, preferred)
    }

    // ------------------------------------------------------------------
    // Interactive search
    // ------------------------------------------------------------------

    /// `GET /releases`: searches with interactive-enabled indexers, evaluates every result
    /// against the media and caches them (30 min) for [`Self::grab_request`].
    pub async fn interactive(&self, q: &ReleaseQuery) -> Result<Vec<ReleaseDto>> {
        let (target, season, episodes) = if let Some(id) = q.movie_id {
            (Target::Movie(self.movie(id).await?), None, Vec::new())
        } else if let Some(eid) = q.episode_id {
            let ep = self
                .library
                .search_api()
                .episodes_by_ids(&[eid])
                .await?
                .pop()
                .ok_or_else(|| SearchError::NotFound(format!("episode {eid} not found")))?;
            let t = self.series_target(ep.series_id).await?;
            let ep = t.episodes_by_id(&[eid]).pop().unwrap_or(ep);
            (t, None, vec![ep])
        } else if let Some(sid) = q.series_id {
            let season = q.season.ok_or_else(|| {
                SearchError::Validation("series search needs `season` (or use episode_id)".into())
            })?;
            (self.series_target(sid).await?, Some(season), Vec::new())
        } else {
            return Err(SearchError::Validation(
                "one of movie_id, series_id + season or episode_id is required".into(),
            ));
        };
        let shared = self.shared().await?;
        let infos = self.fetch(&target, season, &episodes, true).await;
        let ev = Evaluator::new(&self.db, &shared, &target).await?;
        let evals = self.evaluate_all(&ev, infos, SearchKind::Interactive, None);
        let n = evals.len() as u32;
        let now = Instant::now();
        let mut cache = lock(&self.cache);
        cache.retain(|_, c| now.duration_since(c.at) < self.opts.cache_ttl);
        let mut out = Vec::with_capacity(evals.len());
        for (i, e) in evals.iter().enumerate() {
            let snapshot = grab::snapshot(e, shared.indexers.get(&e.info.indexer_id));
            cache.insert(
                (e.info.indexer_id, e.candidate().guid.clone()),
                Cached {
                    release: snapshot,
                    movie_id: target.movie_id(),
                    episode_ids: e.candidate().episode_ids.clone(),
                    rejections: e.decision().rejections.iter().map(|r| r.reason.clone()).collect(),
                    at: now,
                },
            );
            out.push(dto::release_dto(e, &target, n - i as u32, shared.now));
        }
        Ok(out)
    }

    /// `POST /releases/grab`: grabs a cached interactive result (or a raw `download_url`).
    pub async fn grab_request(&self, req: &GrabRequest) -> Result<GrabResponse> {
        let cached = {
            let cache = lock(&self.cache);
            cache
                .get(&(req.indexer_id, req.guid.clone()))
                .filter(|c| c.at.elapsed() < self.opts.cache_ttl)
                .cloned()
        };
        let (release, movie_id, mut episode_ids) = match cached {
            Some(c) => {
                if !c.rejections.is_empty() && !req.force {
                    return Err(SearchError::Validation(format!(
                        "Release was rejected ({}); use force to grab anyway",
                        c.rejections.join("; ")
                    )));
                }
                (c.release, c.movie_id, c.episode_ids)
            }
            None => {
                let Some(url) = req.download_url.clone().filter(|u| !u.trim().is_empty()) else {
                    return Err(SearchError::NotFound(
                        "release not found in the search results cache; search again".into(),
                    ));
                };
                (self.raw_release(req, url)?, None, Vec::new())
            }
        };
        let movie_id = req.movie_id.or(movie_id);
        if let Some(ids) = &req.episode_ids {
            episode_ids = ids.clone();
        }
        let target = if req.movie_id.is_some() {
            self.grab_target(movie_id, &[]).await?
        } else if req.episode_ids.is_some() {
            self.grab_target(None, &episode_ids).await?
        } else {
            self.grab_target(movie_id, &episode_ids).await?
        };
        self.grab(&release, &target, req.download_client_id).await
    }

    fn raw_release(&self, req: &GrabRequest, url: String) -> Result<GrabbedRelease> {
        let (row, ix) = self
            .providers
            .indexers()
            .into_iter()
            .find(|(r, _)| r.id == req.indexer_id)
            .ok_or_else(|| {
                SearchError::NotFound(format!("indexer {} not found or disabled", req.indexer_id))
            })?;
        let title = req.title.clone().unwrap_or_else(|| req.guid.clone());
        let parsed = reelix_parser::parse(&title);
        let magnet = url.starts_with("magnet:");
        Ok(GrabbedRelease {
            guid: req.guid.clone(),
            title,
            indexer_id: Some(row.id),
            indexer: row.name.clone(),
            protocol: ix.protocol(),
            size: None,
            publish_date: None,
            download_url: (!magnet).then(|| url.clone()),
            magnet_url: magnet.then_some(url),
            info_url: None,
            info_hash: None,
            seeders: None,
            quality: parsed.quality,
            languages: parsed.languages,
            release_group: parsed.release_group,
            custom_formats: Vec::new(),
            format_score: 0,
            indexer_flags: Vec::new(),
            seed_ratio: row.settings.get("seed_ratio").and_then(serde_json::Value::as_f64),
            seed_time_minutes: row.setting_u64("seed_time_minutes").map(|v| v as u32),
        })
    }

    // ------------------------------------------------------------------
    // Automatic search
    // ------------------------------------------------------------------

    /// Executes a `search` job payload. `stop` is polled between items (shutdown).
    pub async fn run_search(
        &self,
        req: &SearchRequest,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Result<SearchSummary> {
        let mut shared = self.shared().await?;
        let mut sum = SearchSummary::default();
        let api = self.library.search_api();
        let today = shared.today;
        let media = req.media.unwrap_or(CalendarKind::Movie);
        match req.kind {
            LibrarySearchKind::Movie => {
                for id in &req.ids {
                    if stop() {
                        break;
                    }
                    match self.movie(*id).await {
                        Ok(m) => self.auto_movie(&mut shared, m, &mut sum).await?,
                        Err(e) => tracing::warn!(movie_id = id, error = %e, "search: movie skipped"),
                    }
                }
            }
            LibrarySearchKind::Series | LibrarySearchKind::Season => {
                for id in &req.ids {
                    if stop() {
                        break;
                    }
                    let t = match self.series_target(*id).await {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!(series_id = id, error = %e, "search: series skipped");
                            continue;
                        }
                    };
                    let Target::Series { series, episodes, .. } = &t else {
                        continue;
                    };
                    let mut seasons: Vec<u32> = episodes
                        .iter()
                        .filter(|e| episode_wanted(series, e, today))
                        .filter(|e| {
                            req.kind == LibrarySearchKind::Series || Some(e.season_number) == req.season
                        })
                        .map(|e| e.season_number)
                        .collect();
                    seasons.dedup();
                    for season in seasons {
                        let wanted: Vec<i64> = episodes
                            .iter()
                            .filter(|e| e.season_number == season && episode_wanted(series, e, today))
                            .map(|e| e.id)
                            .collect();
                        self.auto_season(&mut shared, &t, season, &wanted, &mut sum)
                            .await?;
                    }
                }
            }
            LibrarySearchKind::Episode => {
                let eps = api.episodes_by_ids(&req.ids).await?;
                self.auto_episode_set(&mut shared, eps, false, stop, &mut sum)
                    .await?;
            }
            LibrarySearchKind::Missing | LibrarySearchKind::Cutoff => {
                let ids: Vec<i64> = if req.ids.is_empty() {
                    let q = WantedQuery {
                        kind: Some(media),
                        page: None,
                        page_size: Some(0),
                        sort: None,
                        dir: None,
                        monitored: Some(true),
                    };
                    let page = if req.kind == LibrarySearchKind::Missing {
                        self.library.wanted_missing(&q).await?
                    } else {
                        self.library.wanted_cutoff(&q).await?
                    };
                    page.items.into_iter().map(|w| w.id).collect()
                } else {
                    req.ids.clone()
                };
                match media {
                    CalendarKind::Movie => {
                        for (i, id) in ids.iter().enumerate() {
                            if stop() {
                                break;
                            }
                            if i > 0 {
                                tokio::time::sleep(self.opts.throttle).await;
                            }
                            match self.movie(*id).await {
                                Ok(m) => self.auto_movie(&mut shared, m, &mut sum).await?,
                                Err(e) => tracing::warn!(movie_id = id, error = %e, "search: movie skipped"),
                            }
                        }
                    }
                    CalendarKind::Episode => {
                        let eps = api.episodes_by_ids(&ids).await?;
                        self.auto_episode_set(&mut shared, eps, true, stop, &mut sum)
                            .await?;
                    }
                }
            }
        }
        tracing::info!(
            kind = ?req.kind,
            searched = sum.searched,
            grabbed = sum.grabbed,
            pending = sum.pending,
            "search finished"
        );
        Ok(sum)
    }

    /// Episodes grouped by series + season: a season search when several episodes of one
    /// season are wanted, else per-episode searches.
    async fn auto_episode_set(
        &self,
        shared: &mut Shared,
        eps: Vec<EpisodeDto>,
        throttle: bool,
        stop: &(dyn Fn() -> bool + Sync),
        sum: &mut SearchSummary,
    ) -> Result<()> {
        let mut groups: Vec<((i64, u32), Vec<i64>)> = Vec::new();
        for e in &eps {
            let key = (e.series_id, e.season_number);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(e.id),
                None => groups.push((key, vec![e.id])),
            }
        }
        let mut targets: HashMap<i64, Target> = HashMap::new();
        for (i, ((series_id, season), ids)) in groups.into_iter().enumerate() {
            if stop() {
                break;
            }
            if throttle && i > 0 {
                tokio::time::sleep(self.opts.throttle).await;
            }
            if let std::collections::hash_map::Entry::Vacant(v) = targets.entry(series_id) {
                match self.series_target(series_id).await {
                    Ok(t) => {
                        v.insert(t);
                    }
                    Err(e) => {
                        tracing::warn!(series_id, error = %e, "search: series skipped");
                        continue;
                    }
                }
            }
            let t = targets[&series_id].clone();
            if ids.len() > 1 {
                self.auto_season(shared, &t, season, &ids, sum).await?;
            } else {
                let ep = t.episodes_by_id(&ids);
                self.auto_episodes(shared, &t, &ep, sum).await?;
            }
        }
        Ok(())
    }

    async fn auto_movie(&self, shared: &mut Shared, m: MovieDto, sum: &mut SearchSummary) -> Result<()> {
        sum.searched += 1;
        let t = Target::Movie(m);
        let infos = self.fetch(&t, None, &[], false).await;
        let evals = {
            let ev = Evaluator::new(&self.db, shared, &t).await?;
            self.evaluate_all(&ev, infos, SearchKind::Automatic, None)
        };
        self.grab_best(shared, &t, evals, &[], false, sum).await?;
        Ok(())
    }

    /// Season pack first; then per-episode searches for wanted episodes it didn't cover.
    async fn auto_season(
        &self,
        shared: &mut Shared,
        t: &Target,
        season: u32,
        wanted: &[i64],
        sum: &mut SearchSummary,
    ) -> Result<()> {
        if wanted.is_empty() {
            return Ok(());
        }
        sum.searched += 1;
        let infos = self.fetch(t, Some(season), &[], false).await;
        let evals = {
            let ev = Evaluator::new(&self.db, shared, t).await?;
            self.evaluate_all(&ev, infos, SearchKind::Automatic, Some(wanted))
        };
        let covered = self.grab_best(shared, t, evals, wanted, true, sum).await?;
        let rest: Vec<i64> = wanted
            .iter()
            .copied()
            .filter(|id| !covered.contains(id))
            .collect();
        if !rest.is_empty() {
            let eps = t.episodes_by_id(&rest);
            self.auto_episodes(shared, t, &eps, sum).await?;
        }
        Ok(())
    }

    async fn auto_episodes(
        &self,
        shared: &mut Shared,
        t: &Target,
        eps: &[EpisodeDto],
        sum: &mut SearchSummary,
    ) -> Result<()> {
        let mut covered: Vec<i64> = Vec::new();
        for e in eps {
            if covered.contains(&e.id) {
                continue;
            }
            sum.searched += 1;
            let infos = self.fetch(t, None, std::slice::from_ref(e), false).await;
            let evals = {
                let ev = Evaluator::new(&self.db, shared, t).await?;
                self.evaluate_all(&ev, infos, SearchKind::Automatic, Some(&[e.id]))
            };
            covered.extend(self.grab_best(shared, t, evals, &[e.id], false, sum).await?);
        }
        Ok(())
    }

    /// Grabs the best accepted release(s) for a target, or holds a delayed one as pending.
    /// Returns the episode ids now covered (grabbed or pending).
    pub(crate) async fn grab_best(
        &self,
        shared: &mut Shared,
        t: &Target,
        evals: Vec<Eval>,
        wanted: &[i64],
        prefer_pack: bool,
        sum: &mut SearchSummary,
    ) -> Result<Vec<i64>> {
        let mut covered: Vec<i64> = Vec::new();
        let mut attempts = 0usize;
        if let Target::Movie(_) = t {
            for e in evals.iter().filter(|e| e.decision().accepted) {
                if attempts >= self.opts.max_grab_attempts {
                    break;
                }
                attempts += 1;
                if self.try_grab(shared, t, e).await {
                    sum.grabbed += 1;
                    return Ok(covered);
                }
            }
            if let Some(e) = evals.iter().find(|e| e.is_delayed())
                && self.hold(shared, t, e).await?
            {
                sum.pending += 1;
            }
            return Ok(covered);
        }
        // Season pack covering every wanted episode.
        if prefer_pack {
            let pack = evals.iter().filter(|e| {
                e.candidate().parsed.episode().is_some_and(|i| i.full_season)
                    && wanted.iter().all(|w| e.candidate().episode_ids.contains(w))
            });
            for e in pack.clone().filter(|e| e.decision().accepted) {
                if attempts >= self.opts.max_grab_attempts {
                    break;
                }
                attempts += 1;
                if self.try_grab(shared, t, e).await {
                    sum.grabbed += 1;
                    return Ok(wanted.to_vec());
                }
            }
            let best_pack = pack.clone().next();
            if let Some(e) = best_pack.filter(|e| e.is_delayed())
                && !evals.iter().any(|x| x.decision().accepted)
            {
                if self.hold(shared, t, e).await? {
                    sum.pending += 1;
                }
                return Ok(wanted.to_vec());
            }
        }
        let overlaps = |ids: &[i64], set: &[i64]| ids.iter().any(|i| set.contains(i));
        for e in evals.iter().filter(|e| e.decision().accepted) {
            let ids = &e.candidate().episode_ids;
            if overlaps(ids, &covered) || (!wanted.is_empty() && !overlaps(ids, wanted)) {
                continue;
            }
            if attempts >= self.opts.max_grab_attempts {
                break;
            }
            attempts += 1;
            if self.try_grab(shared, t, e).await {
                sum.grabbed += 1;
                covered.extend(ids.iter().copied());
            }
        }
        for e in evals.iter().filter(|e| e.is_delayed()) {
            let ids = &e.candidate().episode_ids;
            if overlaps(ids, &covered) || (!wanted.is_empty() && !overlaps(ids, wanted)) {
                continue;
            }
            if self.hold(shared, t, e).await? {
                sum.pending += 1;
            }
            covered.extend(ids.iter().copied());
        }
        Ok(covered)
    }

    /// Grabs one evaluated release; logs failures. On success the new download joins the
    /// queue snapshot so later evaluations see it.
    async fn try_grab(&self, shared: &mut Shared, t: &Target, e: &Eval) -> bool {
        let snapshot = grab::snapshot(e, shared.indexers.get(&e.info.indexer_id));
        let target = GrabTarget::from_target(t, &e.candidate().episode_ids);
        match self.grab(&snapshot, &target, None).await {
            Ok(r) => {
                if let Ok(Some(row)) = crate::store::tracked::get(&self.db, r.queue_id).await {
                    shared.queue.push(row);
                }
                true
            }
            Err(err) => {
                tracing::warn!(title = %snapshot.title, error = %err, "grab failed");
                false
            }
        }
    }
}
