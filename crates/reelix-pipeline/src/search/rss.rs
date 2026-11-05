//! RSS sync: latest releases from RSS-enabled indexers, mapped to monitored library media and
//! grabbed when they beat what's there (missing, or cutoff unmet). Releases in Audio/Other or
//! Audio/Audiobook (or without categories) are first matched against wanted radio episodes.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use reelix_core::{BookFormat, ContentKind};
use reelix_decision::SearchKind;
use reelix_import::matching::ExpectedEpisode;
use reelix_indexers::ReleaseInfo;

use super::context::{Evaluator, Target};
use super::mapping::{Mapped, MediaIndex, expected_episodes, may_be_radio, release_kind};
use super::{Result, SearchService, SearchSummary, episode_wanted, lock};

impl SearchService {
    /// Task `rss_sync`. Runs when `rss_interval_minutes` (indexer settings) has elapsed since
    /// the last sync (`force` ignores the interval; 0 disables unless forced).
    pub async fn rss_sync(&self, force: bool) -> Result<Option<SearchSummary>> {
        let interval = self.providers.indexer_settings().rss_interval_minutes;
        if !force {
            if interval == 0 {
                return Ok(None);
            }
            let last = *lock(&self.rss_last);
            if last.is_some_and(|l| l.elapsed() < Duration::from_secs(u64::from(interval) * 60 - 30)) {
                return Ok(None);
            }
        }
        *lock(&self.rss_last) = Some(Instant::now());
        let mut shared = self.shared().await?;
        let ids: Vec<i64> = self
            .providers
            .indexers()
            .iter()
            .filter(|(row, _)| row.setting_bool("enable_rss", true))
            .map(|(row, _)| row.id)
            .collect();
        if ids.is_empty() {
            return Ok(Some(SearchSummary::default()));
        }
        let results = self
            .providers
            .indexer_manager()
            .rss(&ids, &[], self.opts.rss_timeout)
            .await;
        self.providers.persist_indexer_status().await;
        let mut releases: Vec<ReleaseInfo> = Vec::new();
        for r in results {
            match r.result {
                Ok(items) => releases.extend(items),
                Err(e) => tracing::warn!(indexer = %r.name, error = %e, "RSS fetch failed"),
            }
        }
        let summary = self.rss_process(&mut shared, releases).await?;
        tracing::info!(
            grabbed = summary.grabbed,
            pending = summary.pending,
            "RSS sync finished"
        );
        Ok(Some(summary))
    }

    /// Maps + evaluates RSS releases and grabs the winners per media item.
    pub(crate) async fn rss_process(
        &self,
        shared: &mut super::Shared,
        releases: Vec<ReleaseInfo>,
    ) -> Result<SearchSummary> {
        let mut sum = SearchSummary::default();
        if releases.is_empty() {
            return Ok(sum);
        }
        let api = self.library.search_api();
        let movies: Vec<_> = api
            .movies(None)
            .await?
            .into_iter()
            .filter(|m| m.monitored)
            .collect();
        let series: Vec<_> = api
            .series(None)
            .await?
            .into_iter()
            .filter(|s| s.monitored)
            .collect();
        let mut all_eps = HashMap::new();
        let mut expected: HashMap<i64, Vec<ExpectedEpisode>> = HashMap::new();
        for s in &series {
            let eps = api.episodes_of(s, None).await?;
            expected.insert(s.id, expected_episodes(&eps));
            all_eps.insert(s.id, eps);
        }
        let index = MediaIndex::new(movies, series, expected);

        // Group releases by media.
        let mut movies_hit: BTreeMap<i64, Vec<ReleaseInfo>> = BTreeMap::new();
        let mut series_hit: BTreeMap<i64, Vec<ReleaseInfo>> = BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        let mut books_hit: BTreeMap<(i64, BookFormat), Vec<ReleaseInfo>> = BTreeMap::new();
        let mut albums_hit: BTreeMap<i64, Vec<ReleaseInfo>> = BTreeMap::new();
        let mut radio_hit: BTreeMap<i64, Vec<ReleaseInfo>> = BTreeMap::new();
        let radio_targets: Vec<reelix_radio::RadioTarget> = match self.radio.get() {
            Some(radio) => match radio
                .search_targets(&reelix_api_types::radio::RadioSearchRequest {
                    kind: reelix_api_types::content::ContentSearchKind::Missing,
                    ids: Vec::new(),
                })
                .await
            {
                Ok(t) => t.into_iter().filter(|t| t.monitored).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "RSS: wanted radio episodes could not be read");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        for r in releases {
            if !seen.insert((r.indexer_id, r.guid.clone())) {
                continue;
            }
            if !radio_targets.is_empty() && may_be_radio(&r) {
                let parsed = reelix_parser::parse_radio(&r.title);
                if let Some(t) = reelix_radio::match_release(&radio_targets, &parsed) {
                    radio_hit.entry(t.episode_id()).or_default().push(r);
                    continue;
                }
            }
            // Book / music categories go to the books and music libraries only.
            match release_kind(&r) {
                Some(k @ (ContentKind::Ebook | ContentKind::Audiobook)) => {
                    if let Some(books) = self.books.get() {
                        let hint = if k == ContentKind::Audiobook {
                            BookFormat::Audiobook
                        } else {
                            BookFormat::Ebook
                        };
                        let parsed = reelix_parser::parse_book(&r.title, Some(hint));
                        match books.search_api().match_release(&parsed).await {
                            Ok(Some(key)) => books_hit.entry(key).or_default().push(r),
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(title = %r.title, error = %e, "RSS: book matching failed")
                            }
                        }
                    }
                    continue;
                }
                Some(ContentKind::Music) => {
                    if let Some(music) = self.music.get() {
                        let parsed = reelix_parser::parse_music(&r.title);
                        match music.search_api().match_release(&parsed).await {
                            Ok(Some(id)) => albums_hit.entry(id).or_default().push(r),
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(title = %r.title, error = %e, "RSS: album matching failed")
                            }
                        }
                    }
                    continue;
                }
                _ => {}
            }
            let parsed = reelix_parser::parse(&r.title);
            match index.map(&r, &parsed) {
                Some(Mapped::Movie(id)) => movies_hit.entry(id).or_default().push(r),
                Some(Mapped::Episodes { series_id, .. }) => series_hit.entry(series_id).or_default().push(r),
                None => {}
            }
        }

