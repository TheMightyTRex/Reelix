//! Book (Open Library) and music (MusicBrainz) search and request views, overlaid with
//! library availability and request state.

use std::collections::HashMap;

use reelix_api_types::requests::{
    ArtistItemDto, AvailabilityStatus, BookSearchPageDto, ContentDetailDto, ContentItemDto,
    ContentRequestAbilityDto, ContentRequestKind, ContentRowDto, MusicSearchPageDto, UserBriefDto,
};
use reelix_core::{BookFormat, Permissions};

use crate::content::{
    advanced_allowed, content_permission, default_formats, kind_word, requested_formats, to_brief,
};
use crate::content_repo::ContentRow;
use crate::discovery::status_rank;
use crate::gateway::{Availability, ContentHit};
use crate::{Actor, Error, RequestService, Result};

fn upstream(e: anyhow::Error) -> Error {
    Error::Upstream(format!("metadata provider error: {e}"))
}

/// The most relevant request: active ones first, then by status, newest first.
fn best(rows: &[ContentRow]) -> Option<&ContentRow> {
    rows.iter()
        .max_by_key(|r| (r.status.is_active(), status_rank(r.status), r.created_at))
}

impl RequestService {
    /// Builds result items for `hits` with availability and the caller's visible requests.
    async fn content_items(
        &self,
        actor: &Actor,
        kind: ContentRequestKind,
        hits: Vec<ContentHit>,
    ) -> Result<Vec<ContentItemDto>> {
        let ids: Vec<String> = hits.iter().map(|h| h.foreign_id.clone()).collect();
        let avail = self
            .library
            .content_availability(kind, &ids)
            .await
            .map_err(|e| Error::Upstream(format!("library: {e}")))?;
        let requests = self.content_requests_by_id(actor, kind, &ids, false).await?;
        let users = requests.values().flatten().map(|r| r.user_id).collect::<Vec<_>>();
        let briefs = self.users_repo().briefs(users).await?;
        Ok(hits
            .into_iter()
            .map(|h| item(kind, h, &avail, &requests, &briefs))
            .collect())
    }

