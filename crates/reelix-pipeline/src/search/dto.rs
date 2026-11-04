//! API read models: release rows, media references, blocklist.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use reelix_api_types::pipeline::{
    BlocklistDto, EpisodeRefDto, FormatRefDto, MediaRefDto, Paged, RejectionDto, ReleaseDto,
};
use reelix_decision::RejectionKind;

use super::context::{Eval, Target};
use super::{Result, SearchError, SearchService};
use crate::store::{ContentRef, blocklist};

fn episode_ref(e: &reelix_api_types::library::EpisodeDto) -> EpisodeRefDto {
    EpisodeRefDto {
        id: e.id,
        season_number: e.season_number,
        episode_number: e.episode_number,
        title: e.title.clone(),
    }
}

/// One interactive-search row; `rank` counts down from the best release.
pub fn release_dto(e: &Eval, t: &Target, rank: u32, now: DateTime<Utc>) -> ReleaseDto {
    let c = e.candidate();
    let d = e.decision();
    let i = &e.info;
    ReleaseDto {
        guid: c.guid.clone(),
        title: c.title.clone(),
        indexer_id: i.indexer_id,
        indexer: i.indexer.clone(),
        protocol: i.protocol,
        size: i.size,
        publish_date: i.publish_date,
        age_minutes: i.publish_date.map(|_| c.age_minutes(now)),
        seeders: i.seeders,
        peers: i.peers,
        quality: c.parsed.quality,
        languages: c.parsed.languages.clone(),
        release_group: c.parsed.release_group.clone(),
        custom_formats: d
            .matched_formats
            .iter()
            .map(|f| FormatRefDto {
                id: f.id,
                name: f.name.clone(),
            })
            .collect(),
        format_score: d.format_score,
        indexer_flags: c
            .indexer_flags
            .iter()
            .map(|f| super::mapping::flag_name(*f))
            .collect(),
        info_url: i.info_url.clone(),
        accepted: d.accepted,
        rejections: d
            .rejections
            .iter()
            .map(|r| RejectionDto {
                reason: r.reason.clone(),
                kind: match r.kind {
                    RejectionKind::Permanent => "permanent",
                    RejectionKind::Temporary => "temporary",
                }
                .into(),
                spec: r.spec.clone(),
            })
            .collect(),
        rank,
        movie_id: t.movie_id(),
        series_id: t.series_id(),
        episodes: t.episodes_by_id(&c.episode_ids).iter().map(episode_ref).collect(),
        full_season: c.parsed.episode().is_some_and(|x| x.full_season),
        book_id: t.content().book_id,
        book_format: t.book_format(),
        album_id: t.content().album_id,
    }
}

/// Batch-resolved media titles.
#[derive(Default)]
pub struct MediaRefs {
    movies: HashMap<i64, MediaRefDto>,
    series: HashMap<i64, MediaRefDto>,
    episodes: HashMap<i64, EpisodeRefDto>,
    books: HashMap<i64, MediaRefDto>,
    albums: HashMap<i64, MediaRefDto>,
    radio: HashMap<i64, MediaRefDto>,
}

impl MediaRefs {
    pub fn media(&self, movie_id: Option<i64>, series_id: Option<i64>) -> Option<MediaRefDto> {
        movie_id
            .and_then(|id| self.movies.get(&id))
            .or_else(|| series_id.and_then(|id| self.series.get(&id)))
            .cloned()
    }

    /// The book (with the row's format), album or radio episode of a content reference.
    pub fn content(&self, c: &ContentRef) -> Option<MediaRefDto> {
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

    pub fn episodes(&self, ids: &[i64]) -> Vec<EpisodeRefDto> {
        ids.iter()
            .filter_map(|id| self.episodes.get(id).cloned())
            .collect()
    }
}

/// Book / album reference of a library book (`kind` `book`, author as subtitle).
pub fn book_ref(b: &reelix_api_types::books::BookDto) -> MediaRefDto {
    MediaRefDto {
        kind: "book".into(),
        id: b.id,
        title: b.title.clone(),
        year: b.year,
        subtitle: Some(b.author_name.clone()),
        book_format: None,
    }
}

/// Album reference (`kind` `album`, artist as subtitle).
pub fn album_ref(a: &reelix_api_types::music::AlbumDto) -> MediaRefDto {
    use chrono::Datelike;
    MediaRefDto {
        kind: "album".into(),
        id: a.id,
        title: a.title.clone(),
        year: a.release_date.map(|d| d.year()),
        subtitle: Some(a.artist_name.clone()),
        book_format: None,
    }
}

/// Radio episode reference (`kind` `radio_episode`, `S20E03 - Title`, show as subtitle).
pub async fn radio_ref(svc: &reelix_radio::RadioService, episode_id: i64) -> Option<MediaRefDto> {
    use chrono::Datelike;
    let e = svc.get_episode(episode_id).await.ok()?;
    let show = svc.get_show(e.show_id).await.ok()?;
    let series = reelix_radio::status::filing_series_number(e.series_kind, e.series_number);
    let title = match e.number {
        Some(n) if e.title.trim().is_empty() => format!("S{series:02}E{n:02}"),
        Some(n) => format!("S{series:02}E{n:02} - {}", e.title.trim()),
        None => e.title.clone(),
    };
    Some(MediaRefDto {
        kind: "radio_episode".into(),
        id: e.id,
        title,
        year: e.first_broadcast.map(|d| d.year()),
        subtitle: Some(show.title),
        book_format: None,
    })
}

/// Distinct book, album and radio episode ids of content references.
pub fn content_ids(items: impl Iterator<Item = ContentRef>) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
    let (mut books, mut albums, mut radio) = (Vec::new(), Vec::new(), Vec::new());
    for c in items {
        books.extend(c.book_id);
        albums.extend(c.album_id);
        radio.extend(c.radio_episode_id);
    }
    (
        uniq(books.into_iter()),
        uniq(albums.into_iter()),
        uniq(radio.into_iter()),
    )
}

