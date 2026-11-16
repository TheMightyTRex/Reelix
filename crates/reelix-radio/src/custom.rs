//! Radio shows added with the owner's own details (RFX-90): a podcast item of the
//! Audiobookshelf library that no source lists, with a typed title, year, overview, station
//! and picture. The show's provider is [`CUSTOM_PROVIDER`], its external id and `abs_item_id`
//! the item, and `custom = 1` (migration 0020). Its series and episodes come from the item's
//! library files ([`RadioService::sync_custom_show`]): when it is added, at each refresh and
//! after each library sync. Each file is one episode (`abs:{abs_episode_id}`), in the series
//! of its Audiobookshelf season (series 1 without one), linked to that file.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use reelix_api_types::custom::{CustomDetails, CustomDetailsChange};
use reelix_api_types::radio::{RadioEpisodeFileDto, RadioFollow, RadioShowDto};
use reelix_core::ContentKind;
use reelix_parser::QualityFamily;

use crate::convert::{clean_title, sort_title};
use crate::error::{RadioError, Result};
use crate::provider::{EpisodeData, SeriesData, ShowData, StationData};
use crate::repo::shows::NewShow;
use crate::{RadioService, entities, repo};

/// `radio_shows.provider` of custom shows (no provider is registered under it).
pub const CUSTOM_PROVIDER: &str = "custom";

/// 1 January of `year` (a custom show's first broadcast is only a year).
pub(crate) fn year_start(year: Option<i32>) -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(year?, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc())
}

/// A station typed for a custom show (`custom` stations are keyed by the clean name).
fn station_of(name: Option<&str>) -> Option<StationData> {
    let name = name.map(str::trim).filter(|n| !n.is_empty())?;
    Some(StationData {
        external_id: clean_title(name),
        name: name.to_owned(),
        logo_url: None,
    })
}

/// External id of the episode made from a library file.
pub(crate) fn file_episode_id(f: &RadioEpisodeFileDto) -> String {
    format!("abs:{}", f.abs_episode_id)
}

/// The show as its library files describe it: the typed details of `show`, a series per
/// Audiobookshelf season and an episode per file.
pub(crate) fn custom_show_data(show: &RadioShowDto, files: &[&RadioEpisodeFileDto]) -> ShowData {
    let season_of = |f: &RadioEpisodeFileDto| f.season.unwrap_or(1);
    let seasons: BTreeSet<u32> = files.iter().map(|f| season_of(f)).collect();
    let series = seasons
        .into_iter()
        .map(|n| SeriesData {
            external_id: format!("season:{n}"),
            number: Some(n),
            ..SeriesData::default()
        })
        .collect();
    let episodes = files
        .iter()
        .map(|f| {
            let name = f
                .path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_stem())
                .map(|s| s.to_string_lossy().into_owned());
            EpisodeData {
                external_id: file_episode_id(f),
                series_external_id: format!("season:{}", season_of(f)),
                number: f.episode,
                title: f
                    .title
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .or(name)
                    .unwrap_or_else(|| f.abs_episode_id.clone()),
                synopsis: None,
                first_broadcast: f.published_at,
                duration_secs: f.duration_secs,
                enclosure_url: f.enclosure_url.clone(),
                guid: f.guid.clone(),
            }
        })
        .collect();
    ShowData {
        external_id: show.external_id.clone(),
        title: show.title.clone(),
        synopsis: show.synopsis.clone(),
        first_broadcast: show.first_broadcast,
        series,
        episodes,
        complete: true,
        ..ShowData::default()
    }
}

