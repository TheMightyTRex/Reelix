//! Plex Media Server (JSON API with `X-Plex-Token`).

use std::path::PathBuf;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{check, json as read_json};
use crate::plextv::{Container, DISCOVER_URL, MetadataList, PLEX_TV_URL, PlexTv};
use crate::settings::{PathMap, Settings};
use crate::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};

const PAGE: usize = 500;

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: "plex".into(),
        name: "Plex".into(),
        description: Some(
            "Library refresh, availability sync, watch history, watchlists and Plex logins".into(),
        ),
        info_url: Some("https://support.plex.tv/articles/204059436".into()),
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder("http://plex:32400"),
            FieldSchema::new("token", "Token", FieldKind::Secret)
                .required()
                .help("X-Plex-Token of the server owner (sign in with Plex to fill it)"),
            FieldSchema::new("path_mappings", "Path mappings", FieldKind::List)
                .help("`reelix_path=server_path` when Plex sees files under another path")
                .advanced(),
            FieldSchema::new("plex_tv_url", "plex.tv URL", FieldKind::Url)
                .default_value(PLEX_TV_URL)
                .advanced(),
            FieldSchema::new("discover_url", "Discover URL", FieldKind::Url)
                .default_value(DISCOVER_URL)
                .advanced(),
        ],
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RootDto {
    #[serde(default)]
    friendly_name: String,
    #[serde(default)]
    machine_identifier: String,
    #[serde(default)]
    version: String,
}

#[derive(Deserialize)]
struct SectionsDto {
    #[serde(rename = "Directory", default)]
    directory: Vec<SectionDto>,
}

#[derive(Deserialize)]
struct SectionDto {
    key: String,
    #[serde(rename = "type")]
    section_type: String,
    #[serde(rename = "Location", default)]
    location: Vec<LocationDto>,
}

#[derive(Deserialize)]
struct LocationDto {
    path: String,
}

/// Plex server adapter.
pub struct Plex {
    http: reqwest::Client,
    url: String,
    token: String,
    paths: PathMap,
    client_id: String,
    plex_tv: PlexTv,
}

/// Whether `path` is `root` or inside it (separator-aware, either slash style).
pub(crate) fn is_within(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches(['/', '\\']);
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
}

impl Plex {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, MsError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            url: s.url("url")?,
            token: s.str("token")?,
            paths: PathMap::parse(&s.list("path_mappings"))?,
            client_id: ctx.client_id.clone(),
            plex_tv: PlexTv::new(ctx).with_base_urls(&s.url("plex_tv_url")?, &s.url("discover_url")?),
        })
    }

    /// The plex.tv client using this server's settings.
    pub fn plex_tv(&self) -> &PlexTv {
        &self.plex_tv
    }

    /// The owner's Discover watchlist (see [`PlexTv::watchlist`]).
    pub async fn watchlist(&self) -> Result<Vec<MsItem>, MsError> {
        self.plex_tv.watchlist(&self.token, true).await
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.url))
            .header(reqwest::header::ACCEPT, "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", &self.client_id)
    }

    async fn sections(&self) -> Result<Vec<SectionDto>, MsError> {
        let c: Container<SectionsDto> = read_json(self.get("/library/sections").send().await?).await?;
        Ok(c.media_container
            .directory
            .into_iter()
            .filter(|d| d.section_type == "movie" || d.section_type == "show")
            .collect())
    }

    async fn paged(&self, path: &str, query: &[(&str, &str)]) -> Result<MetadataList, MsError> {
        let mut all = MetadataList {
            metadata: Vec::new(),
            total_size: None,
        };
        loop {
            let start = all.metadata.len().to_string();
            let size = PAGE.to_string();
            let page: Container<MetadataList> = read_json(
                self.get(path)
                    .query(query)
                    .query(&[
                        ("X-Plex-Container-Start", start.as_str()),
                        ("X-Plex-Container-Size", size.as_str()),
                    ])
                    .send()
                    .await?,
            )
            .await?;
            let page = page.media_container;
            let n = page.metadata.len();
            all.metadata.extend(page.metadata);
            if n < PAGE || page.total_size.is_some_and(|t| all.metadata.len() >= t) {
                return Ok(all);
            }
        }
    }
}

