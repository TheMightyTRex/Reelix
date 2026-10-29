//! Delay-profile holds: releases rejected only by the delay wait are stored in
//! `pending_releases` and grabbed by `process_pending` once due (if still the best and wanted).

use std::collections::BTreeMap;

use chrono::Duration;
use reelix_api_types::pipeline::{GrabResponse, PendingReleaseDto};
use reelix_decision::{RankedRelease, SearchKind};
use reelix_indexers::ReleaseInfo;

use super::context::{Eval, Evaluator, Target};
use super::grab::{self, GrabTarget};
use super::mapping;
use super::{Result, SearchError, SearchService, SearchSummary};
use crate::GrabbedRelease;
use crate::store::pending::{self, PendingRelease};

/// Rebuilds an indexer result from a stored snapshot (for re-evaluation).
fn info_from(rel: &GrabbedRelease) -> ReleaseInfo {
    let mut i = ReleaseInfo::new(rel.protocol, rel.indexer_id.unwrap_or(0), &rel.indexer);
    i.guid = rel.guid.clone();
    i.title = rel.title.clone();
    i.download_url = rel.download_url.clone();
    i.magnet_url = rel.magnet_url.clone();
    i.info_url = rel.info_url.clone();
    i.info_hash = rel.info_hash.clone();
    i.size = rel.size;
    i.publish_date = rel.publish_date;
    i.seeders = rel.seeders;
    i.indexer_flags = rel
        .indexer_flags
        .iter()
        .filter_map(|f| serde_json::from_value(serde_json::Value::String(f.clone())).ok())
        .collect();
    i
}

fn group_key(p: &PendingRelease) -> String {
    let mut eps = p.episode_ids.clone();
    eps.sort_unstable();
    format!("{:?}:{:?}:{eps:?}", p.movie_id, p.series_id)
}

impl SearchService {
    /// Stores a delayed release; `false` when the same release is already pending.
    pub(crate) async fn hold(&self, shared: &super::Shared, t: &Target, e: &Eval) -> Result<bool> {
        let existing = pending::all(&self.db).await?;
        let movie_id = t.movie_id();
        let series_id = t.series_id();
        if existing.iter().any(|p| {
            p.movie_id == movie_id
                && p.series_id == series_id
                && p.release.guid == e.candidate().guid
                && p.release.indexer_id == Some(e.info.indexer_id)
        }) {
            return Ok(false);
        }
        let delay = shared
            .delay_profile(t.tags())
            .map_or(0, |d| d.delay_for(e.info.protocol));
        let base = e.info.publish_date.unwrap_or(shared.now);
        let release_at = (base + Duration::minutes(i64::from(delay))).max(shared.now);
        let snapshot = grab::snapshot(e, shared.indexers.get(&e.info.indexer_id));
        let id = pending::insert(
            &self.db,
            movie_id,
            series_id,
            &e.candidate().episode_ids,
            &snapshot,
            "delay",
            release_at.timestamp_millis(),
        )
        .await?;
        tracing::info!(title = %snapshot.title, release_at = %release_at, "release delayed by delay profile");
        self.emit("pending", "created", id);
        Ok(true)
    }

    /// Drops pending releases made obsolete by a grab (same movie, or overlapping episodes).
    pub(crate) async fn clear_pending_for(
        &self,
        movie_id: Option<i64>,
        series_id: Option<i64>,
        episode_ids: &[i64],
    ) -> Result<()> {
        if movie_id.is_none() && series_id.is_none() {
            return Ok(());
        }
        for p in pending::all(&self.db).await? {
            let hit = match movie_id {
                Some(m) => p.movie_id == Some(m),
                None => {
                    p.series_id == series_id
                        && (episode_ids.is_empty() || p.episode_ids.iter().any(|e| episode_ids.contains(e)))
                }
            };
            if hit && pending::delete(&self.db, p.id).await? {
                self.emit("pending", "deleted", p.id);
            }
        }
        Ok(())
    }

    async fn pending_target(&self, p: &PendingRelease) -> Result<Target> {
        match (p.movie_id, p.series_id) {
            (Some(m), _) => Ok(Target::Movie(self.movie(m).await?)),
            (None, Some(s)) => self.series_target(s).await,
            _ => Err(SearchError::NotFound("pending release has no media".into())),
        }
    }

