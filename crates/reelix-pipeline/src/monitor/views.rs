//! Queue and history DTOs (media titles and episodes resolved in batch).

use std::collections::HashMap;

use reelix_api_types::library::{EpisodeDto, Paged};
use reelix_api_types::pipeline::{
    EpisodeRefDto, FormatRefDto, HistoryDto, HistoryQuery, MediaRefDto, QueueItemDto,
};
use reelix_core::MediaKind;
use reelix_downloaders::DownloadState;
use reelix_library::import_api::TitleMap;

use super::{ActivityError, ActivityService, Result};
use crate::search::dto::{album_ref, book_ref, content_ids, radio_ref};
use crate::store::ContentRef;
use crate::store::history::{self, HistoryEvent, HistoryFilter};
use crate::store::tracked::{self, TrackedState};

/// Wire name of a client state (`queued`, `post_processing`, …).
pub fn client_state_str(s: DownloadState) -> &'static str {
    match s {
        DownloadState::Queued => "queued",
        DownloadState::Downloading => "downloading",
        DownloadState::Paused => "paused",
        DownloadState::PostProcessing => "post_processing",
        DownloadState::Completed => "completed",
        DownloadState::Failed => "failed",
        DownloadState::Warning => "warning",
    }
}

/// Batch-resolved media and episode references.
#[derive(Default)]
pub(crate) struct Refs {
    movies: TitleMap,
    series: TitleMap,
    episodes: HashMap<i64, EpisodeDto>,
    books: HashMap<i64, MediaRefDto>,
    albums: HashMap<i64, MediaRefDto>,
    radio: HashMap<i64, MediaRefDto>,
}

impl Refs {
    pub(crate) async fn load(
        svc: &ActivityService,
        movie_ids: Vec<i64>,
        series_ids: Vec<i64>,
        episode_ids: Vec<i64>,
    ) -> Result<Self> {
        fn dedup(mut v: Vec<i64>) -> Vec<i64> {
            v.sort_unstable();
            v.dedup();
            v
        }
        let imp = svc.library.importer();
        let movies = imp.titles(MediaKind::Movie, &dedup(movie_ids)).await?;
        let series = imp.titles(MediaKind::Series, &dedup(series_ids)).await?;
        let episodes = imp
            .episodes(&dedup(episode_ids))
            .await?
            .into_iter()
            .map(|e| (e.id, e))
            .collect();
        Ok(Self {
            movies,
            series,
            episodes,
            books: HashMap::new(),
            albums: HashMap::new(),
            radio: HashMap::new(),
        })
    }

    /// Adds book / album titles (deleted media is skipped).
    pub(crate) async fn load_content(
        &mut self,
        svc: &ActivityService,
        items: impl Iterator<Item = ContentRef>,
    ) {
        let (books, albums, radio) = content_ids(items);
        if let Some(r) = svc.radio.get() {
            for id in radio {
                if let Some(x) = radio_ref(r, id).await {
                    self.radio.insert(id, x);
                }
            }
        }
        if let Some(b) = svc.books.get() {
            for id in books {
                if let Ok(x) = b.get_book(id).await {
                    self.books.insert(id, book_ref(&x));
                }
            }
        }
        if let Some(m) = svc.music.get() {
            for id in albums {
                if let Ok(x) = m.get_album(id).await {
                    self.albums.insert(id, album_ref(&x));
                }
            }
        }
    }

    /// Movie / series reference, else the book (with format) / album.
    pub(crate) fn media_or_content(
        &self,
        movie_id: Option<i64>,
        series_id: Option<i64>,
        c: &ContentRef,
    ) -> Option<MediaRefDto> {
        if movie_id.is_some() || series_id.is_some() {
            return self.media(movie_id, series_id);
        }
        if let Some(id) = c.radio_episode_id {
            return self.radio.get(&id).cloned();
        }
        if let Some(id) = c.book_id {
            return self.books.get(&id).cloned().map(|mut m| {
                m.book_format = c.book_format;
                m
            });
        }
        c.album_id.and_then(|id| self.albums.get(&id).cloned())
    }