#[async_trait]
impl MediaServer for Plex {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Plex
    }

    async fn test(&self) -> Result<ServerInfo, MsError> {
        let c: Container<RootDto> = read_json(self.get("/").send().await?).await?;
        let r = c.media_container;
        Ok(ServerInfo {
            kind: MediaServerKind::Plex,
            id: r.machine_identifier,
            name: r.friendly_name,
            version: r.version,
        })
    }

    async fn authenticate(&self, _username: &str, _password: &str) -> Result<ExternalUser, MsError> {
        Err(MsError::Unsupported(
            "Plex logins use the plex.tv PIN flow (PlexTv::create_pin / check_pin)",
        ))
    }

    /// The owner account (admin) followed by friends/shared users.
    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        let mut owner = self.plex_tv.account(&self.token).await?;
        owner.is_admin = true;
        owner.access_token = None;
        let mut out = vec![owner];
        out.extend(self.plex_tv.friends(&self.token).await?);
        Ok(out)
    }

    async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError> {
        let sections = self.sections().await?;
        if paths.is_empty() {
            for s in &sections {
                check(
                    self.get(&format!("/library/sections/{}/refresh", s.key))
                        .send()
                        .await?,
                )
                .await?;
            }
            return Ok(());
        }
        for p in paths {
            let server_path = self.paths.to_server(&p.to_string_lossy());
            let Some(section) = sections
                .iter()
                .find(|s| s.location.iter().any(|l| is_within(&server_path, &l.path)))
            else {
                tracing::warn!(path = %server_path, "no Plex library contains path; skipping refresh");
                continue;
            };
            check(
                self.get(&format!("/library/sections/{}/refresh", section.key))
                    .query(&[("path", server_path.as_str())])
                    .send()
                    .await?,
            )
            .await?;
        }
        Ok(())
    }

    async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
        let mut out = Vec::new();
        for s in self.sections().await? {
            let list = self
                .paged(
                    &format!("/library/sections/{}/all", s.key),
                    &[("includeGuids", "1")],
                )
                .await?;
            out.extend(list.metadata.into_iter().filter_map(|m| m.into_item()));
        }
        Ok(out)
    }

    /// `user_id` is the plex.tv account id (see [`MediaServer::users`]).
    async fn watch_history(&self, user_id: &str) -> Result<Vec<WatchedItem>, MsError> {
        let list = self
            .paged(
                "/status/sessions/history/all",
                &[("accountID", user_id), ("sort", "viewedAt:desc")],
            )
            .await?;
        Ok(list
            .metadata
            .into_iter()
            .filter_map(|m| {
                let is_episode = match m.item_type.as_str() {
                    "movie" => false,
                    "episode" => true,
                    _ => return None,
                };
                let (tmdb_id, tvdb_id, imdb_id) = m.ids();
                Some(WatchedItem {
                    kind: if is_episode {
                        MediaKind::Series
                    } else {
                        MediaKind::Movie
                    },
                    id: m.rating_key.unwrap_or_default(),
                    title: m.title,
                    series_title: m.grandparent_title,
                    series_id: m.grandparent_rating_key,
                    season: m.parent_index.filter(|_| is_episode),
                    episode: m.index.filter(|_| is_episode),
                    year: m.year,
                    tmdb_id,
                    tvdb_id,
                    imdb_id,
                    watched_at: m.viewed_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
                    play_count: 1,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::is_within;

    #[test]
    fn within_is_boundary_aware() {
        assert!(is_within("/media/movies/Dune (2021)", "/media/movies/"));
        assert!(is_within("/media/movies", "/media/movies"));
        assert!(!is_within("/media/movies-4k/Dune", "/media/movies"));
        assert!(is_within("D:\\Media\\TV\\Show", "D:\\Media\\TV"));
    }
}