    /// Task `process_pending`: grabs due releases that are still accepted (best per media
    /// item first); due releases that are no longer wanted are dropped.
    pub async fn process_pending(&self) -> Result<SearchSummary> {
        let now = chrono::Utc::now();
        let due = pending::due(&self.db, now.timestamp_millis()).await?;
        let mut sum = SearchSummary::default();
        if due.is_empty() {
            return Ok(sum);
        }
        let mut groups: BTreeMap<String, Vec<PendingRelease>> = BTreeMap::new();
        for p in due {
            groups.entry(group_key(&p)).or_default().push(p);
        }
        let mut shared = self.shared().await?;
        for (_, rows) in groups {
            let t = match self.pending_target(&rows[0]).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::info!(error = %e, "dropping pending releases for missing media");
                    for p in &rows {
                        pending::delete(&self.db, p.id).await?;
                        self.emit("pending", "deleted", p.id);
                    }
                    continue;
                }
            };
            let wanted_media = match &t {
                Target::Movie(m) => m.monitored,
                Target::Series { series, .. } => series.monitored,
            };
            let evals = {
                let ev = Evaluator::new(&self.db, &shared, &t).await?;
                let mut out = Vec::new();
                for p in &rows {
                    let info = info_from(&p.release);
                    let mut c = mapping::candidate(&info, shared.priority(info.indexer_id));
                    c.media_id = Some(t.media_id());
                    c.episode_ids = p.episode_ids.clone();
                    let decision =
                        ev.evaluate(&c, info.info_hash.as_deref(), SearchKind::Automatic, Vec::new());
                    out.push(Eval {
                        info,
                        ranked: RankedRelease {
                            candidate: c,
                            decision,
                        },
                    });
                }
                let preferred = shared.delay_profile(t.tags()).map(|d| d.preferred_protocol);
                super::context::rank(out, preferred)
            };
            for p in &rows {
                pending::delete(&self.db, p.id).await?;
                self.emit("pending", "deleted", p.id);
            }
            if !wanted_media {
                continue;
            }
            match evals.iter().find(|e| e.decision().accepted) {
                Some(best) => {
                    let snapshot = grab::snapshot(best, shared.indexers.get(&best.info.indexer_id));
                    let target = GrabTarget::from_target(&t, &best.candidate().episode_ids);
                    match self.grab(&snapshot, &target, None).await {
                        Ok(r) => {
                            sum.grabbed += 1;
                            if let Ok(Some(row)) = crate::store::tracked::get(&self.db, r.queue_id).await {
                                shared.queue.push(row);
                            }
                        }
                        Err(e) => tracing::warn!(title = %snapshot.title, error = %e, "pending grab failed"),
                    }
                }
                None => tracing::info!(
                    count = rows.len(),
                    "pending releases no longer acceptable; dropped"
                ),
            }
        }
        Ok(sum)
    }

    // ------------------------------------------------------------------
    // API
    // ------------------------------------------------------------------

    pub async fn list_pending(&self) -> Result<Vec<PendingReleaseDto>> {
        let rows = pending::all(&self.db).await?;
        let refs = self
            .media_refs(
                rows.iter().filter_map(|p| p.movie_id),
                rows.iter().filter_map(|p| p.series_id),
                rows.iter().flat_map(|p| p.episode_ids.iter().copied()),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|p| PendingReleaseDto {
                id: p.id,
                title: p.title,
                media: refs.media(p.movie_id, p.series_id),
                episodes: refs.episodes(&p.episode_ids),
                quality: p.release.quality,
                indexer: p.release.indexer,
                protocol: p.release.protocol,
                reason: p.reason,
                added: p.added,
                release_at: p.release_at,
            })
            .collect())
    }

    pub async fn delete_pending(&self, id: i64) -> Result<()> {
        if !pending::delete(&self.db, id).await? {
            return Err(SearchError::NotFound(format!("pending release {id} not found")));
        }
        self.emit("pending", "deleted", id);
        Ok(())
    }

    /// Grabs a pending release now (skipping the remaining delay).
    pub async fn grab_pending(&self, id: i64) -> Result<GrabResponse> {
        let p = pending::all(&self.db)
            .await?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| SearchError::NotFound(format!("pending release {id} not found")))?;
        let target = match (p.movie_id, p.series_id) {
            (Some(m), _) => self.grab_target(Some(m), &[]).await?,
            (None, Some(_)) => self.grab_target(None, &p.episode_ids).await?,
            _ => GrabTarget::default(),
        };
        let r = self.grab(&p.release, &target, None).await?;
        if pending::delete(&self.db, id).await? {
            self.emit("pending", "deleted", id);
        }
        Ok(r)
    }
}
