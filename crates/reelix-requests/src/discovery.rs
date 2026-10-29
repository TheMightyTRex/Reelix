//! Discovery feeds, search and media details overlaid with library availability and
//! request state.

use std::collections::HashMap;

use reelix_api_types::requests::{
    AvailabilityStatus, CastDto, CollectionRefDto, CompanyDto, CrewDto, DiscoverItemDto, DiscoverPageDto,
    GenreDto, MediaDetailDto, PersonDto, RequestAbilityDto, RequestBriefDto, RequestStatus, SearchPageDto,
    SeasonDto, UserBriefDto, VideoDto,
};
use reelix_core::{MediaKind, Permissions};
use reelix_metadata::{Company, Credits, DiscoverFeed, Genre, MultiHit, Page, SearchHit, TimeWindow, Video};

use crate::gateway::Availability;
use crate::repo::{RequestRow, parse_kind, ts};
use crate::requests::{four_k_enabled, sees_all_requests};
use crate::{Actor, Error, RequestService, Result};

/// Parses a feed path (the part after `/discover/`), e.g. `trending`, `popular-movies`,
/// `genre/movie/28`, `network/213`, `studio/420`.
pub fn parse_feed(path: &str) -> Result<DiscoverFeed> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let num = |s: &str| {
        s.parse::<u32>()
            .map_err(|_| Error::Validation(format!("invalid id {s:?} in feed")))
    };
    let kind = |s: &str| parse_kind(s).ok_or_else(|| Error::Validation(format!("unknown media kind {s:?}")));
    Ok(match parts.as_slice() {
        ["trending"] => DiscoverFeed::Trending {
            kind: None,
            window: TimeWindow::Day,
        },
        ["trending-week"] => DiscoverFeed::Trending {
            kind: None,
            window: TimeWindow::Week,
        },
        ["trending-movies"] => DiscoverFeed::Trending {
            kind: Some(MediaKind::Movie),
            window: TimeWindow::Week,
        },
        ["trending-series"] => DiscoverFeed::Trending {
            kind: Some(MediaKind::Series),
            window: TimeWindow::Week,
        },
        ["popular-movies"] => DiscoverFeed::Popular {
            kind: MediaKind::Movie,
        },
        ["popular-series"] => DiscoverFeed::Popular {
            kind: MediaKind::Series,
        },
        ["top-rated-movies"] => DiscoverFeed::TopRated {
            kind: MediaKind::Movie,
        },
        ["top-rated-series"] => DiscoverFeed::TopRated {
            kind: MediaKind::Series,
        },
        ["upcoming"] => DiscoverFeed::Upcoming,
        ["now-playing"] => DiscoverFeed::NowPlaying,
        ["on-the-air"] => DiscoverFeed::OnTheAir,
        ["airing-today"] => DiscoverFeed::AiringToday,
        ["genre", k, id] => DiscoverFeed::Genre {
            kind: kind(k)?,
            genre_id: num(id)?,
        },
        ["network", id] => DiscoverFeed::Network { network_id: num(id)? },
        ["studio", id] => DiscoverFeed::Studio { company_id: num(id)? },
        ["keyword", k, id] => DiscoverFeed::Keyword {
            kind: kind(k)?,
            keyword_id: num(id)?,
        },
        _ => return Err(Error::NotFound(format!("unknown discover feed {path:?}"))),
    })
}

fn genre(g: Genre) -> GenreDto {
    GenreDto {
        id: g.id,
        name: g.name,
    }
}

fn company(c: Company) -> CompanyDto {
    CompanyDto {
        id: c.id,
        name: c.name,
        logo_url: c.logo_url,
    }
}

fn credits(c: Credits) -> (Vec<CastDto>, Vec<CrewDto>) {
    let cast = c
        .cast
        .into_iter()
        .take(30)
        .map(|m| CastDto {
            tmdb_id: m.tmdb_id,
            name: m.name,
            character: m.character,
            profile_url: m.profile_url,
        })
        .collect();
    const JOBS: [&str; 6] = [
        "Director",
        "Screenplay",
        "Writer",
        "Creator",
        "Producer",
        "Original Music Composer",
    ];
    let crew = c
        .crew
        .into_iter()
        .filter(|m| JOBS.contains(&m.job.as_str()))
        .take(12)
        .map(|m| CrewDto {
            tmdb_id: m.tmdb_id,
            name: m.name,
            job: m.job,
            profile_url: m.profile_url,
        })
        .collect();
    (cast, crew)
}

