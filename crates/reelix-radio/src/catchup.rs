//! Catch-up downloads (RFX-96): wanted episodes that no podcast feed or download site brings, fetched from the
//! source's own episode page with yt-dlp and filed into the podcast library like any download.
//!
//! - Only sources named in the radio settings' `catchup_providers` (none by default), for monitored, wanted
//!   episodes of followed shows, oldest first, at most [`PER_RUN`] a run (hourly, [`crate::kinds::RADIO_CATCHUP`]).
//! - The fetching itself is the server's (a [`CatchupFetcher`] using `reelix-direct`'s yt-dlp, which accepts only
//!   the known catch-up sites); without one attached, nothing happens.
//! - Each episode is fetched into its own folder under `<data_dir>/radio/catchup/`, filed with
//!   [`RadioService::file_download`] (moved, so nothing is left behind), and the folder removed. A failure is
//!   logged and the episode tried again next run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use reelix_import::content::ContentDownload;
use reelix_import::files::TransferMode;

use crate::RadioService;
use crate::error::Result;

/// Episodes fetched at most per run, so a long list does not keep the server busy for hours.
pub const PER_RUN: usize = 5;

/// Fetches the audio at a catch-up page into a folder (the server's yt-dlp).
#[async_trait::async_trait]
pub trait CatchupFetcher: Send + Sync + 'static {
    async fn fetch(&self, page_url: &str, into: &Path) -> anyhow::Result<()>;
}

/// What a run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CatchupSummary {
    pub tried: usize,
    pub filed: usize,
    pub failed: usize,
}

impl RadioService {
    /// Attaches the fetcher (the server, with yt-dlp); until then catch-up does nothing.
    pub fn set_catchup(&self, fetcher: Arc<dyn CatchupFetcher>) {
        if let Ok(mut f) = self.catchup.write() {
            *f = Some(fetcher);
        }
    }

    fn catchup_dir(&self) -> Option<PathBuf> {
        self.data_dir.get().map(|d| d.join("radio").join("catchup"))
    }

    /// One catch-up run (see the module docs).
    pub async fn run_catchup(&self) -> Result<CatchupSummary> {
        let mut sum = CatchupSummary::default();
        let fetcher = self.catchup.read().ok().and_then(|f| f.clone());
        let Some(fetcher) = fetcher else {
            return Ok(sum);
        };
        let allowed = self.settings().await?.catchup_providers;
        if allowed.is_empty() {
            return Ok(sum);
        }
        let Some(base) = self.catchup_dir() else {
            return Ok(sum);
        };
        let shows = crate::repo::shows::load(&self.db, None).await?;
        let monitored: std::collections::HashSet<i64> =
            shows.iter().filter(|s| s.monitored).map(|s| s.id).collect();
        for w in self.wanted_episodes(None).await? {
            if sum.tried >= PER_RUN {
                break;
            }
            if !monitored.contains(&w.show_id) || !allowed.contains(&w.provider) {
                continue;
            }
            let Some(page) = self
                .providers
                .get(&w.provider)
                .and_then(|p| p.catchup_url(&w.external_id))
            else {
                continue;
            };
            // Already filed and waiting for the library to see it: not fetched again (as the indexer search).
            if self.filed_path(w.episode_id).await?.is_some() {
                continue;
            }
            sum.tried += 1;
            let dir = base.join(w.episode_id.to_string());
            let _ = tokio::fs::remove_dir_all(&dir).await;
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                tracing::warn!(error = %e, "radio catch-up folder not made");
                sum.failed += 1;
                continue;
            }
            let outcome = match fetcher.fetch(&page, &dir).await {
                Ok(()) => {
                    let d = ContentDownload {
                        source: dir.clone(),
                        release_title: Some(format!("{} - {}", w.show_title, w.title)),
                        mode: TransferMode::Move,
                        download_client: Some("catch-up".to_owned()),
                        download_id: None,
                    };
                    self.file_download(&d, w.episode_id)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            };
            match outcome {
                Ok(()) => {
                    sum.filed += 1;
                    tracing::info!(episode = w.episode_id, show = %w.show_title, "radio catch-up filed");
                }
                Err(e) => {
                    sum.failed += 1;
                    tracing::warn!(episode = w.episode_id, show = %w.show_title, error = %e, "radio catch-up failed");
                }
            }
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        Ok(sum)
    }
}
