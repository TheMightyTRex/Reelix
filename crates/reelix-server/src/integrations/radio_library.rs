//! Radio's podcast library reader: the Audiobookshelf podcast library chosen in the radio
//! settings (`abs_library_id`), read through the Audiobookshelf servers configured under
//! media servers ([`reelix_radio::RadioLibrarySync`]).
//!
//! Each sync finds the enabled Audiobookshelf server holding that podcast library, reads every
//! episode with an audio file and hands them to
//! [`RadioService::sync_library_files`](reelix_radio::RadioService::sync_library_files)
//! (upsert, remove unseen files, match, mark synced). Problems found on the way (no server,
//! no library chosen, unreachable, folder not mounted) are kept as health checks while radio
//! follows shows. [`RadioLibrarySync::request_scan`] asks the same server to scan the library.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use anyhow::anyhow;
use reelix_api::HealthSource;
use reelix_api_types::{HealthCheck, HealthLevel};
use reelix_mediaserver::BuildContext;
use reelix_mediaserver::audiobookshelf::{AbsLibrary, AbsPodcastEpisode, Audiobookshelf};
use reelix_providers::ProviderRow;
use reelix_radio::{LibrarySyncSummary, NewEpisodeFile, RadioLibrarySync, RadioService};

const SOURCE: &str = "radio";

type RowsFn = dyn Fn() -> Vec<ProviderRow> + Send + Sync;

/// Reads the radio podcast library from Audiobookshelf.
pub struct AbsRadioLibrary {
    /// Configured media servers (only enabled `audiobookshelf` rows are used).
    servers: Arc<RowsFn>,
    ctx: BuildContext,
    radio: Weak<RadioService>,
    health: Mutex<Vec<HealthCheck>>,
}

impl AbsRadioLibrary {
    pub fn new(servers: Arc<RowsFn>, ctx: BuildContext, radio: Weak<RadioService>) -> Arc<Self> {
        Arc::new(Self {
            servers,
            ctx,
            radio,
            health: Mutex::new(Vec::new()),
        })
    }

    fn set_health(&self, checks: Vec<HealthCheck>) {
        *self.health.lock().unwrap_or_else(|p| p.into_inner()) = checks;
    }

    /// The enabled Audiobookshelf servers, by name.
    fn clients(&self) -> Vec<(String, Audiobookshelf)> {
        (self.servers)()
            .into_iter()
            .filter(|r| r.enabled && r.implementation == "audiobookshelf")
            .filter_map(|r| {
                let settings = serde_json::Value::Object(r.settings.clone());
                match Audiobookshelf::from_settings(&settings, &self.ctx) {
                    Ok(c) => Some((r.name, c)),
                    Err(e) => {
                        tracing::warn!(server = %r.name, error = %e, "Audiobookshelf settings are invalid");
                        None
                    }
                }
            })
            .collect()
    }