fn videos(v: Vec<Video>) -> (Vec<VideoDto>, Option<String>) {
    let trailer = v
        .iter()
        .filter(|x| x.kind == "Trailer" && x.site.eq_ignore_ascii_case("youtube"))
        .max_by_key(|x| x.official)
        .and_then(|x| x.url.clone());
    let list = v
        .into_iter()
        .map(|x| VideoDto {
            name: x.name,
            kind: x.kind,
            site: x.site,
            url: x.url,
        })
        .collect();
    (list, trailer)
}

/// Most relevant request: active ones first, then newest.
fn pick(rows: &[RequestRow], is_4k: bool, briefs: &HashMap<i64, UserBriefDto>) -> Option<RequestBriefDto> {
    rows.iter()
        .filter(|r| r.is_4k == is_4k)
        .max_by_key(|r| (r.status.is_active(), r.created_at))
        .map(|r| brief(r, briefs))
}

fn brief(r: &RequestRow, briefs: &HashMap<i64, UserBriefDto>) -> RequestBriefDto {
    RequestBriefDto {
        id: r.id,
        status: r.status,
        is_4k: r.is_4k,
        seasons: r.seasons.clone(),
        requested_by: briefs.get(&r.user_id).cloned().unwrap_or(UserBriefDto {
            id: r.user_id,
            username: "deleted".into(),
            display_name: "Deleted user".into(),
            avatar: None,
        }),
        created_at: ts(r.created_at),
    }
}

impl RequestService {
    /// Overlays availability and request state on metadata hits (adult titles dropped).
    pub async fn overlay(&self, actor: &Actor, hits: Vec<SearchHit>) -> Result<Vec<DiscoverItemDto>> {
        let hits: Vec<SearchHit> = hits
            .into_iter()
            .filter(|h| !h.adult && h.tmdb_id.is_some())
            .collect();
        let mut avail: HashMap<(MediaKind, u32), Availability> = HashMap::new();
        let mut reqs: HashMap<(MediaKind, u32), Vec<RequestRow>> = HashMap::new();
        for kind in [MediaKind::Movie, MediaKind::Series] {
            let ids: Vec<u32> = hits
                .iter()
                .filter(|h| h.kind == kind)
                .filter_map(|h| h.tmdb_id)
                .collect();
            if ids.is_empty() {
                continue;
            }
            match self.library.availability(kind, &ids).await {
                Ok(m) => avail.extend(m.into_iter().map(|(k, v)| ((kind, k), v))),
                Err(e) => tracing::warn!(error = %e, "library availability lookup failed"),
            }
            for (id, rows) in self.requests_by_tmdb(actor, kind, &ids).await? {
                reqs.insert((kind, id), rows);
            }
        }
        let users = reqs.values().flatten().map(|r| r.user_id);
        let briefs = self.users_repo().briefs(users).await?;
        Ok(hits
            .into_iter()
            .map(|h| {
                let id = h.tmdb_id.unwrap_or_default();
                let rows = reqs.get(&(h.kind, id)).map(Vec::as_slice).unwrap_or(&[]);
                DiscoverItemDto {
                    kind: h.kind,
                    tmdb_id: id,
                    tvdb_id: h.tvdb_id,
                    imdb_id: h.imdb_id,
                    title: h.title,
                    original_title: h.original_title,
                    year: h.year,
                    release_date: h.release_date,
                    overview: h.overview,
                    poster: h.poster,
                    backdrop: h.backdrop,
                    genre_ids: h.genre_ids,
                    original_language: h.original_language,
                    vote_average: h.vote_average,
                    vote_count: h.vote_count,
                    popularity: h.popularity,
                    availability: avail.get(&(h.kind, id)).map(|a| a.status).unwrap_or_default(),
                    request: pick(rows, false, &briefs),
                    request_4k: pick(rows, true, &briefs),
                }
            })
            .collect())
    }

