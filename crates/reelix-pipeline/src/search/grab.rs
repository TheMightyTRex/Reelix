//! Sending a release to a download client: payload through the indexer, client choice by
//! protocol + tags, then `tracked_downloads` + `history` rows, notification and events.

use chrono::Duration;
use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto};
use reelix_api_types::pipeline::GrabResponse;
use reelix_core::{MediaKind, Protocol};
use reelix_downloaders::{AddOptions, PayloadPreference, ReleasePayload};
use reelix_indexers::IndexerError;
use reelix_notify::{EpisodeSummary, MediaSummary, NotificationEvent, ReleaseSummary};

use super::context::{Eval, IndexerInfo, Target};
use super::mapping::flag_name;
use super::{Result, SearchError, SearchService};
use crate::store::history::{self, HistoryEvent, NewHistory};
use crate::store::tracked::{self, NewTracked};
use crate::{FormatRef, GrabbedRelease};

/// The media a grab is for (all empty for a raw grab).
#[derive(Debug, Clone, Default)]
pub struct GrabTarget {
    pub movie: Option<MovieDto>,
    pub series: Option<SeriesDto>,
    pub episodes: Vec<EpisodeDto>,
}

impl GrabTarget {
    pub fn from_target(t: &Target, episode_ids: &[i64]) -> Self {
        match t {
            Target::Movie(m) => Self {
                movie: Some(m.clone()),
                ..Self::default()
            },
            Target::Series { series, .. } => Self {
                movie: None,
                series: Some(series.clone()),
                episodes: t.episodes_by_id(episode_ids),
            },
        }
    }

    fn tags(&self) -> Vec<i64> {
        self.movie
            .as_ref()
            .map(|m| m.tags.clone())
            .or_else(|| self.series.as_ref().map(|s| s.tags.clone()))
            .unwrap_or_default()
    }

    fn kind(&self) -> Option<MediaKind> {
        if self.movie.is_some() {
            Some(MediaKind::Movie)
        } else if self.series.is_some() {
            Some(MediaKind::Series)
        } else {
            None
        }
    }

    /// Released / aired within the last 14 days.
    fn is_recent(&self, today: chrono::NaiveDate) -> bool {
        let recent = |d: chrono::NaiveDate| d <= today && today - d <= Duration::days(14);
        match &self.movie {
            Some(m) => [m.in_cinemas, m.digital_release, m.physical_release]
                .into_iter()
                .flatten()
                .any(recent),
            None => self.episodes.iter().filter_map(|e| e.air_date).any(recent),
        }
    }

    fn summary(&self) -> Option<MediaSummary> {
        if let Some(m) = &self.movie {
            let mut s = MediaSummary::new(MediaKind::Movie, &m.title);
            s.id = Some(m.id);
            s.year = m.year;
            s.tmdb_id = Some(i64::from(m.tmdb_id));
            s.imdb_id = m.imdb_id.clone();
            s.poster_url = m.images.poster.clone();
            s.overview = m.overview.clone();
            return Some(s);
        }
        let series = self.series.as_ref()?;
        let mut s = MediaSummary::new(MediaKind::Series, &series.title);
        s.id = Some(series.id);
        s.year = series.year;
        s.tmdb_id = series.tmdb_id.map(i64::from);
        s.tvdb_id = series.tvdb_id.map(i64::from);
        s.imdb_id = series.imdb_id.clone();
        s.poster_url = series.images.poster.clone();
        s.overview = series.overview.clone();
        s.episodes = self
            .episodes
            .iter()
            .map(|e| EpisodeSummary {
                season: e.season_number,
                episode: e.episode_number,
                absolute: e.absolute_number,
                title: e.title.clone(),
                air_date: e.air_date.map(|d| d.to_string()),
            })
            .collect();
        Some(s)
    }
}

