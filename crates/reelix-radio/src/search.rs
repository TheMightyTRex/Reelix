//! What an indexer search for radio looks for, and whether a release is the episode.
//!
//! The pipeline (`reelix_pipeline::search`) runs the searches: it asks
//! [`RadioService::search_targets`] for [`RadioTarget`]s, queries the Torznab/Newznab
//! indexers in the radio categories with [`RadioTarget::search_terms`], parses releases with
//! [`reelix_parser::parse_radio`], keeps the ones [`RadioTarget::check_release`] accepts, scores
//! them with the show's quality profile (or the first audiobook profile) and the delay
//! profiles, and grabs the best. RSS sync uses [`match_release`] against every wanted episode.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::radio::{RadioEpisodeStatus, RadioSearchRequest};
use reelix_db::NewJob;
use reelix_import::matching::{normalize_title, similarity};
use reelix_parser::{ParsedRadio, QualityFamily, is_bbc_pid};

use crate::error::{RadioError, Result};
use crate::wanted::WantedEpisode;
use crate::{RadioService, kinds, repo};

/// The provider whose episode ids are BBC programme ids.
const BBC_PROVIDER: &str = crate::providers::bbc::ID;

/// One episode to search for.
#[derive(Debug, Clone, PartialEq)]
pub struct RadioTarget {
    pub episode: WantedEpisode,
    /// The show's profile, else the first audiobook profile (radio ranks as audiobooks).
    pub quality_profile_id: i64,
    pub tags: Vec<i64>,
    /// Show and episode are monitored.
    pub monitored: bool,
    /// Known length (size checks).
    pub duration_secs: Option<u32>,
}

impl RadioTarget {
    pub fn episode_id(&self) -> i64 {
        self.episode.episode_id
    }

    /// `S20E03` (specials `S00E01`), or `None` without an episode number.
    pub fn number_label(&self) -> Option<String> {
        self.episode
            .number
            .map(|n| format!("S{:02}E{n:02}", self.episode.series_number))
    }

    /// The BBC programme id of a BBC episode (its external id), when it is one.
    pub fn bbc_pid(&self) -> Option<&str> {
        let e = &self.episode;
        let id = e.external_id.trim();
        (e.provider == BBC_PROVIDER && is_bbc_pid(id)).then_some(id)
    }

    /// Playing time in minutes for the size check (`None` = unknown, no size check).
    pub fn runtime_minutes(&self) -> Option<u32> {
        self.duration_secs.map(|s| s.div_ceil(60)).filter(|m| *m > 0)
    }

    /// Indexer search terms, most specific first: `Show S20E03`, `Show 2026 11 04`,
    /// `Show Episode title` (a title that only repeats the show or says "Episode 3" is left out).
    pub fn search_terms(&self) -> Vec<String> {
        let e = &self.episode;
        let show = e.show_title.trim();
        let mut out = Vec::new();
        if let Some(label) = self.number_label() {
            out.push(format!("{show} {label}"));
        }
        if let Some(d) = e.first_broadcast {
            out.push(format!("{show} {}", d.format("%Y %m %d")));
        }
        let title = e.title.trim();
        let key = normalize_title(title);
        if !key.is_empty() && key != normalize_title(show) && !generic_title(title) {
            out.push(format!("{show} {title}"));
        }
        if out.is_empty() {
            out.push(show.to_owned());
        }
        out.dedup();
        out
    }

    /// Whether a parsed release is this episode. A BBC programme id in the name decides for a
    /// BBC episode (the same id matches whatever the name says; another id is refused).
    /// Otherwise the show must match, then the numbers (series + episode), else the broadcast
    /// date (one day either way), else the episode title.
    pub fn check_release(&self, p: &ParsedRadio) -> std::result::Result<(), String> {
        if let (Some(got), Some(want)) = (p.pid.as_deref(), self.bbc_pid()) {
            return if got.eq_ignore_ascii_case(want) {
                Ok(())
            } else {
                Err(format!(
                    "Wrong episode: release is BBC programme {got}, wanted {want}"
                ))
            };
        }
        if !show_matches(&self.episode.show_title, p) {
            return Err(format!(
                "Wrong show: {:?} is not {}",
                p.show, self.episode.show_title
            ));
        }
        episode_matches(&self.episode, p)
    }
}

/// `Episode 3`, `Ep. 3`, `Part 2`: titles that say nothing a search could use.
fn generic_title(t: &str) -> bool {
    let w: Vec<&str> = t.split_whitespace().collect();
    matches!(w.as_slice(), [a, n] if ["episode", "ep", "ep.", "part", "programme", "show"]
        .contains(&a.to_ascii_lowercase().as_str()) && n.chars().all(|c| c.is_ascii_digit()))
}