    fn page_dto(p: Page<SearchHit>, results: Vec<DiscoverItemDto>) -> DiscoverPageDto {
        DiscoverPageDto {
            page: p.page,
            total_pages: p.total_pages,
            total_results: p.total_results,
            results,
        }
    }

    /// `GET /discover/{feed}`.
    pub async fn discover(&self, actor: &Actor, feed: &str, page: u32) -> Result<DiscoverPageDto> {
        let feed = parse_feed(feed)?;
        let mut p = self.metadata.discover(&feed, page.max(1)).await?;
        let hits = std::mem::take(&mut p.results);
        let mut items = self.overlay(actor, hits).await?;
        if self.request_settings().await?.hide_available {
            items.retain(|i| i.availability != AvailabilityStatus::Available);
        }
        Ok(Self::page_dto(p, items))
    }

    pub async fn genres(&self, kind: MediaKind) -> Result<Vec<GenreDto>> {
        Ok(self.metadata.genres(kind).await?.into_iter().map(genre).collect())
    }

    /// `GET /search?q=` (movies + series + people).
    pub async fn search(&self, actor: &Actor, q: &str, page: u32) -> Result<SearchPageDto> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(SearchPageDto {
                page: 1,
                total_pages: 0,
                total_results: 0,
                results: Vec::new(),
                people: Vec::new(),
            });
        }
        let p = self.metadata.search(q, page.max(1)).await?;
        let mut hits = Vec::new();
        let mut people = Vec::new();
        for r in p.results {
            match r {
                MultiHit::Media(h) => hits.push(h),
                MultiHit::Person(x) => people.push(PersonDto {
                    tmdb_id: x.tmdb_id,
                    name: x.name,
                    known_for_department: x.known_for_department,
                    profile_url: x.profile_url,
                }),
            }
        }
        Ok(SearchPageDto {
            page: p.page,
            total_pages: p.total_pages,
            total_results: p.total_results,
            results: self.overlay(actor, hits).await?,
            people,
        })
    }

    /// `GET /media/{kind}/{id}/recommendations|similar`.
    pub async fn related(
        &self,
        actor: &Actor,
        kind: MediaKind,
        id: u32,
        similar: bool,
        page: u32,
    ) -> Result<DiscoverPageDto> {
        let mut p = self.metadata.related(kind, id, similar, page.max(1)).await?;
        let hits = std::mem::take(&mut p.results);
        let items = self.overlay(actor, hits).await?;
        Ok(Self::page_dto(p, items))
    }

    /// `GET /media/movie/{id}` or `GET /media/tv/{id}`.
    pub async fn media_detail(&self, actor: &Actor, kind: MediaKind, tmdb_id: u32) -> Result<MediaDetailDto> {
        let settings = self.request_settings().await?;
        let availability = self
            .library
            .availability(kind, &[tmdb_id])
            .await
            .map_err(|e| Error::Upstream(format!("library: {e}")))?
            .remove(&tmdb_id)
            .unwrap_or_default();
        let rows = self
            .requests_by_tmdb(actor, kind, &[tmdb_id])
            .await?
            .remove(&tmdb_id)
            .unwrap_or_default();
        // Duplicate checks need every user's requests, not just the visible ones.
        let all_rows = if sees_all_requests(actor.permissions) {
            rows.clone()
        } else {
            self.requests_by_tmdb(&admin_view(actor), kind, &[tmdb_id])
                .await?
                .remove(&tmdb_id)
                .unwrap_or_default()
        };
        let briefs = self.users_repo().briefs(rows.iter().map(|r| r.user_id)).await?;
        let mut d = match kind {
            MediaKind::Movie => {
                let m = self.metadata.movie(tmdb_id).await?;
                let (cast, crew) = credits(m.credits);
                let (videos, trailer_url) = videos(m.videos);
                let recs = self.overlay(actor, m.recommendations).await?;
                let similar = self.overlay(actor, m.similar).await?;
                MediaDetailDto {
                    kind,
                    tmdb_id,
                    tvdb_id: None,
                    imdb_id: m.imdb_id,
                    title: m.title,
                    original_title: Some(m.original_title),
                    original_language: m.original_language,
                    overview: m.overview,
                    tagline: m.tagline,
                    year: m.year,
                    release_date: m.release_date,
                    last_air_date: None,
                    next_air_date: None,
                    cinema_date: m.release_dates.cinema,
                    digital_date: m.release_dates.digital,
                    physical_date: m.release_dates.physical,
                    status: enum_text(&m.status),
                    runtime_minutes: m.runtime_minutes,
                    certification: m.certification,
                    genres: m.genres.into_iter().map(genre).collect(),
                    companies: m.studios.into_iter().map(company).collect(),
                    poster: m.images.poster,
                    backdrop: m.images.backdrop,
                    logo: m.images.logo,
                    cast,
                    crew,
                    videos,
                    trailer_url,
                    vote_average: m.ratings.vote_average,
                    vote_count: m.ratings.vote_count,
                    homepage: m.homepage,
                    collection: m.collection.map(|c| CollectionRefDto {
                        tmdb_id: c.tmdb_id,
                        name: c.name,
                        poster: c.poster,
                        backdrop: c.backdrop,
                    }),
                    seasons: Vec::new(),
                    availability: availability.status,
                    library_id: availability.library_id,
                    requests: Vec::new(),
                    request_ability: ability_placeholder(),
                    recommendations: recs,
                    similar,
                }
            }
            MediaKind::Series => {
                let s = self.metadata.series(tmdb_id).await?;
                let (cast, crew) = credits(s.credits);
                let (videos, trailer_url) = videos(s.videos);
                let recs = self.overlay(actor, s.recommendations).await?;
                let similar = self.overlay(actor, s.similar).await?;
                let season_req = |n: u32, is_4k: bool| {
                    all_rows
                        .iter()
                        .filter(|r| r.is_4k == is_4k && r.status.is_active() && r.seasons.contains(&n))
                        .map(|r| r.status)
                        .max_by_key(|s| status_rank(*s))
                };
                let seasons = s
                    .seasons
                    .into_iter()
                    .filter(|x| x.season_number > 0)
                    .map(|x| SeasonDto {
                        season_number: x.season_number,
                        availability: availability.seasons.get(&x.season_number).copied().unwrap_or(
                            if availability.status == AvailabilityStatus::Available {
                                AvailabilityStatus::Available
                            } else {
                                AvailabilityStatus::Unknown
                            },
                        ),
                        request_status: season_req(x.season_number, false),
                        request_status_4k: season_req(x.season_number, true),
                        name: x.name,
                        overview: x.overview,
                        air_date: x.air_date,
                        poster: x.poster,
                        episode_count: x.episode_count.max(x.episodes.len() as u32),
                    })
                    .collect();
                MediaDetailDto {
                    kind,
                    tmdb_id,
                    tvdb_id: s.tvdb_id,
                    imdb_id: s.imdb_id,
                    title: s.title,
                    original_title: s.original_title,
                    original_language: s.original_language,
                    overview: s.overview,
                    tagline: None,
                    year: s.year,
                    release_date: s.first_air_date,
                    last_air_date: s.last_air_date,
                    next_air_date: s.next_air_date,
                    cinema_date: None,
                    digital_date: None,
                    physical_date: None,
                    status: enum_text(&s.status),
                    runtime_minutes: s.runtime_minutes,
                    certification: s.certification,
                    genres: s.genres.into_iter().map(genre).collect(),
                    companies: s.networks.into_iter().map(company).collect(),
                    poster: s.images.poster,
                    backdrop: s.images.backdrop,
                    logo: s.images.logo,
                    cast,
                    crew,
                    videos,
                    trailer_url,
                    vote_average: s.ratings.vote_average,
                    vote_count: s.ratings.vote_count,
                    homepage: None,
                    collection: None,
                    seasons,
                    availability: availability.status,
                    library_id: availability.library_id,
                    requests: Vec::new(),
                    request_ability: ability_placeholder(),
                    recommendations: recs,
                    similar,
                }
            }
        };
        d.requests = {
            let mut v: Vec<RequestBriefDto> = rows.iter().map(|r| brief(r, &briefs)).collect();
            v.sort_by_key(|b| std::cmp::Reverse(b.created_at));
            v
        };

        // What may the user do?
        let p = actor.permissions;
        let perm = match kind {
            MediaKind::Movie => Permissions::REQUEST_MOVIE,
            MediaKind::Series => Permissions::REQUEST_TV,
        };
        let mut reason = None;
        let open = match kind {
            MediaKind::Movie => {
                if availability.status == AvailabilityStatus::Available {
                    reason = Some("already available".to_owned());
                    false
                } else if all_rows.iter().any(|r| !r.is_4k && r.status.is_active()) {
                    reason = Some("already requested".to_owned());
                    false
                } else {
                    true
                }
            }
            MediaKind::Series => {
                let open = d
                    .seasons
                    .iter()
                    .any(|s| s.availability != AvailabilityStatus::Available && s.request_status.is_none());
                if !open {
                    reason = Some("every season is available or requested".to_owned());
                }
                open
            }
        };
        let mut can = p.has(perm) && open;
        if !p.has(perm) {
            reason = Some("your account cannot request this".to_owned());
        }
        if can && !p.has(Permissions::MANAGE_REQUESTS) {
            let me = self.user_record(actor.id).await?;
            let q = self.quota_of(&me, &settings).await?;
            let usage = match kind {
                MediaKind::Movie => q.movie,
                MediaKind::Series => q.tv,
            };
            if usage.restricted {
                can = false;
                reason = Some("request quota reached".to_owned());
            }
        }
        d.request_ability = RequestAbilityDto {
            can_request: can,
            can_request_4k: p.has(perm) && p.has(Permissions::REQUEST_4K) && four_k_enabled(&settings, kind),
            can_request_advanced: p.has(Permissions::REQUEST_ADVANCED) || p.has(Permissions::MANAGE_REQUESTS),
            partial_seasons: settings.partial_requests,
            reason: if can { None } else { reason },
        };
        Ok(d)
    }
}