impl RadioService {
    /// Adds a show with typed details for an Audiobookshelf podcast item no show uses; its
    /// episodes are made from the item's files at once.
    pub async fn add_custom_show(
        &self,
        d: &CustomDetails,
        abs_item_id: &str,
        station: Option<&str>,
        quality_profile_id: Option<i64>,
        monitored: bool,
    ) -> Result<RadioShowDto> {
        let item = abs_item_id.trim();
        if item.is_empty() {
            return Err(RadioError::Validation(
                "choose a show of the podcast library".into(),
            ));
        }
        let files = repo::files::all(&self.db).await?;
        if !files.iter().any(|f| f.abs_item_id == item) {
            return Err(RadioError::Validation(format!(
                "{item} is not a podcast of the Audiobookshelf library (as of its last sync)"
            )));
        }
        if let Some(s) = repo::shows::load(&self.db, None)
            .await?
            .into_iter()
            .find(|s| s.abs_item_id.as_deref() == Some(item))
        {
            return Err(RadioError::Conflict(format!(
                "that podcast already belongs to {} (radio show {})",
                s.title, s.id
            )));
        }
        let profile = match quality_profile_id {
            Some(p) => {
                self.check_quality_profile(p).await?;
                Some(p)
            }
            None => self
                .db
                .quality_profiles()
                .list_family(QualityFamily::of_content(ContentKind::Radio))
                .await?
                .first()
                .map(|p| p.id),
        };
        let station_id = match station_of(station) {
            Some(st) => Some(repo::stations::upsert(&self.db, CUSTOM_PROVIDER, &st).await?),
            None => None,
        };
        let data = ShowData {
            external_id: item.to_owned(),
            title: d.title.clone(),
            synopsis: d.overview.clone(),
            first_broadcast: year_start(d.year),
            complete: true,
            ..ShowData::default()
        };
        let id = repo::shows::insert(
            &self.db,
            &data,
            &NewShow {
                provider: CUSTOM_PROVIDER,
                follow: RadioFollow::None,
                monitored,
                root_folder_id: None,
                quality_profile_id: profile,
                tags: &[],
                station_id,
                now: self.now().timestamp_millis(),
            },
        )
        .await?;
        sqlx::query("UPDATE radio_shows SET abs_item_id = ?, custom = 1 WHERE id = ?")
            .bind(item)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        self.sync_custom_show(id).await?;
        self.emit(entities::SHOW, "created", id);
        tracing::info!(id, title = %d.title, "custom radio show added");
        self.get_show(id).await
    }

    async fn custom_show(&self, id: i64) -> Result<RadioShowDto> {
        let s = repo::shows::require(&self.db, id).await?;
        if !s.custom {
            return Err(RadioError::Validation(format!(
                "radio show {id} comes from a source; only shows added with their own details can be changed here"
            )));
        }
        Ok(s)
    }