fn uniq(it: impl Iterator<Item = i64>) -> Vec<i64> {
    let mut v: Vec<i64> = it.collect();
    v.sort_unstable();
    v.dedup();
    v
}

impl SearchService {
    pub async fn media_refs(
        &self,
        movie_ids: impl Iterator<Item = i64>,
        series_ids: impl Iterator<Item = i64>,
        episode_ids: impl Iterator<Item = i64>,
    ) -> Result<MediaRefs> {
        let api = self.library.search_api();
        let (m, s, e) = (uniq(movie_ids), uniq(series_ids), uniq(episode_ids));
        let mut refs = MediaRefs::default();
        if !m.is_empty() {
            for x in api.movies(Some(&m)).await? {
                refs.movies.insert(
                    x.id,
                    MediaRefDto {
                        kind: "movie".into(),
                        id: x.id,
                        title: x.title,
                        year: x.year,
                        subtitle: None,
                        book_format: None,
                    },
                );
            }
        }
        if !s.is_empty() {
            for x in api.series(Some(&s)).await? {
                refs.series.insert(
                    x.id,
                    MediaRefDto {
                        kind: "series".into(),
                        id: x.id,
                        title: x.title,
                        year: x.year,
                        subtitle: None,
                        book_format: None,
                    },
                );
            }
        }
        if !e.is_empty() {
            for x in api.episodes_by_ids(&e).await? {
                refs.episodes.insert(x.id, episode_ref(&x));
            }
        }
        Ok(refs)
    }

    /// Adds book / album / radio episode references (titles with author / artist / show) to
    /// `refs`. Unknown ids
    /// (deleted media) are skipped.
    pub(crate) async fn content_refs(&self, refs: &mut MediaRefs, items: impl Iterator<Item = ContentRef>) {
        let (books, albums, radio) = content_ids(items);
        if let Some(svc) = self.radio.get() {
            for id in radio {
                if let Some(r) = radio_ref(svc, id).await {
                    refs.radio.insert(id, r);
                }
            }
        }
        if let Some(svc) = self.books.get() {
            for id in books {
                if let Ok(b) = svc.get_book(id).await {
                    refs.books.insert(id, book_ref(&b));
                }
            }
        }
        if let Some(svc) = self.music.get() {
            for id in albums {
                if let Ok(a) = svc.get_album(id).await {
                    refs.albums.insert(id, album_ref(&a));
                }
            }
        }
    }

    /// `GET /blocklist` (newest first; `page` 1-based, default page size 50).
    pub async fn list_blocklist(
        &self,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> Result<Paged<BlocklistDto>> {
        let page = page.unwrap_or(1).max(1);
        let size = page_size.unwrap_or(50);
        let (offset, limit) = if size == 0 {
            (0, i64::MAX)
        } else {
            (i64::from(page - 1) * i64::from(size), i64::from(size))
        };
        let (rows, total) = blocklist::list(&self.db, offset, limit).await?;
        let mut refs = self
            .media_refs(
                rows.iter().filter_map(|b| b.movie_id),
                rows.iter().filter_map(|b| b.series_id),
                std::iter::empty(),
            )
            .await?;
        self.content_refs(&mut refs, rows.iter().map(|b| b.content)).await;
        Ok(Paged {
            items: rows
                .into_iter()
                .map(|b| BlocklistDto {
                    id: b.id,
                    media: refs
                        .media(b.movie_id, b.series_id)
                        .or_else(|| refs.content(&b.content)),
                    source_title: b.source_title,
                    quality: b.quality,
                    protocol: b.protocol,
                    indexer: b.indexer,
                    message: b.message,
                    date: b.date,
                })
                .collect(),
            total: total as u64,
            page,
            page_size: size,
        })
    }

    pub async fn delete_blocklist(&self, id: i64) -> Result<()> {
        if !blocklist::delete(&self.db, id).await? {
            return Err(SearchError::NotFound(format!("blocklist entry {id} not found")));
        }
        self.emit("blocklist", "deleted", id);
        Ok(())
    }

    /// Removes the given entries, or everything when `ids` is empty. Returns the count.
    pub async fn clear_blocklist(&self, ids: &[i64]) -> Result<u64> {
        let n = if ids.is_empty() {
            blocklist::clear(&self.db).await?
        } else {
            blocklist::delete_many(&self.db, ids).await?
        };
        if n > 0 {
            self.emit("blocklist", "deleted", 0);
        }
        Ok(n)
    }
}