/// Snapshot stored with tracked / pending rows.
pub fn snapshot(e: &Eval, ix: Option<&IndexerInfo>) -> GrabbedRelease {
    let c = e.candidate();
    let d = e.decision();
    let i = &e.info;
    GrabbedRelease {
        guid: c.guid.clone(),
        title: c.title.clone(),
        indexer_id: Some(i.indexer_id),
        indexer: i.indexer.clone(),
        protocol: i.protocol,
        size: i.size,
        publish_date: i.publish_date,
        download_url: i.download_url.clone(),
        magnet_url: i.magnet_url.clone(),
        info_url: i.info_url.clone(),
        info_hash: i.info_hash.clone(),
        seeders: i.seeders,
        quality: c.parsed.quality,
        languages: c.parsed.languages.clone(),
        release_group: c.parsed.release_group.clone(),
        custom_formats: d
            .matched_formats
            .iter()
            .map(|f| FormatRef {
                id: f.id,
                name: f.name.clone(),
            })
            .collect(),
        format_score: d.format_score,
        indexer_flags: c.indexer_flags.iter().map(|f| flag_name(*f)).collect(),
        seed_ratio: ix.and_then(|x| x.seed_ratio).or(i.minimum_ratio),
        seed_time_minutes: ix
            .and_then(|x| x.seed_time_minutes)
            .or(i.minimum_seed_time.map(|s| s.div_ceil(60) as u32)),
    }
}

fn file_name(title: &str, ext: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-()[] ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{}.{ext}",
        if clean.trim().is_empty() {
            "release"
        } else {
            clean.trim()
        }
    )
}

impl SearchService {
    /// The media for a grab from library ids (`episode_ids` determine the series).
    pub async fn grab_target(&self, movie_id: Option<i64>, episode_ids: &[i64]) -> Result<GrabTarget> {
        if let Some(id) = movie_id {
            return Ok(GrabTarget {
                movie: Some(self.movie(id).await?),
                ..GrabTarget::default()
            });
        }
        if episode_ids.is_empty() {
            return Ok(GrabTarget::default());
        }
        let api = self.library.search_api();
        let episodes = api.episodes_by_ids(episode_ids).await?;
        let Some(series_id) = episodes.first().map(|e| e.series_id) else {
            return Err(SearchError::NotFound("episodes not found".into()));
        };
        if episodes.iter().any(|e| e.series_id != series_id) {
            return Err(SearchError::Validation(
                "episodes belong to different series".into(),
            ));
        }
        let series = api.series(Some(&[series_id])).await?.pop();
        Ok(GrabTarget {
            movie: None,
            series,
            episodes,
        })
    }

    async fn payload(&self, rel: &GrabbedRelease, pref: PayloadPreference) -> Result<ReleasePayload> {
        let url = rel.download_url.clone().filter(|u| !u.trim().is_empty());
        let Some(url) = url else {
            return rel
                .magnet_url
                .clone()
                .map(ReleasePayload::Magnet)
                .ok_or_else(|| SearchError::Validation("release has no download link".into()));
        };
        if url.starts_with("magnet:") {
            return Ok(ReleasePayload::Magnet(url));
        }
        if pref == PayloadPreference::Url {
            return Ok(ReleasePayload::Url(url));
        }
        let Some(ix) = rel.indexer_id else {
            return Ok(ReleasePayload::Url(url));
        };
        match self.providers.indexer_manager().download(ix, &url).await {
            Ok(bytes) => Ok(match rel.protocol {
                Protocol::Torrent => ReleasePayload::TorrentFile {
                    bytes,
                    name: file_name(&rel.title, "torrent"),
                },
                Protocol::Usenet => ReleasePayload::NzbFile {
                    bytes,
                    name: file_name(&rel.title, "nzb"),
                },
            }),
            Err(IndexerError::MagnetRedirect(m)) => Ok(ReleasePayload::Magnet(m)),
            Err(e) => match &rel.magnet_url {
                Some(m) => Ok(ReleasePayload::Magnet(m.clone())),
                None => Err(SearchError::Unavailable(format!(
                    "cannot fetch the release from {}: {e}",
                    rel.indexer
                ))),
            },
        }
    }

