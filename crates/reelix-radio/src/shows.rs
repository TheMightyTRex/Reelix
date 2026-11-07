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
use crate::repo::{self, batch::Batches, shows::NewShow};
use crate::status::{episode_status, monitored_by_follow};
use crate::{RadioService, entities, keys};

/// Newest episodes shown by a preview.
const PREVIEW_EPISODES: usize = 10;

impl RadioService {
    // ---------------- shows ----------------

    pub async fn list_shows(&self, q: &RadioShowListQuery) -> Result<Paged<RadioShowDto>> {
        let (mut items, total, page, page_size) = repo::shows::page(&self.db, q).await?;
        self.fill_abs_urls(&mut items).await?;
        Ok(Paged {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn get_show(&self, id: i64) -> Result<RadioShowDto> {
        let mut show = repo::shows::require(&self.db, id).await?;
        self.fill_abs_urls(std::slice::from_mut(&mut show)).await?;
        Ok(show)
    }

    /// Remembers the address of the Audiobookshelf server holding the podcast library (set by
    /// the library sync; `None` forgets it), from which shows get their `abs_web_url`.
    pub async fn set_abs_web_url(&self, base: Option<&str>) -> Result<()> {
        let base = base
            .map(|b| b.trim().trim_end_matches('/').to_owned())
            .filter(|b| !b.is_empty());
        if self.abs_web_base().await? != base {
            self.db.settings().set(keys::ABS_WEB_URL, &base).await?;
        }
        Ok(())
    }

    /// The Audiobookshelf server address saved by the last library sync.
    pub async fn abs_web_base(&self) -> Result<Option<String>> {
        Ok(self
            .db
            .settings()
            .get::<Option<String>>(keys::ABS_WEB_URL)
            .await?
            .flatten())
    }

    /// Sets `abs_web_url` (`{server}/item/{abs_item_id}`) on shows mapped to an item.
    async fn fill_abs_urls(&self, shows: &mut [RadioShowDto]) -> Result<()> {
        if shows.iter().all(|s| s.abs_item_id.is_none()) {
            return Ok(());
        }
        let Some(base) = self.abs_web_base().await? else {
            return Ok(());
        };
        for s in shows {
            s.abs_web_url = s.abs_item_id.as_deref().map(|item| abs_item_url(&base, item));
        }
        Ok(())
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
        // The id asked for, when it is not the one the show is saved under (an Apple id
        // handed to `rss`, a BBC series inside a brand): remembered as an alias.
        let mut asked: Option<(String, String)> = None;
        let (provider, data) = match (
            req.link.as_deref().map(str::trim).filter(|l| !l.is_empty()),
            &req.external_id,
        ) {
            (Some(link), _) => self.resolve_link(link).await?,
            (None, Some(external_id)) => {
                let id = req.provider.as_deref().unwrap_or(crate::providers::rss::ID);
                asked = Some((id.to_owned(), external_id.trim().to_owned()));
                let p = self.enabled_provider(id).await?;
                let fetch = ShowFetch {
                    max_lookups: settings.max_lookups_per_refresh,
                    quick: true,
                    ..ShowFetch::default()
                };
                let data = p.show(external_id.trim(), &fetch).await?;
                (self.owner(p, &data)?, data)
            }
            (None, None) => {
                return Err(RadioError::Validation(
                    "give a link to the show, or a provider and the show's id".into(),
                ));
            }
        };
        let asked =
            asked.filter(|(p, e)| (p.as_str(), e.as_str()) != (provider.id(), data.external_id.as_str()));
        if let Some(id) = repo::shows::by_external(&self.db, provider.id(), &data.external_id).await? {
            if let Some((p, e)) = &asked {
                repo::shows::add_alias(&self.db, p, e, id).await?;
            }
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
        if let Some((p, e)) = &asked {
            repo::shows::add_alias(&self.db, p, e, id).await?;
        }
        self.apply_show_data(id, &data, true).await?;
        self.download_artwork(id).await;
        self.emit(entities::SHOW, "created", id);
        if !data.complete {
            // A quick first read (e.g. the BBC's newest series only): read the rest now.
            self.queue_refresh(vec![id]).await?;
        }
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

    /// Monitors or unmonitors one episode by hand and re-derives its status.
    pub async fn set_episode_monitored(&self, id: i64, monitored: bool) -> Result<RadioEpisodeDto> {
        let e = repo::episodes::require(&self.db, id).await?;
        self.apply_monitored(e.show_id, &[id], monitored).await?;
        self.get_episode(id).await
    }

    /// Monitors or unmonitors every episode of a series by hand and re-derives their statuses.
    pub async fn set_series_monitored(
        &self,
        series_id: i64,
        monitored: bool,
    ) -> Result<Vec<RadioEpisodeDto>> {
        let series = repo::series::require(&self.db, series_id).await?;
        let ids = repo::episodes::ids_for_series(&self.db, series_id).await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.apply_monitored(series.show_id, &ids, monitored).await?;
        repo::episodes::load(&self.db, &ids).await
    }

    /// Saves `monitored` on episodes `ids` of show `show_id` and their new statuses; emits
    /// `entity.changed` for each changed episode and, if any changed, the show.
    async fn apply_monitored(&self, show_id: i64, ids: &[i64], monitored: bool) -> Result<()> {
        let show = repo::shows::require(&self.db, show_id).await?;
        let now = self.now();
        let mut any = false;
        for e in repo::episodes::load(&self.db, ids).await? {
            let flag_changed = e.monitored != monitored;
            if flag_changed {
                repo::episodes::set_monitored(&self.db, e.id, monitored).await?;
            }
            let status = episode_status(
                e.file_id.is_some(),
                show.monitored,
                monitored,
                e.first_broadcast,
                now,
            );
            let status_changed = repo::episodes::set_status(self.db.pool(), e.id, status).await?;
            if flag_changed || status_changed {
                any = true;
                self.emit(entities::EPISODE, "updated", e.id);
            }
        }
        if any {
            self.emit(entities::SHOW, "updated", show_id);
        }
        Ok(())
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
        let mut batch = Batches::new(&self.db);
        for e in repo::episodes::for_show(&self.db, show_id).await? {
            let s = episode_status(
                e.file_id.is_some(),
                show.monitored,
                e.monitored,
                e.first_broadcast,
                now,
            );
            if repo::episodes::set_status(batch.conn().await?, e.id, s).await? {
                changed.push(e.id);
            }
            batch.wrote().await?;
        }
        batch.commit().await?;
        for id in &changed {
            self.emit(entities::EPISODE, "updated", *id);
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
            None => self.providers.for_search(&settings.enabled_providers, &prefs),
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
        // Directories find the same feed: keep the first hit per show and per feed.
        let mut seen = std::collections::HashSet::new();
        hits.retain(|h| {
            let feed = h
                .feed_url
                .as_deref()
                .map(|f| f.trim_end_matches('/').to_ascii_lowercase());
            let fresh = seen.insert((h.provider.clone(), h.external_id.clone()));
            fresh && feed.is_none_or(|f| seen.insert((String::new(), f)))
        });
        self.mark_in_library(hits).await
    }

    /// The show behind a pasted link, with its newest episodes. Saves nothing.
    pub async fn preview(&self, link: &str) -> Result<RadioShowPreviewDto> {
        let (p, data) = self.resolve_link(link.trim()).await?;
        self.preview_of(p.id(), &data).await
    }

    /// The preview of `data` (owned by provider `owner`): the show (marked when followed)
    /// and its newest episodes.
    pub(crate) async fn preview_of(&self, owner: &str, data: &ShowData) -> Result<RadioShowPreviewDto> {
        let mut hit = self.mark_in_library(vec![data.hit(owner)]).await?;
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
                Ok(Some(data)) => return Ok((self.owner(p, &data)?, data)),
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

    /// The provider that owns `data` ([`ShowData::provider`], e.g. a directory handing a
    /// feed to `rss`), else `asked`.
    pub(crate) fn owner(
        &self,
        asked: std::sync::Arc<dyn RadioProvider>,
        data: &ShowData,
    ) -> Result<std::sync::Arc<dyn RadioProvider>> {
        match data.provider.as_deref() {
            Some(id) if id != asked.id() => self
                .providers
                .get(id)
                .ok_or_else(|| RadioError::Validation(format!("unknown radio source {id}"))),
            _ => Ok(asked),
        }
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

    /// Sets `show_id` on hits of followed shows: same provider and id, an id the show was
    /// added under (alias), or the same feed.
    pub(crate) async fn mark_in_library(
        &self,
        mut hits: Vec<RadioShowHitDto>,
    ) -> Result<Vec<RadioShowHitDto>> {
        let known = repo::shows::known(&self.db).await?;
        for h in &mut hits {
            h.show_id = known.find(&h.provider, &h.external_id, h.feed_url.as_deref());
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

/// `{server}/item/{item}`: a podcast's page in the Audiobookshelf web app.
fn abs_item_url(base: &str, item: &str) -> String {
    format!("{}/item/{}", base.trim_end_matches('/'), item.trim())
}

#[cfg(test)]
mod tests {
    #[test]
    fn abs_item_urls() {
        assert_eq!(
            super::abs_item_url("http://abs:13378/", "li_1"),
            "http://abs:13378/item/li_1"
        );
        assert_eq!(
            super::abs_item_url("https://x.example/audiobookshelf", "li_2"),
            "https://x.example/audiobookshelf/item/li_2"
        );
    }
}