/// Same show, ignoring case, punctuation and articles; a station or prefix left in front of
/// the name (`BBC Radio 4 Extra The Archers`) is tolerated.
pub fn show_matches(show_title: &str, p: &ParsedRadio) -> bool {
    let want = normalize_title(show_title);
    let got = normalize_title(&p.show);
    if want.is_empty() || got.is_empty() {
        return false;
    }
    got == want || similarity(&got, &want) >= 0.9 || (want.len() >= 6 && got.ends_with(&want))
}

/// Whether the release's episode identity fits `e` (see [`RadioTarget::check_release`]).
pub fn episode_matches(e: &WantedEpisode, p: &ParsedRadio) -> std::result::Result<(), String> {
    if let (Some(s), Some(n)) = (p.series, p.episode) {
        return if Some(n) == e.number && s == e.series_number {
            Ok(())
        } else {
            Err(format!(
                "Wrong episode: release is S{s:02}E{n:02}, wanted {}",
                e.number.map_or_else(
                    || "an unnumbered episode".to_owned(),
                    |x| format!("S{:02}E{x:02}", e.series_number)
                )
            ))
        };
    }
    if let Some(d) = p.air_date {
        return match e.first_broadcast {
            Some(fb) if (fb.date_naive() - d).num_days().abs() <= 1 => Ok(()),
            Some(fb) => Err(format!(
                "Wrong episode: release is from {d}, the episode was broadcast on {}",
                fb.date_naive()
            )),
            None => Err(format!(
                "Release is from {d} but the episode has no broadcast date"
            )),
        };
    }
    if let Some(t) = p.episode_title.as_deref() {
        let (a, b) = (normalize_title(t), normalize_title(&e.title));
        return if !b.is_empty() && (a == b || similarity(&a, &b) >= 0.85) {
            Ok(())
        } else {
            Err(format!("Wrong episode: {t:?} is not {:?}", e.title))
        };
    }
    Err("Release does not say which episode it is (no number, date or title)".into())
}

/// The first target a release is (RSS sync); `None` when none fits.
pub fn match_release<'a>(targets: &'a [RadioTarget], p: &ParsedRadio) -> Option<&'a RadioTarget> {
    if !p.identifies_episode() {
        return None;
    }
    targets.iter().find(|t| t.check_release(p).is_ok())
}