    /// `GET /search/books?q=`: Open Library works with availability.
    pub async fn search_books(&self, actor: &Actor, q: &str) -> Result<BookSearchPageDto> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(BookSearchPageDto { results: Vec::new() });
        }
        let hits = self.library.lookup_books(q).await.map_err(upstream)?;
        Ok(BookSearchPageDto {
            results: self.content_items(actor, ContentRequestKind::Book, hits).await?,
        })
    }

    /// `GET /search/music?q=`: MusicBrainz albums (with availability) and artists.
    pub async fn search_music(&self, actor: &Actor, q: &str) -> Result<MusicSearchPageDto> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(MusicSearchPageDto {
                albums: Vec::new(),
                artists: Vec::new(),
            });
        }
        let hits = self.library.lookup_albums(q).await.map_err(upstream)?;
        let albums = self.content_items(actor, ContentRequestKind::Music, hits).await?;
        let artists = match self.library.lookup_artists(q).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "artist search failed");
                Vec::new()
            }
        };
        Ok(MusicSearchPageDto {
            albums,
            artists: artists
                .into_iter()
                .map(|a| ArtistItemDto {
                    musicbrainz_id: a.musicbrainz_id,
                    name: a.name,
                    disambiguation: a.disambiguation,
                    artist_type: a.artist_type,
                    country: a.country,
                    library_id: a.library_id,
                })
                .collect(),
        })
    }

    /// `GET /discover/content/{kind}/{row}`: a Discover row of books or albums with
    /// availability. Hides available items when the request settings say so.
    pub async fn discover_content(
        &self,
        actor: &Actor,
        kind: ContentRequestKind,
        row: &str,
    ) -> Result<ContentRowDto> {
        if !kind.discover_rows().iter().any(|(id, _)| *id == row) {
            return Err(Error::NotFound(format!(
                "unknown {} discover row: {row}",
                kind.as_str()
            )));
        }
        let hits = self.library.discover_content(kind, row).await.map_err(upstream)?;
        let mut results = self.content_items(actor, kind, hits).await?;
        if self.request_settings().await?.hide_available {
            results.retain(|i| i.availability != AvailabilityStatus::Available);
        }
        Ok(ContentRowDto { results })
    }

    /// `GET /media/book/{workId}` / `GET /media/album/{releaseGroupId}`: the request view.
    pub async fn content_detail(
        &self,
        actor: &Actor,
        kind: ContentRequestKind,
        id: &str,
    ) -> Result<ContentDetailDto> {
        let id = id.trim();
        let hit = self
            .library
            .content(kind, id)
            .await
            .map_err(upstream)?
            .ok_or_else(|| Error::NotFound(format!("{} not found", kind_word(kind))))?;
        let overview = hit.overview.clone();
        let foreign_id = hit.foreign_id.clone();
        let mut items = self.content_items(actor, kind, vec![hit]).await?;
        let item = items.remove(0);

        // Duplicate checks look at every user's requests; the list shows only visible ones.
        let ids = [foreign_id.clone()];
        let all = self
            .content_requests_by_id(actor, kind, &ids, true)
            .await?
            .remove(&foreign_id)
            .unwrap_or_default();
        let visible: Vec<&ContentRow> = all
            .iter()
            .filter(|r| r.user_id == actor.id || crate::requests::sees_all_requests(actor.permissions))
            .collect();
        let briefs = self
            .users_repo()
            .briefs(visible.iter().map(|r| r.user_id))
            .await?;
        let requests = visible.iter().map(|r| to_brief(r, &briefs)).collect();

        let settings = self.request_settings().await?;
        let active: Vec<&ContentRow> = all.iter().filter(|r| r.status.is_active()).collect();
        let formats: Vec<BookFormat> = match kind {
            ContentRequestKind::Book => {
                let taken = requested_formats(active.iter().copied());
                BookFormat::ALL
                    .into_iter()
                    .filter(|f| !taken.contains(f))
                    .collect()
            }
            ContentRequestKind::Music => Vec::new(),
        };
        let mut reason = None;
        if !actor.permissions.has(content_permission(kind)) {
            reason = Some(match kind {
                ContentRequestKind::Book => "You are not allowed to request books".to_string(),
                ContentRequestKind::Music => "You are not allowed to request music".to_string(),
            });
        } else if item.availability == AvailabilityStatus::Available {
            reason = Some("Already available".into());
        } else if match kind {
            ContentRequestKind::Book => formats.is_empty(),
            ContentRequestKind::Music => !active.is_empty(),
        } {
            reason = Some("Already requested".into());
        } else if !actor.permissions.has(Permissions::MANAGE_REQUESTS) {
            let me = self.user_record(actor.id).await?;
            let q = self.quota_of(&me, &settings).await?;
            let usage = match kind {
                ContentRequestKind::Book => &q.book,
                ContentRequestKind::Music => &q.music,
            };
            if usage.restricted {
                reason = Some(format!(
                    "Request quota reached ({} per {} days)",
                    usage.limit.unwrap_or_default(),
                    usage.days
                ));
            }
        }
        let default_formats = match kind {
            ContentRequestKind::Book => {
                let d: Vec<BookFormat> = default_formats(&settings)
                    .into_iter()
                    .filter(|f| formats.contains(f))
                    .collect();
                if d.is_empty() { formats.clone() } else { d }
            }
            ContentRequestKind::Music => Vec::new(),
        };
        Ok(ContentDetailDto {
            item,
            overview,
            requests,
            request_ability: ContentRequestAbilityDto {
                can_request: reason.is_none(),
                can_request_advanced: advanced_allowed(actor),
                formats,
                default_formats,
                reason,
            },
        })
    }
}

fn item(
    kind: ContentRequestKind,
    h: ContentHit,
    avail: &HashMap<String, Availability>,
    requests: &HashMap<String, Vec<ContentRow>>,
    briefs: &HashMap<i64, UserBriefDto>,
) -> ContentItemDto {
    let a = avail.get(&h.foreign_id).cloned().unwrap_or_default();
    let request = requests
        .get(&h.foreign_id)
        .and_then(|rows| best(rows))
        .map(|r| to_brief(r, briefs));
    ContentItemDto {
        kind,
        foreign_id: h.foreign_id,
        title: h.title,
        subtitle: h.subtitle,
        subtitle_id: h.subtitle_id,
        year: h.year,
        cover: h.cover,
        album_type: h.album_type,
        availability: a.status,
        library_id: a.library_id,
        request,
    }
}