    /// Sends `rel` to a download client and starts tracking it.
    pub async fn grab(
        &self,
        rel: &GrabbedRelease,
        t: &GrabTarget,
        client_id: Option<i64>,
    ) -> Result<GrabResponse> {
        let tags = t.tags();
        let (row, client) = match client_id {
            Some(id) => {
                let (row, c) = self.providers.download_client(id).ok_or_else(|| {
                    SearchError::NotFound(format!("download client {id} not found or disabled"))
                })?;
                if c.protocol() != rel.protocol {
                    return Err(SearchError::Validation(format!(
                        "download client {} does not handle {:?} releases",
                        row.name, rel.protocol
                    )));
                }
                (row, c)
            }
            None => self
                .providers
                .download_clients_for(rel.protocol)
                .into_iter()
                .find(|(r, _)| r.tags.is_empty() || r.tags.iter().any(|x| tags.contains(x)))
                .ok_or_else(|| {
                    SearchError::Unavailable(format!(
                        "no enabled {} download client is available for this media",
                        match rel.protocol {
                            Protocol::Torrent => "torrent",
                            Protocol::Usenet => "usenet",
                        }
                    ))
                })?,
        };
        let payload = self.payload(rel, client.payload_preference()).await?;
        let today = self.library.search_api().today();
        let opts = AddOptions {
            category: None,
            media_kind: t.kind(),
            priority: None,
            is_recent: t.is_recent(today),
            add_paused: false,
            seed_ratio: rel.seed_ratio.filter(|_| rel.protocol == Protocol::Torrent),
            seed_time_minutes: rel
                .seed_time_minutes
                .filter(|_| rel.protocol == Protocol::Torrent)
                .map(u64::from),
            title: rel.title.clone(),
        };
        let download_id = client
            .add(&payload, &opts)
            .await
            .map_err(|e| SearchError::Unavailable(format!("{} rejected the release: {e}", row.name)))?;
        let movie_id = t.movie.as_ref().map(|m| m.id);
        let series_id = t.series.as_ref().map(|s| s.id);
        let episode_ids: Vec<i64> = t.episodes.iter().map(|e| e.id).collect();
        let queue_id = tracked::insert(
            &self.db,
            &NewTracked {
                download_client_id: row.id,
                download_id: download_id.0.clone(),
                protocol: rel.protocol,
                title: rel.title.clone(),
                movie_id,
                series_id,
                episode_ids: episode_ids.clone(),
                release: rel.clone(),
            },
        )
        .await?;
        let mut h = NewHistory::new(HistoryEvent::Grabbed, &rel.title);
        h.movie_id = movie_id;
        h.series_id = series_id;
        h.quality = Some(rel.quality);
        h.languages = rel.languages.clone();
        h.custom_formats = rel.custom_formats.clone();
        h.format_score = rel.format_score;
        h.download_id = Some(download_id.0.clone());
        h.protocol = Some(rel.protocol);
        h.indexer = Some(rel.indexer.clone());
        h.download_client = Some(row.name.clone());
        h.data = serde_json::json!({
            "guid": rel.guid,
            "indexer_id": rel.indexer_id,
            "size": rel.size,
            "publish_date": rel.publish_date,
            "info_url": rel.info_url,
            "release_group": rel.release_group,
            "indexer_flags": rel.indexer_flags,
            "download_client_id": row.id,
            "queue_id": queue_id,
        });
        if episode_ids.is_empty() {
            let id = history::insert(&self.db, &h).await?;
            self.emit("history", "created", id);
        } else {
            for e in &episode_ids {
                h.episode_id = Some(*e);
                let id = history::insert(&self.db, &h).await?;
                self.emit("history", "created", id);
            }
        }
        self.clear_pending_for(movie_id, series_id, &episode_ids).await?;
        tracing::info!(title = %rel.title, client = %row.name, download_id = %download_id, "release grabbed");
        self.emit("queue", "grabbed", queue_id);
        if let Some(media) = t.summary() {
            let release = ReleaseSummary {
                title: rel.title.clone(),
                indexer: Some(rel.indexer.clone()),
                size_bytes: rel.size,
                quality: Some(rel.quality.quality.name().to_owned()),
                protocol: Some(rel.protocol),
                release_group: rel.release_group.clone(),
                custom_formats: rel.custom_formats.iter().map(|f| f.name.clone()).collect(),
                custom_format_score: Some(rel.format_score),
                download_client: Some(row.name.clone()),
                download_id: Some(download_id.0.clone()),
            };
            self.providers
                .notify(&NotificationEvent::Grab { media, release })
                .await;
        }
        Ok(GrabResponse {
            queue_id,
            download_client: row.name,
            download_id: download_id.0,
        })
    }
}
