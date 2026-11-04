//! Shows, episodes and stations: add, edit, delete, list, read, search and preview.

use reelix_api_types::library::Paged;
use reelix_api_types::radio::{
    AddRadioShowRequest, RadioEpisodeDto, RadioPreviewEpisodeDto, RadioProviderDto, RadioShowDto,
    RadioShowEdit, RadioShowEpisodesDto, RadioShowHitDto, RadioShowListQuery, RadioShowPreviewDto,
    RadioStationDto,
};
use reelix_core::ContentKind;
use reelix_parser::QualityFamily;

use crate::error::{RadioError, Result};
use crate::provider::{ProviderError, RadioProvider, SearchPrefs, ShowData, ShowFetch};
use crate::repo::{self, shows::NewShow};
use crate::status::{episode_status, monitored_by_follow};
use crate::{RadioService, entities};

/// Newest episodes shown by a preview.
const PREVIEW_EPISODES: usize = 10;

impl RadioService {
    // ---------------- shows ----------------

    pub async fn list_shows(&self, q: &RadioShowListQuery) -> Result<Paged<RadioShowDto>> {
        let (items, total, page, page_size) = repo::shows::page(&self.db, q).await?;
        Ok(Paged {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn get_show(&self, id: i64) -> Result<RadioShowDto> {
        repo::shows::require(&self.db, id).await
    }

    /// A show's series and episodes.
    pub async fn show_episodes(&self, id: i64) -> Result<RadioShowEpisodesDto> {
        repo::shows::require(&self.db, id).await?;
        Ok(RadioShowEpisodesDto {
            series: repo::series::for_show(&self.db, id).await?,
            episodes: repo::episodes::for_show(&self.db, id).await?,
        })
    }

    /// One episode with its matched file.
    pub async fn get_episode(&self, id: i64) -> Result<RadioEpisodeDto> {
        let mut e = repo::episodes::require(&self.db, id).await?;
        if let Some(f) = e.file_id {
            e.file = repo::files::get(&self.db, f).await?;
        }
        Ok(e)
    }

    pub async fn stations(&self) -> Result<Vec<RadioStationDto>> {
        repo::stations::list(&self.db).await
    }

    pub async fn provider_list(&self) -> Result<Vec<RadioProviderDto>> {
        let s = self.settings().await?;
        Ok(self.providers.dtos(&s.enabled_providers))
    }

    /// Adds a show from a search hit (`provider` + `external_id`) or a pasted `link`, with its
    /// series and episodes, statuses from the follow choice.
    pub async fn add_show(&self, req: AddRadioShowRequest) -> Result<RadioShowDto> {
        let settings = self.settings().await?;
        let (provider, data) = match (
            req.link.as_deref().map(str::trim).filter(|l| !l.is_empty()),
            &req.external_id,
        ) {
            (Some(link), _) => self.resolve_link(link).await?,
            (None, Some(external_id)) => {
                let id = req.provider.as_deref().unwrap_or(crate::providers::rss::ID);
                let p = self.enabled_provider(id).await?;
                let fetch = ShowFetch {
                    max_lookups: settings.max_lookups_per_refresh,
                    ..ShowFetch::default()
                };
                let data = p.show(external_id.trim(), &fetch).await?;
                (p, data)
            }
            (None, None) => {
                return Err(RadioError::Validation(
                    "give a link to the show, or a provider and the show's id".into(),
                ));
            }
        };
        if let Some(id) = repo::shows::by_external(&self.db, provider.id(), &data.external_id).await? {
            return Err(RadioError::Conflict(format!(
                "{} is already in the library (show {id})",
                data.title
            )));
        }
        if let Some(r) = req.root_folder_id {
            self.radio_root(r).await?;
        }
        if let Some(p) = req.quality_profile_id {
            self.check_quality_profile(p).await?;
        }
        let station_id = match &data.station {
            Some(st) => Some(repo::stations::upsert(&self.db, provider.id(), st).await?),
            None => None,
        };
        let follow = req.follow.unwrap_or(settings.default_follow);
        let id = repo::shows::insert(
            &self.db,
            &data,
            &NewShow {
                provider: provider.id(),
                follow,
                monitored: req.monitored,
                root_folder_id: req.root_folder_id,
                quality_profile_id: req.quality_profile_id,
                tags: &req.tags,
                station_id,
                now: self.now().timestamp_millis(),
            },
        )
        .await?;
        self.apply_show_data(id, &data, true).await?;
        self.emit(entities::SHOW, "created", id);
        self.get_show(id).await
    }

    /// Edits a show's follow choice, monitoring, tags, root folder, quality profile or
    /// Audiobookshelf item. A new follow choice is re-applied to every episode.
    pub async fn update_show(&self, id: i64, edit: RadioShowEdit) -> Result<RadioShowDto> {
        let mut show = repo::shows::require(&self.db, id).await?;
        let follow_changed = edit.follow.is_some_and(|f| f != show.follow);
        let monitored_changed = edit.monitored.is_some_and(|m| m != show.monitored);
        if let Some(f) = edit.follow
            && follow_changed
        {
            show.follow = f;
            show.followed_at = Some(self.now());
        }
        if let Some(m) = edit.monitored {
            show.monitored = m;
        }
        if let Some(t) = edit.tags {
            show.tags = t;
        }
        if let Some(r) = edit.root_folder_id {
            if let Some(r) = r {
                self.radio_root(r).await?;
            }
            show.root_folder_id = r;
        }
        if let Some(p) = edit.quality_profile_id {
            if let Some(p) = p {
                self.check_quality_profile(p).await?;
            }
            show.quality_profile_id = p;
        }
        if let Some(a) = edit.abs_item_id {
            show.abs_item_id = a.map(|a| a.trim().to_owned()).filter(|a| !a.is_empty());
        }
        repo::shows::save_settings(&self.db, &show).await?;
        if follow_changed {
            for e in repo::episodes::for_show(&self.db, id).await? {
                let m = monitored_by_follow(show.follow, show.followed_at, e.first_broadcast, false);
                repo::episodes::set_monitored(&self.db, e.id, m).await?;
            }
        }
        if follow_changed || monitored_changed {
            self.recompute_statuses(id).await?;
        }
        self.emit(entities::SHOW, "updated", id);
        self.get_show(id).await
    }

    /// Deletes a show with its series and episodes (library files are not touched).
    pub async fn delete_show(&self, id: i64) -> Result<()> {
        if !repo::shows::delete(&self.db, id).await? {
            return Err(RadioError::NotFound(format!("radio show {id} not found")));
        }
        self.emit(entities::SHOW, "deleted", id);
        Ok(())
    }

    /// Re-derives every episode status of a show (after a follow change, a refresh, a match or
    /// a file that disappeared). Returns the ids of episodes whose status changed (each also
    /// gets an `entity.changed`).
    pub async fn recompute_statuses(&self, show_id: i64) -> Result<Vec<i64>> {
        let show = repo::shows::require(&self.db, show_id).await?;
        let now = self.now();
        let mut changed = Vec::new();
        for e in repo::episodes::for_show(&self.db, show_id).await? {
            let s = episode_status(
                e.file_id.is_some(),
                show.monitored,
                e.monitored,
                e.first_broadcast,
                now,
            );
            if repo::episodes::set_status(&self.db, e.id, s).await? {
                changed.push(e.id);
                self.emit(entities::EPISODE, "updated", e.id);
            }
        }
        Ok(changed)
    }

    // ---------------- search / preview ----------------

    /// Searches by name: one provider, or every enabled provider that can search. A pasted
    /// link is previewed instead. Hits already in the library carry `show_id`.
    pub async fn search(&self, q: &str, provider: Option<&str>) -> Result<Vec<RadioShowHitDto>> {
        let q = q.trim();
        if q.is_empty() {
            return Err(RadioError::Validation(
                "type a show's name or paste a link".into(),
            ));
        }
        if q.starts_with("http://") || q.starts_with("https://") {
            let (p, data) = self.resolve_link(q).await?;
            return self.mark_in_library(vec![data.hit(p.id())]).await;
        }
        let settings = self.settings().await?;
        let prefs = SearchPrefs {
            country: settings.country.clone(),
            language: settings.language.clone(),
        };
        let candidates: Vec<_> = match provider {
            Some(id) => vec![self.enabled_provider(id).await?],
            None => self
                .providers
                .enabled(&settings.enabled_providers)
                .into_iter()
                .filter(|p| p.supports_search())
                .collect(),
        };
        if candidates.is_empty() {
            return Err(RadioError::Validation(
                "no enabled source can search by name yet; paste a link to the show or its podcast feed"
                    .into(),
            ));
        }
        let mut hits = Vec::new();
        let mut last_err = None;
        for p in &candidates {
            match p.search(q, &prefs).await {
                Ok(h) => hits.extend(h),
                Err(e) if candidates.len() == 1 => return Err(e.into()),
                Err(e) => {
                    tracing::warn!(provider = p.id(), error = %e, "radio search failed");
                    last_err = Some(e);
                }
            }
        }
        if hits.is_empty()
            && let Some(e) = last_err
        {
            return Err(e.into());
        }
        self.mark_in_library(hits).await
    }

    /// The show behind a pasted link, with its newest episodes. Saves nothing.
    pub async fn preview(&self, link: &str) -> Result<RadioShowPreviewDto> {
        let (p, data) = self.resolve_link(link.trim()).await?;
        let mut hit = self.mark_in_library(vec![data.hit(p.id())]).await?;
        let numbers: std::collections::HashMap<&str, Option<u32>> = data
            .series
            .iter()
            .map(|s| (s.external_id.as_str(), s.number))
            .collect();
        let mut eps: Vec<_> = data.episodes.iter().collect();
        eps.sort_by_key(|e| std::cmp::Reverse(e.first_broadcast));
        Ok(RadioShowPreviewDto {
            show: hit.remove(0),
            first_broadcast: data.first_broadcast,
            series_count: data.series.len() as u32,
            episode_count: data.episodes.len() as u32,
            latest_episodes: eps
                .into_iter()
                .take(PREVIEW_EPISODES)
                .map(|e| RadioPreviewEpisodeDto {
                    title: e.title.clone(),
                    series_number: numbers.get(e.series_external_id.as_str()).copied().flatten(),
                    number: e.number,
                    first_broadcast: e.first_broadcast,
                    duration_secs: e.duration_secs,
                })
                .collect(),
        })
    }

    /// Asks each enabled provider, in registry order, whether `link` is one of its shows.
    pub(crate) async fn resolve_link(
        &self,
        link: &str,
    ) -> Result<(std::sync::Arc<dyn RadioProvider>, ShowData)> {
        let settings = self.settings().await?;
        let mut last_err: Option<ProviderError> = None;
        for p in self.providers.enabled(&settings.enabled_providers) {
            match p.preview(link).await {
                Ok(Some(data)) => return Ok((p, data)),
                Ok(None) => {}
                Err(e) => last_err = Some(e),
            }
        }
        Err(match last_err {
            Some(e) => e.into(),
            None => RadioError::Validation(
                "no source recognises this link; paste the show's page or its podcast feed address".into(),
            ),
        })
    }

    pub(crate) async fn enabled_provider(&self, id: &str) -> Result<std::sync::Arc<dyn RadioProvider>> {
        let settings = self.settings().await?;
        self.providers
            .enabled(&settings.enabled_providers)
            .into_iter()
            .find(|p| p.id() == id)
            .ok_or_else(|| match self.providers.get(id) {
                Some(_) => {
                    RadioError::Validation(format!("the source {id} is turned off in the radio settings"))
                }
                None => RadioError::Validation(format!("unknown radio source {id}")),
            })
    }

    async fn mark_in_library(&self, mut hits: Vec<RadioShowHitDto>) -> Result<Vec<RadioShowHitDto>> {
        let known = repo::shows::external_ids(&self.db).await?;
        for h in &mut hits {
            h.show_id = known
                .iter()
                .find(|(p, e, _)| *p == h.provider && *e == h.external_id)
                .map(|k| k.2);
        }
        Ok(hits)
    }

    // ---------------- validation ----------------

    /// Radio episodes rank with the audiobook qualities ([`QualityFamily::of_content`]).
    pub(crate) async fn check_quality_profile(&self, id: i64) -> Result<()> {
        let family = QualityFamily::of_content(ContentKind::Radio);
        match self.db.quality_profiles().get_any(id).await? {
            Some(p) if p.family == family => Ok(()),
            Some(p) => Err(RadioError::Validation(format!(
                "quality profile {id} ({}) is not an audiobook profile",
                p.name
            ))),
            None => Err(RadioError::Validation(format!(
                "quality profile {id} does not exist"
            ))),
        }
    }
}