    pub(crate) fn media(&self, movie_id: Option<i64>, series_id: Option<i64>) -> Option<MediaRefDto> {
        let (kind, map, id) = match (movie_id, series_id) {
            (Some(id), _) => ("movie", &self.movies, id),
            (None, Some(id)) => ("series", &self.series, id),
            _ => return None,
        };
        map.get(&id).map(|(title, year)| MediaRefDto {
            kind: kind.into(),
            id,
            title: title.clone(),
            year: *year,
            subtitle: None,
            book_format: None,
        })
    }

    pub(crate) fn episode(&self, id: i64) -> Option<EpisodeRefDto> {
        self.episodes.get(&id).map(|e| EpisodeRefDto {
            id: e.id,
            season_number: e.season_number,
            episode_number: e.episode_number,
            title: e.title.clone(),
        })
    }

    pub(crate) fn episodes(&self, ids: &[i64]) -> Vec<EpisodeRefDto> {
        let mut out: Vec<_> = ids.iter().filter_map(|id| self.episode(*id)).collect();
        out.sort_by_key(|e| (e.season_number, e.episode_number));
        out
    }
}

fn page_bounds(page: Option<u32>, page_size: Option<u32>, total: usize) -> (usize, usize, u32, u32) {
    let page = page.unwrap_or(1).max(1);
    match page_size.filter(|s| *s > 0) {
        Some(size) => {
            let start = ((page - 1) as usize).saturating_mul(size as usize).min(total);
            let end = (start + size as usize).min(total);
            (start, end, page, size)
        }
        None => (0, total, 1, total as u32),
    }
}