    /// Changes the typed details of a custom show (`year` is the first broadcast's year;
    /// `station` `Some(None)` removes the station).
    pub async fn change_custom_show(
        &self,
        id: i64,
        c: &CustomDetailsChange,
        station: Option<Option<&str>>,
    ) -> Result<RadioShowDto> {
        let mut s = self.custom_show(id).await?;
        if let Some(t) = &c.title {
            s.title = t.clone();
        }
        if let Some(o) = &c.overview {
            s.synopsis = o.clone();
        }
        if let Some(y) = c.year {
            s.first_broadcast = year_start(y);
        }
        if let Some(st) = station {
            s.station_id = match station_of(st) {
                Some(st) => Some(repo::stations::upsert(&self.db, CUSTOM_PROVIDER, &st).await?),
                None => None,
            };
        }
        sqlx::query(
            "UPDATE radio_shows SET title = ?, sort_title = ?, clean_title = ?, synopsis = ?, first_broadcast = ?, \
                station_id = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&s.title)
        .bind(sort_title(&s.title))
        .bind(clean_title(&s.title))
        .bind(&s.synopsis)
        .bind(s.first_broadcast.map(|t| t.timestamp_millis()))
        .bind(s.station_id)
        .bind(reelix_db::now_ms())
        .bind(id)
        .execute(self.db.pool())
        .await?;
        self.emit(entities::SHOW, "updated", id);
        self.get_show(id).await
    }

    /// Records the picture saved for a custom show (relative to the data folder, as downloaded
    /// artwork is; `None` removes it).
    pub async fn set_custom_show_artwork(&self, id: i64, rel: Option<&str>) -> Result<()> {
        self.custom_show(id).await?;
        self.set_show_artwork_path(id, rel).await
    }

    /// Series and episodes of a custom show from its library files, each episode linked to its
    /// file (a hand-set link or "Not this file" is left alone). Returns the new episodes.
    pub async fn sync_custom_show(&self, id: i64) -> Result<u32> {
        let show = repo::shows::require(&self.db, id).await?;
        let Some(item) = show.abs_item_id.clone().filter(|_| show.custom) else {
            return Ok(0);
        };
        let all = repo::files::all(&self.db).await?;
        let files: Vec<&RadioEpisodeFileDto> = all.iter().filter(|f| f.abs_item_id == item).collect();
        let added = self
            .merge(id, &custom_show_data(&show, &files), &HashSet::new(), false)
            .await?;
        let episodes: HashMap<String, (i64, Option<i64>, bool)> = repo::episodes::for_show(&self.db, id)
            .await?
            .into_iter()
            .map(|e| (e.external_id, (e.id, e.file_id, e.manual_match)))
            .collect();
        let mut linked = false;
        for f in &files {
            let Some((eid, file, manual)) = episodes.get(&file_episode_id(f)) else {
                continue;
            };
            if *manual || *file == Some(f.id) {
                continue;
            }
            // The file may be linked to an episode of another show by an automatic match.
            if let Some(other) = repo::episodes::for_file(&self.db, f.id).await?
                && other.id != *eid
            {
                if other.manual_match {
                    continue;
                }
                repo::episodes::set_file(&self.db, other.id, None, false).await?;
                self.recompute_statuses(other.show_id).await?;
            }
            repo::episodes::set_file(&self.db, *eid, Some(f.id), false).await?;
            linked = true;
        }
        if linked {
            self.recompute_statuses(id).await?;
        }
        Ok(added)
    }

    /// [`Self::sync_custom_show`] for every custom show (after a library sync).
    pub(crate) async fn sync_custom_shows(&self) -> Result<()> {
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM radio_shows WHERE custom = 1 ORDER BY id")
            .fetch_all(self.db.pool())
            .await?;
        for id in ids {
            if let Err(e) = self.sync_custom_show(id).await {
                tracing::warn!(show_id = id, error = %e, "custom radio show not synced");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: i64, ep: &str, season: Option<u32>, title: Option<&str>) -> RadioEpisodeFileDto {
        RadioEpisodeFileDto {
            id,
            abs_item_id: "li_1".into(),
            abs_episode_id: ep.into(),
            item_title: Some("Garden Hour".into()),
            title: title.map(str::to_owned),
            season,
            episode: Some(id as u32),
            guid: None,
            enclosure_url: None,
            published_at: None,
            duration_secs: Some(60),
            path: Some(format!("/podcasts/Garden Hour/{ep}.mp3")),
            size: Some(1),
            seen_at: Utc::now(),
        }
    }

    #[test]
    fn data_from_files() {
        let show = RadioShowDto {
            id: 1,
            provider: CUSTOM_PROVIDER.into(),
            external_id: "li_1".into(),
            title: "Garden Hour".into(),
            sort_title: "Garden Hour".into(),
            synopsis: Some("Weekly".into()),
            author: None,
            web_url: None,
            first_broadcast: year_start(Some(1998)),
            station_id: None,
            station_name: None,
            artwork_url: None,
            has_artwork: false,
            feed_url: None,
            follow: RadioFollow::None,
            followed_at: None,
            monitored: false,
            tags: Vec::new(),
            root_folder_id: None,
            abs_item_id: Some("li_1".into()),
            quality_profile_id: None,
            refreshed_at: None,
            added_at: Utc::now(),
            statistics: Default::default(),
            abs_web_url: None,
            keep_episodes: None,
            custom: true,
        };
        let a = file(1, "ep_a", None, Some("Roses"));
        let b = file(2, "ep_b", Some(2), None);
        let d = custom_show_data(&show, &[&a, &b]);
        assert_eq!(d.title, "Garden Hour");
        assert_eq!(d.synopsis.as_deref(), Some("Weekly"));
        assert!(d.complete);
        let series: Vec<&str> = d.series.iter().map(|s| s.external_id.as_str()).collect();
        assert_eq!(series, ["season:1", "season:2"]);
        assert_eq!(d.episodes[0].external_id, "abs:ep_a");
        assert_eq!(d.episodes[0].title, "Roses");
        assert_eq!(d.episodes[1].series_external_id, "season:2");
        assert_eq!(d.episodes[1].title, "ep_b");
        assert_eq!(
            year_start(Some(1998)).map(|t| t.timestamp_millis()),
            Some(883_612_800_000)
        );
        assert!(station_of(Some("  ")).is_none());
        assert_eq!(station_of(Some("Radio Leeds")).unwrap().name, "Radio Leeds");
    }
}
