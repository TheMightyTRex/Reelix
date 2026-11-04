//! Sending a release to a download client: payload through the indexer, client choice by
//! protocol + tags, then `tracked_downloads` + `history` rows, notification and events.

use chrono::Duration;
use reelix_api_types::library::{EpisodeDto, MovieDto, SeriesDto};
use reelix_api_types::pipeline::GrabResponse;
use reelix_books::BookTarget;
use reelix_core::{ContentKind, MediaKind, Protocol};
use reelix_downloaders::{AddOptions, PayloadPreference, ReleasePayload};
use reelix_indexers::IndexerError;
use reelix_music::AlbumTarget;
use reelix_notify::{EpisodeSummary, MediaSummary, NotificationEvent, ReleaseSummary};
use reelix_radio::RadioTarget;

use super::context::{Eval, IndexerInfo, Target};
use super::mapping::flag_name;
use super::{Result, SearchError, SearchService};
use crate::store::ContentRef;
use crate::store::history::{self, HistoryEvent, NewHistory};
use crate::store::tracked::{self, NewTracked};
use crate::{FormatRef, GrabbedRelease};

/// The media a grab is for (all empty for a raw grab).
#[derive(Debug, Clone, Default)]
pub struct GrabTarget {
    pub movie: Option<MovieDto>,
    pub series: Option<SeriesDto>,
    pub episodes: Vec<EpisodeDto>,
    /// One format of a book.
    pub book: Option<Box<BookTarget>>,
    pub album: Option<Box<AlbumTarget>>,
    /// One radio episode.
    pub radio: Option<Box<RadioTarget>>,
}

impl GrabTarget {
    pub fn from_target(t: &Target, episode_ids: &[i64]) -> Self {
        match t {
            Target::Movie(m) => Self {
                movie: Some(m.clone()),
                ..Self::default()
            },
            Target::Series { series, .. } => Self {
                series: Some(series.clone()),
                episodes: t.episodes_by_id(episode_ids),
                ..Self::default()
            },
            Target::Book(b) => Self {
                book: Some(b.clone()),
                ..Self::default()
            },
            Target::Album(a) => Self {
                album: Some(a.clone()),
                ..Self::default()
            },
            Target::Radio(r) => Self {
                radio: Some(r.clone()),
                ..Self::default()
            },
        }
    }

    fn tags(&self) -> Vec<i64> {
        self.movie
            .as_ref()
            .map(|m| m.tags.clone())
            .or_else(|| self.series.as_ref().map(|s| s.tags.clone()))
            .or_else(|| self.book.as_ref().map(|b| b.tags.clone()))
            .or_else(|| self.album.as_ref().map(|a| a.tags.clone()))
            .or_else(|| self.radio.as_ref().map(|r| r.tags.clone()))
            .unwrap_or_default()
    }

    /// Book (format) / album / radio episode reference.
    pub fn content(&self) -> ContentRef {
        if let Some(b) = &self.book {
            return ContentRef::book(b.book.id, b.format);
        }
        if let Some(r) = &self.radio {
            return ContentRef::radio_episode(r.episode_id());
        }
        self.album
            .as_ref()
            .map(|a| ContentRef::album(a.album.id))
            .unwrap_or_default()
    }

    fn content_kind(&self) -> Option<ContentKind> {
        self.book
            .as_ref()
            .map(|b| b.format.content_kind())
            .or_else(|| self.album.as_ref().map(|_| ContentKind::Music))
            .or_else(|| self.radio.as_ref().map(|_| ContentKind::Radio))
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
        if let Some(b) = &self.book {
            return b.book.release_date.is_some_and(recent);
        }
        if let Some(a) = &self.album {
            return a.album.release_date.is_some_and(recent);
        }
        if let Some(r) = &self.radio {
            return r.episode.first_broadcast.is_some_and(|d| recent(d.date_naive()));
        }
        match &self.movie {
            Some(m) => [m.in_cinemas, m.digital_release, m.physical_release]
                .into_iter()
                .flatten()
                .any(recent),
            None => self.episodes.iter().filter_map(|e| e.air_date).any(recent),
        }
    }