    /// The server holding podcast library `id`, with the library.
    async fn locate(&self, id: &str) -> Result<(Audiobookshelf, AbsLibrary), String> {
        let clients = self.clients();
        if clients.is_empty() {
            return Err(
                "Radio follows shows but no Audiobookshelf server is set up; add one under Settings → Media servers"
                    .into(),
            );
        }
        let mut errors = Vec::new();
        for (name, c) in clients {
            match c.podcast_libraries().await {
                Ok(libs) => {
                    if let Some(l) = libs.into_iter().find(|l| l.id == id) {
                        return Ok((c, l));
                    }
                }
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(if errors.is_empty() {
            format!(
                "Audiobookshelf has no podcast library {id}; choose the radio podcast library again under Settings → Radio"
            )
        } else {
            format!(
                "Reelix cannot read the radio podcast library from Audiobookshelf ({})",
                errors.join("; ")
            )
        })
    }

    async fn run(&self, svc: &RadioService) -> anyhow::Result<(LibrarySyncSummary, Vec<HealthCheck>)> {
        let settings = svc.settings().await?;
        let warn = |m: String| HealthCheck {
            source: SOURCE.into(),
            level: HealthLevel::Warning,
            message: m,
        };
        let Some(id) = settings.abs_library_id.clone() else {
            return Ok((
                LibrarySyncSummary::default(),
                vec![warn(
                    "Radio follows shows but no Audiobookshelf podcast library is chosen; pick one under Settings → Radio"
                        .into(),
                )],
            ));
        };
        let (abs, library) = match self.locate(&id).await {
            Ok(x) => x,
            Err(m) => {
                return Ok((
                    LibrarySyncSummary::default(),
                    vec![HealthCheck {
                        source: SOURCE.into(),
                        level: HealthLevel::Error,
                        message: m,
                    }],
                ));
            }
        };
        // Links to the podcasts' pages in Audiobookshelf ("Open in Audiobookshelf").
        svc.set_abs_web_url(Some(abs.url())).await?;
        let mut checks = Vec::new();
        if let Some(f) = &settings.abs_library_folder
            && !library.folders.iter().any(|x| within(f, &x.full_path))
        {
            checks.push(warn(format!(
                "The radio folder as Audiobookshelf sees it ({f}) is not in the podcast library “{}” ({}); check Settings → Radio",
                library.name,
                library
                    .folders
                    .iter()
                    .map(|x| x.full_path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(f) = settings.library_folder.clone() {
            let res =
                tokio::task::spawn_blocking(move || reelix_radio::check_library_folder(&PathBuf::from(f)))
                    .await
                    .map_err(|e| anyhow!(e))?;
            if let Err(m) = res {
                checks.push(warn(format!("Radio: {m}")));
            }
        }
        let episodes = match abs.podcast_episodes(&id).await {
            Ok(e) => e,
            Err(e) => {
                checks.push(HealthCheck {
                    source: SOURCE.into(),
                    level: HealthLevel::Error,
                    message: format!(
                        "Reelix cannot read the radio podcast library “{}” from Audiobookshelf ({e})",
                        library.name
                    ),
                });
                return Ok((LibrarySyncSummary::default(), checks));
            }
        };
        let files: Vec<NewEpisodeFile> = episodes.into_iter().map(to_file).collect();
        let summary = svc.sync_library_files(&files).await?;
        Ok((summary, checks))
    }
}

/// Whether `path` is `folder` or inside it (either separator, trailing ones ignored).
fn within(path: &str, folder: &str) -> bool {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_owned();
    let (p, f) = (norm(path), norm(folder));
    p == f || p.starts_with(&format!("{f}/"))
}

fn to_file(e: AbsPodcastEpisode) -> NewEpisodeFile {
    NewEpisodeFile {
        abs_item_id: e.item_id,
        abs_episode_id: e.episode_id,
        item_title: e.item_title,
        title: e.title,
        season: e.season,
        episode: e.episode,
        guid: e.guid,
        enclosure_url: e.enclosure_url,
        published_at: e.published_at,
        duration_secs: e.duration_secs,
        path: e.path,
        size: e.size,
    }
}

#[async_trait::async_trait]
impl RadioLibrarySync for AbsRadioLibrary {
    async fn sync(&self, svc: &RadioService) -> anyhow::Result<LibrarySyncSummary> {
        let followed = svc.followed_show_count().await? > 0;
        let settings = svc.settings().await?;
        if settings.abs_library_id.is_none() && !followed {
            self.set_health(Vec::new());
            return Ok(LibrarySyncSummary::default());
        }
        let (summary, checks) = self.run(svc).await?;
        let failed = checks.iter().any(|c| c.level == HealthLevel::Error);
        let message = checks
            .iter()
            .find(|c| c.level == HealthLevel::Error)
            .map(|c| c.message.clone());
        self.set_health(if followed { checks } else { Vec::new() });
        if failed {
            return Err(anyhow!(message.unwrap_or_default()));
        }
        Ok(summary)
    }

    async fn request_scan(&self) -> anyhow::Result<()> {
        let radio = self
            .radio
            .upgrade()
            .ok_or_else(|| anyhow!("the radio service is shutting down"))?;
        let Some(id) = radio.settings().await?.abs_library_id else {
            return Ok(());
        };
        let (abs, _) = self.locate(&id).await.map_err(|m| anyhow!(m))?;
        abs.scan_library(&id).await?;
        Ok(())
    }
}

impl HealthSource for AbsRadioLibrary {
    fn checks(&self) -> Vec<HealthCheck> {
        self.health.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn folders_within() {
        assert!(super::within("/podcasts", "/podcasts/"));
        assert!(super::within("/podcasts/radio", "/podcasts"));
        assert!(!super::within("/podcasts2", "/podcasts"));
        assert!(super::within("D:\\Pods\\Radio", "D:/Pods"));
    }
}
