//! Hand-offs for getting episodes that have no podcast feed: the wanted list an indexer
//! search starts from, and the file name filing uses.
//!
//! The indexer search ([`crate::search`], run by the pipeline), release parsing
//! (`reelix_parser::parse_radio`) and filing into the podcast library ([`crate::filing`]) build
//! on [`RadioService::wanted_episodes`] and [`episode_file_name`]; see `docs/radio.md`.

use chrono::{DateTime, Utc};
use reelix_api_types::radio::{RadioEpisodeDto, RadioEpisodeStatus, RadioShowDto};
use reelix_import::naming::{NamingConfig, sanitize};

use crate::error::Result;
use crate::status::filing_series_number;
use crate::{RadioService, repo};

/// A wanted episode with what a search and filing need.
#[derive(Debug, Clone, PartialEq)]
pub struct WantedEpisode {
    pub episode_id: i64,
    pub show_id: i64,
    pub show_title: String,
    pub station_name: Option<String>,
    /// Filing series number (specials = 0).
    pub series_number: u32,
    pub number: Option<u32>,
    pub title: String,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub external_id: String,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
}

impl RadioService {
    /// Episodes with status Wanted, of `show_ids` (`None` = every show), oldest first.
    pub async fn wanted_episodes(&self, show_ids: Option<&[i64]>) -> Result<Vec<WantedEpisode>> {
        let shows = repo::shows::load(&self.db, show_ids).await?;
        let mut out = Vec::new();
        for s in shows {
            for e in repo::episodes::for_show(&self.db, s.id).await? {
                if e.status != RadioEpisodeStatus::Wanted {
                    continue;
                }
                out.push(WantedEpisode::of(&s, e));
            }
        }
        out.sort_by_key(|w| (w.first_broadcast.is_none(), w.first_broadcast, w.episode_id));
        Ok(out)
    }
}

impl WantedEpisode {
    /// The search / filing facts of an episode of `show` (whatever its status).
    pub(crate) fn of(show: &RadioShowDto, e: RadioEpisodeDto) -> Self {
        Self {
            episode_id: e.id,
            show_id: show.id,
            show_title: show.title.clone(),
            station_name: show.station_name.clone(),
            series_number: filing_series_number(e.series_kind, e.series_number),
            number: e.number,
            title: e.title,
            first_broadcast: e.first_broadcast,
            external_id: e.external_id,
            quality_profile_id: show.quality_profile_id,
            root_folder_id: show.root_folder_id,
        }
    }
}

/// `{Show}/{Show} - S{series:00}E{episode:00} - {Title}.{ext}` (relative to the podcast
/// library folder), with characters file systems reject replaced.
pub fn episode_file_name(show: &str, series: u32, episode: Option<u32>, title: &str, ext: &str) -> String {
    let cfg = NamingConfig::default();
    let show = sanitize(show.trim(), &cfg);
    let ep = episode.map_or_else(|| "E00".to_owned(), |n| format!("E{n:02}"));
    let name = sanitize(&format!("{show} - S{series:02}{ep} - {}", title.trim()), &cfg);
    format!("{show}/{name}.{}", ext.trim_start_matches('.'))
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_names() {
        assert_eq!(
            super::episode_file_name("The News Quiz", 20, Some(3), "Episode 3", "mp3"),
            "The News Quiz/The News Quiz - S20E03 - Episode 3.mp3"
        );
        let n = super::episode_file_name("A/B", 0, None, "What? Now: yes", ".m4a");
        assert!(n.ends_with(".m4a"), "{n}");
        assert_eq!(n.matches('/').count(), 1, "{n}");
        assert!(n.contains("S00E00"), "{n}");
    }
}