impl ActivityService {
    /// `GET /queue`: active tracked downloads with live client progress. `page_size` 0/None = all.
    pub async fn queue(&self, page: Option<u32>, page_size: Option<u32>) -> Result<Paged<QueueItemDto>> {
        let rows = tracked::active(&self.db).await?;
        let total = rows.len();
        let (start, end, page, size) = page_bounds(page, page_size, total);
        let rows = &rows[start..end];
        let mut refs = Refs::load(
            self,
            rows.iter().filter_map(|t| t.movie_id).collect(),
            rows.iter().filter_map(|t| t.series_id).collect(),
            rows.iter().flat_map(|t| t.episode_ids.iter().copied()).collect(),
        )
        .await?;
        refs.load_content(self, rows.iter().map(|t| t.content)).await;
        let names: HashMap<i64, String> = self
            .providers
            .clients()
            .into_iter()
            .map(|c| (c.id, c.name))
            .collect();
        let items = rows
            .iter()
            .map(|t| {
                let snap = self.snapshot_of(t.download_client_id, &t.download_id);
                let rel = t.release.as_ref();
                let done = matches!(t.state, TrackedState::ImportPending | TrackedState::Importing);
                let progress = snap
                    .as_ref()
                    .map(|s| s.progress)
                    .unwrap_or(if done { 1.0 } else { 0.0 });
                let mut messages = t.status_messages.clone();
                if let Some(m) = snap.as_ref().and_then(|s| s.message.clone())
                    && snap.as_ref().is_some_and(|s| s.state == DownloadState::Warning)
                    && !messages.contains(&m)
                {
                    messages.push(m);
                }
                let client_completed = snap.as_ref().is_some_and(|s| s.state == DownloadState::Completed);
                QueueItemDto {
                    id: t.id,
                    title: t.title.clone(),
                    download_client_id: t.download_client_id,
                    download_client: names
                        .get(&t.download_client_id)
                        .cloned()
                        .or_else(|| snap.as_ref().map(|s| s.client_name.clone()))
                        .unwrap_or_else(|| format!("#{}", t.download_client_id)),
                    download_id: t.download_id.clone(),
                    protocol: t.protocol,
                    state: t.state.as_str().to_owned(),
                    client_state: snap.as_ref().map(|s| client_state_str(s.state).to_owned()),
                    progress,
                    size: snap
                        .as_ref()
                        .map(|s| s.total)
                        .filter(|s| *s > 0)
                        .or_else(|| rel.and_then(|r| r.size)),
                    remaining: snap.as_ref().map(|s| s.remaining),
                    eta_secs: snap.as_ref().and_then(|s| s.eta_secs),
                    media: refs.media_or_content(t.movie_id, t.series_id, &t.content),
                    episodes: refs.episodes(&t.episode_ids),
                    quality: rel.map(|r| r.quality),
                    custom_formats: rel
                        .map(|r| {
                            r.custom_formats
                                .iter()
                                .map(|f| FormatRefDto {
                                    id: f.id,
                                    name: f.name.clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    format_score: rel.map_or(0, |r| r.format_score),
                    indexer: rel.map(|r| r.indexer.clone()).filter(|i| !i.is_empty()),
                    messages,
                    output_path: t.output_path.clone(),
                    // Manual import maps video files; book / album downloads are retried instead.
                    can_manual_import: t.content.is_empty()
                        && (t.state == TrackedState::ImportPending
                            || (t.state == TrackedState::Downloading && client_completed)),
                    added: t.added,
                }
            })
            .collect();
        Ok(Paged {
            items,
            total: total as u64,
            page,
            page_size: size,
        })
    }

    /// `GET /history` (newest first; default page size 50, max 500).
    pub async fn history(&self, q: &HistoryQuery) -> Result<Paged<HistoryDto>> {
        let event = match q.event_type.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => Some(
                HistoryEvent::parse(s)
                    .ok_or_else(|| ActivityError::validation(format!("unknown event_type {s:?}")))?,
            ),
            None => None,
        };
        let page = q.page.unwrap_or(1).max(1);
        let size = q.page_size.unwrap_or(50).clamp(1, 500);
        let filter = HistoryFilter {
            movie_id: q.movie_id,
            series_id: q.series_id,
            episode_id: q.episode_id,
            event,
            download_id: None,
            book_id: q.book_id,
            author_id: q.author_id,
            album_id: q.album_id,
            artist_id: q.artist_id,
            radio_episode_id: q.radio_episode_id,
        };
        let (rows, total) = history::list(
            &self.db,
            &filter,
            i64::from(page - 1) * i64::from(size),
            i64::from(size),
        )
        .await?;
        let mut refs = Refs::load(
            self,
            rows.iter().filter_map(|h| h.movie_id).collect(),
            rows.iter().filter_map(|h| h.series_id).collect(),
            rows.iter().filter_map(|h| h.episode_id).collect(),
        )
        .await?;
        refs.load_content(self, rows.iter().map(|h| h.content)).await;
        let items = rows
            .into_iter()
            .map(|h| HistoryDto {
                id: h.id,
                event_type: h.event.as_str().to_owned(),
                date: h.date,
                media: refs.media_or_content(h.movie_id, h.series_id, &h.content),
                episode: h.episode_id.and_then(|e| refs.episode(e)),
                source_title: h.source_title,
                quality: h.quality,
                languages: h.languages,
                custom_formats: h
                    .custom_formats
                    .into_iter()
                    .map(|f| FormatRefDto {
                        id: f.id,
                        name: f.name,
                    })
                    .collect(),
                format_score: h.format_score,
                indexer: h.indexer,
                download_client: h.download_client,
                download_id: h.download_id,
                data: h.data,
            })
            .collect();
        Ok(Paged {
            items,
            total: total.max(0) as u64,
            page,
            page_size: size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging() {
        assert_eq!(page_bounds(None, None, 5), (0, 5, 1, 5));
        assert_eq!(page_bounds(Some(2), Some(2), 5), (2, 4, 2, 2));
        assert_eq!(page_bounds(Some(9), Some(2), 5), (5, 5, 9, 2));
    }
}