fn ability_placeholder() -> RequestAbilityDto {
    RequestAbilityDto {
        can_request: false,
        can_request_4k: false,
        can_request_advanced: false,
        partial_seasons: false,
        reason: None,
    }
}

/// A copy of `actor` that sees all requests (internal duplicate checks only).
fn admin_view(actor: &Actor) -> Actor {
    let mut a = actor.clone();
    a.permissions |= Permissions::VIEW_REQUESTS;
    a
}

fn status_rank(s: RequestStatus) -> u8 {
    match s {
        RequestStatus::Available => 6,
        RequestStatus::PartiallyAvailable => 5,
        RequestStatus::Processing => 4,
        RequestStatus::Approved => 3,
        RequestStatus::Pending => 2,
        RequestStatus::Failed => 1,
        RequestStatus::Declined => 0,
    }
}

/// snake_case serde name of a unit enum value.
fn enum_text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeds_parse() {
        assert!(matches!(
            parse_feed("trending").unwrap(),
            DiscoverFeed::Trending { kind: None, .. }
        ));
        assert_eq!(
            parse_feed("popular-series").unwrap(),
            DiscoverFeed::Popular {
                kind: MediaKind::Series
            }
        );
        assert_eq!(
            parse_feed("genre/tv/18").unwrap(),
            DiscoverFeed::Genre {
                kind: MediaKind::Series,
                genre_id: 18
            }
        );
        assert_eq!(
            parse_feed("network/213").unwrap(),
            DiscoverFeed::Network { network_id: 213 }
        );
        assert_eq!(
            parse_feed("studio/2").unwrap(),
            DiscoverFeed::Studio { company_id: 2 }
        );
        assert!(matches!(parse_feed("nope"), Err(Error::NotFound(_))));
        assert!(matches!(parse_feed("genre/movie/x"), Err(Error::Validation(_))));
    }
}
