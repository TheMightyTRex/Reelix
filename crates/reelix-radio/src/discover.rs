//! Discover: the providers' curated lists (charts, trending, categories), previews of one
//! show by id, and which shows are already followed. Used by the request portal (any
//! signed-in user may browse; following needs MANAGE_LIBRARY or an approved request).

use std::collections::{HashMap, HashSet};

use reelix_api_types::radio::{RadioShowHitDto, RadioShowPreviewDto};
use reelix_api_types::requests::{RadioDiscoverDto, RadioDiscoverListDto};

use crate::error::{RadioError, Result};
use crate::provider::{SearchPrefs, ShowFetch};
use crate::{RadioService, repo};

impl RadioService {
    async fn prefs(&self) -> Result<(Vec<String>, SearchPrefs)> {
        let s = self.settings().await?;
        Ok((
            s.enabled_providers,
            SearchPrefs {
                country: s.country,
                language: s.language,
            },
        ))
    }

    /// The Discover lists of the enabled sources for the settings' country and language
    /// (the BBC only for `GB` or no country). Row ids are `{provider}-{list}`.
    pub async fn discover_lists(&self) -> Result<RadioDiscoverDto> {
        let (enabled, prefs) = self.prefs().await?;
        let mut out = RadioDiscoverDto::default();
        for p in self.providers.for_discover(&enabled, &prefs) {
            for l in p.discover_lists() {
                let dto = RadioDiscoverListDto {
                    id: format!("{}-{}", p.id(), l.id),
                    name: l.name,
                    provider: p.id().to_owned(),
                    provider_name: p.name().to_owned(),
                };
                if l.category {
                    out.categories.push(dto);
                } else {
                    out.rows.push(dto);
                }
            }
        }
        Ok(out)
    }

    /// The shows of one Discover list (`row` from [`discover_lists`](Self::discover_lists)),
    /// followed ones carrying `show_id`.
    pub async fn discover(&self, row: &str) -> Result<Vec<RadioShowHitDto>> {
        let unknown = || RadioError::NotFound(format!("unknown radio discover row: {row}"));
        let (provider, list) = row.split_once('-').ok_or_else(unknown)?;
        let (enabled, prefs) = self.prefs().await?;
        let p = self
            .providers
            .for_discover(&enabled, &prefs)
            .into_iter()
            .find(|p| p.id() == provider)
            .filter(|p| p.discover_lists().iter().any(|l| l.id == list))
            .ok_or_else(unknown)?;
        let mut hits = p.discover(list, &prefs).await?;
        let mut seen = HashSet::new();
        hits.retain(|h| seen.insert((h.provider.clone(), h.external_id.clone())));
        self.mark_in_library(hits).await
    }

    /// One show by provider and id with its newest episodes, as Discover shows it (the
    /// provider's quick read; nothing is saved). When it turns out to be a followed show under
    /// another id (an Apple id whose feed is followed), that id is remembered as an alias so
    /// lists mark it from then on.
    pub async fn preview_show(&self, provider: &str, external_id: &str) -> Result<RadioShowPreviewDto> {
        let external_id = external_id.trim();
        if external_id.is_empty() {
            return Err(RadioError::Validation("the show's id is missing".into()));
        }
        let p = self.enabled_provider(provider).await?;
        let fetch = ShowFetch {
            quick: true,
            ..ShowFetch::default()
        };
        let data = p.show(external_id, &fetch).await?;
        let owner = self.owner(p, &data)?;
        let preview = self.preview_of(owner.id(), &data).await?;
        if let Some(id) = preview.show.show_id
            && (provider, external_id) != (owner.id(), data.external_id.as_str())
        {
            repo::shows::add_alias(&self.db, provider, external_id, id).await?;
        }
        Ok(preview)
    }

    /// The followed show behind each `(provider, external_id)` (its own id, an alias, or the
    /// feed URL of an `rss` id); ids that are not followed are left out.
    pub async fn followed_shows(&self, keys: &[(String, String)]) -> Result<HashMap<(String, String), i64>> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let known = repo::shows::known(&self.db).await?;
        Ok(keys
            .iter()
            .filter_map(|(p, e)| known.find(p, e, None).map(|id| ((p.clone(), e.clone()), id)))
            .collect())
    }
}