impl RadioService {
    /// Episodes to search for.
    ///
    /// - `item`: the given episodes, whatever their status, unless they already have a file
    ///   (in the library, or filed by Reelix and still on disk): a search asked for by hand.
    /// - `parent`: wanted episodes of the given shows; `missing` / `cutoff`: every wanted
    ///   episode ([`RadioService::wanted_episodes`]: monitored, broadcast, no file).
    ///
    /// Episodes filed by Reelix whose file still exists are left out until the library sync
    /// matches them.
    pub async fn search_targets(&self, req: &RadioSearchRequest) -> Result<Vec<RadioTarget>> {
        let wanted: Vec<WantedEpisode> = match req.kind {
            ContentSearchKind::Item => {
                let eps = repo::episodes::load(&self.db, &req.ids).await?;
                let show_ids: Vec<i64> = eps
                    .iter()
                    .map(|e| e.show_id)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let shows: HashMap<i64, _> = repo::shows::load(&self.db, Some(&show_ids))
                    .await?
                    .into_iter()
                    .map(|s| (s.id, s))
                    .collect();
                eps.into_iter()
                    .filter(|e| e.status != RadioEpisodeStatus::InLibrary)
                    .filter_map(|e| shows.get(&e.show_id).map(|s| WantedEpisode::of(s, e)))
                    .collect()
            }
            ContentSearchKind::Parent => self.wanted_episodes(Some(&req.ids)).await?,
            ContentSearchKind::Missing | ContentSearchKind::Cutoff => self.wanted_episodes(None).await?,
        };
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = wanted.iter().map(|w| w.episode_id).collect();
        let filed = repo::episodes::filed(&self.db, Some(&ids)).await?;
        let on_disk: HashSet<i64> = tokio::task::spawn_blocking(move || {
            filed
                .into_iter()
                .filter(|(_, p)| Path::new(p).is_file())
                .map(|(id, _)| id)
                .collect()
        })
        .await
        .map_err(|e| RadioError::Internal(e.to_string()))?;
        let episodes: HashMap<i64, _> = repo::episodes::load(&self.db, &ids)
            .await?
            .into_iter()
            .map(|e| (e.id, e))
            .collect();
        let show_ids: Vec<i64> = wanted
            .iter()
            .map(|w| w.show_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let shows: HashMap<i64, _> = repo::shows::load(&self.db, Some(&show_ids))
            .await?
            .into_iter()
            .map(|s| (s.id, s))
            .collect();
        let default_profile = self
            .db
            .quality_profiles()
            .list_family(QualityFamily::Audiobook)
            .await?
            .first()
            .map(|p| p.id);
        let mut out = Vec::with_capacity(wanted.len());
        for w in wanted {
            if on_disk.contains(&w.episode_id) {
                continue;
            }
            let Some(profile) = w.quality_profile_id.or(default_profile) else {
                tracing::warn!(
                    episode_id = w.episode_id,
                    "radio search: no audiobook quality profile"
                );
                continue;
            };
            let ep = episodes.get(&w.episode_id);
            let show = shows.get(&w.show_id);
            out.push(RadioTarget {
                quality_profile_id: profile,
                tags: show.map(|s| s.tags.clone()).unwrap_or_default(),
                monitored: show.is_some_and(|s| s.monitored) && ep.is_some_and(|e| e.monitored),
                duration_secs: ep.and_then(|e| e.duration_secs),
                episode: w,
            });
        }
        Ok(out)
    }

    /// The target of one episode for a grab or a delayed release (any status).
    pub async fn search_target(&self, episode_id: i64) -> Result<RadioTarget> {
        repo::episodes::require(&self.db, episode_id).await?;
        self.search_targets(&RadioSearchRequest {
            kind: ContentSearchKind::Item,
            ids: vec![episode_id],
        })
        .await?
        .pop()
        .ok_or_else(|| {
            RadioError::Validation(format!(
                "radio episode {episode_id} already has a file or no audiobook quality profile exists"
            ))
        })
    }

    /// Queues an indexer search (job `radio_episode_search`, run by the pipeline).
    pub async fn request_search(&self, req: RadioSearchRequest) -> Result<Option<i64>> {
        if matches!(req.kind, ContentSearchKind::Item | ContentSearchKind::Parent) && req.ids.is_empty() {
            return Err(RadioError::Validation("`ids` must not be empty".into()));
        }
        match req.kind {
            ContentSearchKind::Item => {
                for id in &req.ids {
                    repo::episodes::require(&self.db, *id).await?;
                }
            }
            ContentSearchKind::Parent => {
                for id in &req.ids {
                    repo::shows::require(&self.db, *id).await?;
                }
            }
            _ => {}
        }
        let mut ids = req.ids.clone();
        ids.sort_unstable();
        ids.dedup();
        let kind = crate::convert::enum_str(&req.kind);
        let dedupe = format!(
            "{}:{kind}:{}",
            kinds::RADIO_EPISODE_SEARCH,
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        let payload = serde_json::to_value(RadioSearchRequest { kind: req.kind, ids })
            .map_err(|e| RadioError::Internal(e.to_string()))?;
        self.enqueue(NewJob::new(kinds::RADIO_EPISODE_SEARCH, payload).dedupe(dedupe))
            .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use reelix_parser::parse_radio;

    use super::*;

    fn bbc(show: &str, series: u32, number: Option<u32>, pid: &str) -> RadioTarget {
        let mut t = target(show, series, number, "Episode", None);
        t.episode.provider = "bbc".into();
        t.episode.external_id = pid.into();
        t
    }

    fn target(
        show: &str,
        series: u32,
        number: Option<u32>,
        title: &str,
        date: Option<(i32, u32, u32)>,
    ) -> RadioTarget {
        RadioTarget {
            episode: WantedEpisode {
                episode_id: 1,
                show_id: 1,
                show_title: show.into(),
                provider: "rss".into(),
                station_name: Some("BBC Radio 4".into()),
                series_number: series,
                number,
                title: title.into(),
                first_broadcast: date
                    .map(|(y, m, d)| chrono::Utc.with_ymd_and_hms(y, m, d, 18, 15, 0).unwrap()),
                external_id: "ep-1".into(),
                quality_profile_id: None,
                root_folder_id: None,
            },
            quality_profile_id: 1,
            tags: vec![],
            monitored: true,
            duration_secs: Some(13 * 60),
        }
    }

    #[test]
    fn terms() {
        let t = target("The News Quiz", 20, Some(3), "Episode 3", Some((2026, 11, 4)));
        assert_eq!(
            t.search_terms(),
            ["The News Quiz S20E03", "The News Quiz 2026 11 04"]
        );
        let t = target("Desert Island Discs", 1, None, "Dolly Parton", None);
        assert_eq!(t.search_terms(), ["Desert Island Discs Dolly Parton"]);
        let t = target("Show", 0, Some(1), "Christmas Special", None);
        assert_eq!(t.search_terms(), ["Show S00E01", "Show Christmas Special"]);
        assert_eq!(t.runtime_minutes(), Some(13));
    }

    #[test]
    fn checks() {
        let t = target("The Archers", 1, Some(20412), "Monday", Some((2026, 11, 4)));
        assert!(
            t.check_release(&parse_radio("BBC.Radio.4.The.Archers.2026.11.04.mp3"))
                .is_ok()
        );
        assert!(t.check_release(&parse_radio("The Archers 2026-11-05")).is_ok());
        assert!(
            t.check_release(&parse_radio("The Archers 2026-11-09"))
                .unwrap_err()
                .contains("Wrong episode")
        );
        assert!(
            t.check_release(&parse_radio("Ambridge Stories 2026-11-04"))
                .unwrap_err()
                .contains("Wrong show")
        );

        let t = target("The News Quiz", 20, Some(3), "Episode 3", None);
        assert!(t.check_release(&parse_radio("The.News.Quiz.S20E03.AAC")).is_ok());
        assert!(
            t.check_release(&parse_radio(
                "[BBC R4] The News Quiz - S20E04 - Episode 4 (128kbps)"
            ))
            .is_err()
        );
        assert!(t.check_release(&parse_radio("The News Quiz")).is_err());

        let t = target("Desert Island Discs", 1, None, "Dolly Parton", None);
        assert!(
            t.check_release(&parse_radio("Desert Island Discs - Dolly Parton (128kbps)"))
                .is_ok()
        );
        assert!(
            t.check_release(&parse_radio("Desert Island Discs - Elton John"))
                .is_err()
        );

        let targets = vec![
            target("Desert Island Discs", 1, None, "Dolly Parton", None),
            target("The News Quiz", 20, Some(3), "Episode 3", None),
        ];
        let hit = match_release(&targets, &parse_radio("The News Quiz S20E03")).unwrap();
        assert_eq!(hit.episode.show_title, "The News Quiz");
        assert!(match_release(&targets, &parse_radio("Desert Island Discs")).is_none());
    }

    #[test]
    fn bbc_programme_ids_decide() {
        let name = "The_Unbelievable_Truth_-_Series_20_Episode_3_b09zv5m0_original.m4a";
        let p = parse_radio(name);
        // The same id matches even when the show is named differently.
        assert!(
            bbc("Something Else", 1, Some(1), "b09zv5m0")
                .check_release(&p)
                .is_ok()
        );
        assert!(
            bbc("The Unbelievable Truth", 20, Some(3), "B09ZV5M0")
                .check_release(&p)
                .is_ok()
        );
        // Another id is refused even when the show and numbers fit.
        assert!(
            bbc("The Unbelievable Truth", 20, Some(3), "b09zv5m1")
                .check_release(&p)
                .unwrap_err()
                .contains("BBC programme b09zv5m0")
        );
        // Without an id in the name, BBC episodes match by show and numbers as before.
        let t = bbc("The Unbelievable Truth", 20, Some(3), "b09zv5m0");
        assert!(
            t.check_release(&parse_radio("The Unbelievable Truth S20E03"))
                .is_ok()
        );
        assert!(
            t.check_release(&parse_radio("The Unbelievable Truth S20E04"))
                .is_err()
        );
        // Episodes of other providers ignore the id and match by show and numbers.
        let t = target("The Unbelievable Truth", 20, Some(3), "Episode 3", None);
        assert!(t.check_release(&p).is_ok());
        assert!(
            target("Other Show", 20, Some(3), "Episode 3", None)
                .check_release(&p)
                .is_err()
        );
        // A BBC episode whose external id is not a programme id falls back to the name.
        let t = bbc("The Unbelievable Truth", 20, Some(3), "not-a-pid");
        assert_eq!(t.bbc_pid(), None);
        assert!(t.check_release(&p).is_ok());

        // RSS sync: the id picks the right episode among several of the same show.
        let targets = vec![
            bbc("The Unbelievable Truth", 20, Some(2), "b09zv5l9"),
            bbc("The Unbelievable Truth", 20, Some(3), "b09zv5m0"),
        ];
        let hit = match_release(&targets, &parse_radio("Front_Row_b09zv5m0_original.m4a")).unwrap();
        assert_eq!(hit.episode.number, Some(3));
        assert!(match_release(&targets, &parse_radio("Front_Row_b09zv5m9_original.m4a")).is_none());
    }
}