    fn summary(&self) -> Option<MediaSummary> {
        if let Some(b) = &self.book {
            return Some(book_summary(b));
        }
        if let Some(a) = &self.album {
            return Some(album_summary(a));
        }
        if let Some(r) = &self.radio {
            return Some(radio_summary(r));
        }
        if let Some(m) = &self.movie {
            let mut s = MediaSummary::new(MediaKind::Movie, &m.title);
            s.id = Some(m.id);
            s.year = m.year;
            s.tmdb_id = Some(i64::from(m.tmdb_id));
            s.imdb_id = m.imdb_id.clone();
            s.poster_url = m.images.poster.clone();
            s.backdrop_url = m.images.backdrop.clone();
            s.overview = m.overview.clone();
            s.genres = m.genres.clone();
            s.rating = Some(m.ratings.vote_average).filter(|r| *r > 0.0);
            s.trailer_url = m.youtube_trailer.as_deref().map(crate::monitor::youtube_url);
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
        s.backdrop_url = series.images.backdrop.clone();
        s.overview = series.overview.clone();
        s.genres = series.genres.clone();
        s.rating = Some(series.ratings.vote_average).filter(|r| *r > 0.0);
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

/// Notification summary of a book format (`Author - Title`).
pub(crate) fn book_summary(b: &BookTarget) -> MediaSummary {
    let mut s = MediaSummary::content(
        b.format.content_kind(),
        format!("{} - {}", b.author.name, b.book.title),
    );
    s.id = Some(b.book.id);
    s.year = b.book.year;
    s.poster_url = b.book.images.poster.clone();
    s.overview = b.book.overview.clone();
    s
}

/// Notification summary of an album (`Artist - Album`).
pub(crate) fn album_summary(a: &AlbumTarget) -> MediaSummary {
    use chrono::Datelike;
    let mut s = MediaSummary::content(
        ContentKind::Music,
        format!("{} - {}", a.artist.name, a.album.title),
    );
    s.id = Some(a.album.id);
    s.year = a.album.release_date.map(|d| d.year());
    s.poster_url = a.album.images.poster.clone();
    s
}

/// Notification summary of a radio episode (`Show - S20E03 - Title`).
pub(crate) fn radio_summary(r: &RadioTarget) -> MediaSummary {
    use chrono::Datelike;
    let e = &r.episode;
    let mut title = e.show_title.clone();
    if let Some(label) = r.number_label() {
        title.push_str(&format!(" - {label}"));
    }
    if !e.title.trim().is_empty() {
        title.push_str(&format!(" - {}", e.title.trim()));
    }
    let mut s = MediaSummary::content(ContentKind::Radio, title);
    s.id = Some(e.episode_id);
    s.year = e.first_broadcast.map(|d| d.year());
    s
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
    /// The book / album / radio episode for a grab.
    pub async fn grab_target_content(&self, c: &ContentRef) -> Result<GrabTarget> {
        let t = self.content_target(c).await?;
        Ok(GrabTarget::from_target(&t, &[]))
    }

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
            series,
            episodes,
            ..GrabTarget::default()
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
        let ix = match rel.indexer_id {
            Some(ix) if pref != PayloadPreference::Url => ix,
            _ => {
                // The link is handed on as-is (to the download client, or fetched by the
                // torrent adapter), so the SSRF guard has to run here.
                let parsed = url::Url::parse(url.trim())
                    .map_err(|e| SearchError::Validation(format!("invalid download link: {e}")))?;
                self.guard()
                    .check(&parsed)
                    .await
                    .map_err(|e| SearchError::Validation(format!("download link rejected: {e}")))?;
                return Ok(ReleasePayload::Url(url));
            }
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
        let content = t.content();
        let opts = AddOptions {
            // Books and music go to the client's book / music category.
            category: t.content_kind().and_then(|k| client.category_for_kind(k)),
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
        if !content.is_empty() {
            tracked::set_content(&self.db, queue_id, &content).await?;
        }
        let mut h = NewHistory::new(HistoryEvent::Grabbed, &rel.title);
        h.movie_id = movie_id;
        h.series_id = series_id;
        h.content = content;
        h.author_id = t.book.as_ref().map(|b| b.author.id);
        h.artist_id = t.album.as_ref().map(|a| a.artist.id);
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
        if let Some(c) = &opts.category
            && !content.is_empty()
        {
            h.data["category"] = serde_json::Value::String(c.clone());
        }
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
        if content.is_empty() {
            self.clear_pending_for(movie_id, series_id, &episode_ids).await?;
        } else {
            self.clear_pending_for_content(&content).await?;
        }
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