        for (id, infos) in movies_hit {
            let Some(m) = index.movie(id).cloned() else {
                continue;
            };
            let t = Target::Movie(m);
            let evals = {
                let ev = Evaluator::new(&self.db, shared, &t).await?;
                self.evaluate_all(&ev, infos, SearchKind::Rss, None)
            };
            sum.searched += 1;
            self.grab_best(shared, &t, evals, &[], false, &mut sum).await?;
        }
        for (id, infos) in series_hit {
            let Some(s) = index.series_by_id(id).cloned() else {
                continue;
            };
            let eps = all_eps.remove(&id).unwrap_or_default();
            let wanted: Vec<i64> = eps
                .iter()
                .filter(|e| episode_wanted(&s, e, shared.today))
                .map(|e| e.id)
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let t = Target::series(s, eps);
            let evals = {
                let ev = Evaluator::new(&self.db, shared, &t).await?;
                self.evaluate_all(&ev, infos, SearchKind::Rss, Some(&wanted))
            };
            sum.searched += 1;
            self.grab_best(shared, &t, evals, &wanted, false, &mut sum)
                .await?;
        }
        if !books_hit.is_empty() || !albums_hit.is_empty() || !radio_hit.is_empty() {
            shared.load_content(&self.db).await?;
        }
        for ((id, format), infos) in books_hit {
            let Some(books) = self.books.get() else { break };
            // Only monitored formats that are missing or below the cutoff.
            let wanted = books
                .search_api()
                .targets(&[id], Some(format), reelix_books::Wanted::MissingOrCutoff)
                .await;
            let Some(b) = wanted.ok().and_then(|mut v| v.pop()) else {
                continue;
            };
            let t = Target::Book(Box::new(b));
            self.rss_content(shared, &t, infos, &mut sum).await?;
        }
        for (id, infos) in albums_hit {
            let Some(music) = self.music.get() else { break };
            let wanted = music
                .search_api()
                .targets(&[id], reelix_music::Wanted::MissingOrCutoff)
                .await;
            let Some(a) = wanted
                .ok()
                .and_then(|mut v| v.pop())
                .filter(|a| a.album.monitored && a.artist.monitored)
            else {
                continue;
            };
            let t = Target::Album(Box::new(a));
            self.rss_content(shared, &t, infos, &mut sum).await?;
        }
        for (id, infos) in radio_hit {
            let Some(t) = radio_targets.iter().find(|t| t.episode_id() == id) else {
                continue;
            };
            let t = Target::Radio(Box::new(t.clone()));
            self.rss_content(shared, &t, infos, &mut sum).await?;
        }
        Ok(sum)
    }

    async fn rss_content(
        &self,
        shared: &mut super::Shared,
        t: &Target,
        infos: Vec<ReleaseInfo>,
        sum: &mut SearchSummary,
    ) -> Result<()> {
        let evals = {
            let ev = Evaluator::new(&self.db, shared, t).await?;
            self.evaluate_all(&ev, infos, SearchKind::Rss, None)
        };
        sum.searched += 1;
        self.grab_best(shared, t, evals, &[], false, sum).await?;
        Ok(())
    }
}
