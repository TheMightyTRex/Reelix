//! `/media/movie/{tmdbId}` and `/media/tv/{tmdbId}` — the request view of a title.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use reelix_api_types::Permissions;
use reelix_api_types::requests::{AvailabilityStatus, DiscoverItemDto, MediaDetailDto, SeasonDto};
use reelix_core::MediaKind;

use crate::api;
use crate::components::media_card::{MediaBadge, MediaCard, MediaRow, tmdb_image};
use crate::components::request_modal::{ReportIssueModal, RequestModal};
use crate::components::request_status::{
    RequestStatusBadge, UserChip, availability_label, availability_tone, badge_for, item_badge, kind_label,
    media_href, refetch_on_change, request_status_label, request_status_tone,
};
use crate::components::{ErrorBox, Icon, Skeleton, StatusBadge};
use crate::format;
use crate::state::AuthState;

fn runtime(mins: u32) -> String {
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

fn date(d: Option<chrono::NaiveDate>) -> String {
    d.map(|d| d.format("%b %-d, %Y").to_string())
        .unwrap_or_else(|| "—".into())
}

#[component]
pub fn MediaDetailPage() -> impl IntoView {
    let params = use_params_map();
    let key = Memo::new(move |_| {
        params.with(|p| {
            let kind = p.get("kind").unwrap_or_default();
            let id = p.get("id").and_then(|i| i.parse::<u32>().ok());
            (kind, id)
        })
    });
    let detail = LocalResource::new(move || {
        let (kind, id) = key.get();
        async move {
            let seg = if kind == "tv" { "tv" } else { "movie" };
            match id {
                Some(id) => api::get::<MediaDetailDto>(&format!("/media/{seg}/{id}")).await,
                None => Err(api::ApiError {
                    status: 404,
                    code: "not_found".into(),
                    message: "That title doesn't exist.".into(),
                }),
            }
        }
    });
    refetch_on_change(&["request", "issue", "movie", "series"], move || detail.refetch());

    view! {
        {move || match detail.get() {
            None => view! {
                <div class="md-hero skeleton" aria-hidden="true"></div>
                <Skeleton lines=6 />
            }
            .into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| detail.refetch()) /> }.into_any(),
            Some(Ok(d)) => view! { <Detail d=d refetch=Callback::new(move |()| detail.refetch()) /> }.into_any(),
        }}
    }
}

#[component]
fn Detail(d: MediaDetailDto, refetch: Callback<()>) -> impl IntoView {
    let auth = AuthState::expect();
    let request_open = RwSignal::new(false);
    let issue_open = RwSignal::new(false);
    let detail_sig = Signal::stored(Some(d.clone()));

    let backdrop = tmdb_image(d.backdrop.as_deref(), "w1280");
    let poster = tmdb_image(d.poster.as_deref(), "w500");
    let year = d.year.map(|y| format!(" ({y})")).unwrap_or_default();
    let meta: Vec<String> = [
        Some(kind_label(d.kind).to_string()),
        d.certification.clone().filter(|c| !c.is_empty()),
        d.runtime_minutes.filter(|m| *m > 0).map(runtime),
        (!d.genres.is_empty()).then(|| {
            d.genres
                .iter()
                .map(|g| g.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        }),
        (!d.status.is_empty()).then(|| format::humanize_kind(&d.status)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let badge = badge_for(d.availability, d.requests.iter().find(|r| r.status.is_active()));
    let ability = d.request_ability.clone();
    let can_issue = auth.has(Permissions::CREATE_ISSUES)
        && matches!(
            d.availability,
            AvailabilityStatus::Available | AvailabilityStatus::PartiallyAvailable
        );
    let manage_href = auth
        .has(Permissions::MANAGE_LIBRARY)
        .then_some(())
        .and(d.library_id)
        .map(|id| match d.kind {
            MediaKind::Movie => format!("/movies/{id}"),
            MediaKind::Series => format!("/series/{id}"),
        });
    let tmdb_href = format!(
        "https://www.themoviedb.org/{}/{}",
        if d.kind == MediaKind::Movie { "movie" } else { "tv" },
        d.tmdb_id
    );
    let imdb_href = d
        .imdb_id
        .clone()
        .map(|i| format!("https://www.imdb.com/title/{i}/"));
    let crew = d
        .crew
        .iter()
        .filter(|c| {
            matches!(
                c.job.as_str(),
                "Director" | "Creator" | "Screenplay" | "Writer" | "Novel"
            )
        })
        .take(6)
        .cloned()
        .collect::<Vec<_>>();

    let request_button = if ability.can_request {
        view! {
            <button type="button" class="btn btn-primary btn-lg" on:click=move |_| request_open.set(true)>
                <Icon name="requests" />
                "Request"
            </button>
        }
        .into_any()
    } else {
        let reason = ability.reason.clone().unwrap_or_else(|| "Not requestable".into());
        view! {
            <button type="button" class="btn btn-secondary btn-lg" disabled title=reason.clone()>
                {reason.clone()}
            </button>
        }
        .into_any()
    };

    let release_rows = (d.kind == MediaKind::Movie).then(|| {
        view! {
            <dl class="kv">
                <div class="kv-row"><dt>"In cinemas"</dt><dd>{date(d.cinema_date.or(d.release_date))}</dd></div>
                <div class="kv-row"><dt>"Digital"</dt><dd>{date(d.digital_date)}</dd></div>
                <div class="kv-row"><dt>"Physical"</dt><dd>{date(d.physical_date)}</dd></div>
            </dl>
        }
    });
    let series_rows = (d.kind == MediaKind::Series).then(|| {
        view! {
            <dl class="kv">
                <div class="kv-row"><dt>"First aired"</dt><dd>{date(d.release_date)}</dd></div>
                <div class="kv-row"><dt>"Last aired"</dt><dd>{date(d.last_air_date)}</dd></div>
                <div class="kv-row"><dt>"Next episode"</dt><dd>{date(d.next_air_date)}</dd></div>
                <div class="kv-row"><dt>"Seasons"</dt><dd>{d.seasons.len()}</dd></div>
            </dl>
        }
    });
    let companies = (!d.companies.is_empty()).then(|| {
        let label = if d.kind == MediaKind::Movie {
            "Studios"
        } else {
            "Networks"
        };
        let feed = if d.kind == MediaKind::Movie {
            "studio"
        } else {
            "network"
        };
        view! {
            <div class="md-companies">
                <span class="field-label">{label}</span>
                <div class="chip-list">
                    {d.companies.iter().cloned().map(|c| view! {
                        <A href=format!("/discover/{feed}/{}", c.id) attr:class="pill-link">{c.name}</A>
                    }).collect_view()}
                </div>
            </div>
        }
    });

    let requests = (!d.requests.is_empty()).then(|| {
        view! {
            <section class="card">
                <header class="card-header"><h2 class="card-title">"Requests"</h2>
                    <A href="/requests" attr:class="card-link">"All requests"</A></header>
                <div class="card-body">
                    <ul class="request-mini-list">
                        {d.requests.iter().map(|r| {
                            let seasons = if r.seasons.is_empty() { String::new() } else {
                                format!("Seasons {}", r.seasons.iter().map(u32::to_string).collect::<Vec<_>>().join(", "))
                            };
                            view! {
                                <li class="request-mini">
                                    <UserChip user=r.requested_by.clone() />
                                    <span class="muted">{format::relative(r.created_at)}</span>
                                    {r.is_4k.then(|| view! { <span class="badge tone-accent">"4K"</span> })}
                                    <span class="muted">{seasons}</span>
                                    <RequestStatusBadge status=r.status />
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                </div>
            </section>
        }
    });

    let seasons = (d.kind == MediaKind::Series && !d.seasons.is_empty()).then(|| {
        view! {
            <section class="card">
                <header class="card-header"><h2 class="card-title">"Seasons"</h2></header>
                <div class="card-body">
                    <ul class="season-table">
                        {d.seasons.iter().cloned().map(|s| view! { <SeasonLine s=s /> }).collect_view()}
                    </ul>
                </div>
            </section>
        }
    });

    let cast = (!d.cast.is_empty()).then(|| {
        view! {
            <MediaRow title="Cast">
                {d.cast.iter().take(20).map(|c| {
                    let img = tmdb_image(c.profile_url.as_deref(), "w185");
                    let initials: String = c.name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect();
                    view! {
                        <div class="cast-card">
                            <div class="cast-photo">
                                {match img {
                                    Some(src) => view! { <img src=src alt="" loading="lazy" /> }.into_any(),
                                    None => view! { <span aria-hidden="true">{initials}</span> }.into_any(),
                                }}
                            </div>
                            <span class="cast-name">{c.name.clone()}</span>
                            {c.character.clone().map(|ch| view! { <span class="cast-role">{ch}</span> })}
                        </div>
                    }
                }).collect_view()}
            </MediaRow>
        }
    });

    let rec_row = |title: &'static str, items: Vec<DiscoverItemDto>| {
        (!items.is_empty()).then(|| {
            view! {
                <MediaRow title=title>
                    {items.into_iter().map(|item| view! {
                        <MediaCard
                            href=media_href(item.kind, item.tmdb_id)
                            title=item.title.clone()
                            subtitle=item.year.map(|y| y.to_string())
                            poster=item.poster.clone()
                            badge=item_badge(&item)
                        />
                    }).collect_view()}
                </MediaRow>
            }
        })
    };

    let on_requested = Callback::new(move |_| refetch.run(()));

    view! {
        <div class="md-page">
            <section class="md-hero">
                {backdrop.map(|src| view! { <img class="md-backdrop" src=src alt="" /> })}
                <div class="md-shade"></div>
                <div class="md-head">
                    <div class="md-poster">
                        {match poster {
                            Some(src) => view! { <img src=src alt=format!("{} poster", d.title) /> }.into_any(),
                            None => view! { <span class="media-poster-fallback" aria-hidden="true">{d.title.chars().next().map(String::from).unwrap_or_default()}</span> }.into_any(),
                        }}
                    </div>
                    <div class="md-info">
                        <div class="md-badges">
                            {badge.map(|b| view! { <MediaBadge kind=b large=true /> })}
                            {(badge.is_none() && d.availability != AvailabilityStatus::Unknown).then(|| view! {
                                <StatusBadge tone=availability_tone(d.availability) label=availability_label(d.availability) />
                            })}
                        </div>
                        <h1 class="md-title">{d.title.clone()}<span class="md-year">{year}</span></h1>
                        {d.tagline.clone().filter(|t| !t.is_empty()).map(|t| view! { <p class="md-tagline">{t}</p> })}
                        <p class="md-meta">{meta.join(" · ")}</p>
                        <div class="md-ratings">
                            {(d.vote_count > 0).then(|| view! {
                                <span class="rating" title=format!("{} votes on TMDB", d.vote_count)>
                                    <span class="rating-star"><Icon name="star" class="icon-fill" /></span>
                                    {format!("{:.1}", d.vote_average)}
                                    <span class="muted">" / 10 TMDB"</span>
                                </span>
                            })}
                        </div>
                        <div class="md-actions">
                            {request_button}
                            {d.trailer_url.clone().map(|u| view! {
                                <a class="btn btn-secondary btn-lg" href=u target="_blank" rel="noopener noreferrer">
                                    <Icon name="play" />
                                    "Trailer"
                                </a>
                            })}
                            {can_issue.then(|| view! {
                                <button type="button" class="btn btn-ghost btn-lg" on:click=move |_| issue_open.set(true)>
                                    <Icon name="issues" />
                                    "Report issue"
                                </button>
                            })}
                            {manage_href.map(|h| view! {
                                <A href=h attr:class="btn btn-ghost btn-lg">
                                    <Icon name="settings" />
                                    "Manage"
                                </A>
                            })}
                        </div>
                    </div>
                </div>
            </section>

            <div class="md-grid">
                <div class="stack">
                    <section class="card">
                        <header class="card-header"><h2 class="card-title">"Overview"</h2></header>
                        <div class="card-body">
                            <p class="md-overview">{d.overview.clone().unwrap_or_else(|| "No overview available.".into())}</p>
                            {(!crew.is_empty()).then(|| view! {
                                <ul class="crew-list">
                                    {crew.into_iter().map(|c| view! {
                                        <li><strong>{c.name}</strong><span class="muted">{c.job}</span></li>
                                    }).collect_view()}
                                </ul>
                            })}
                            {companies}
                        </div>
                    </section>
                    {seasons}
                    {requests}
                </div>
                <aside class="stack">
                    <section class="card">
                        <header class="card-header"><h2 class="card-title">"Details"</h2></header>
                        <div class="card-body">
                            {release_rows}
                            {series_rows}
                            <dl class="kv">
                                {d.original_title.clone().filter(|o| *o != d.title).map(|o| view! {
                                    <div class="kv-row"><dt>"Original title"</dt><dd>{o}</dd></div>
                                })}
                                {d.original_language.clone().map(|l| view! {
                                    <div class="kv-row"><dt>"Language"</dt><dd>{l.to_uppercase()}</dd></div>
                                })}
                                {d.collection.clone().map(|c| view! {
                                    <div class="kv-row"><dt>"Collection"</dt><dd>{c.name}</dd></div>
                                })}
                            </dl>
                            <div class="link-row">
                                <a href=tmdb_href target="_blank" rel="noopener noreferrer" class="card-link">
                                    "TMDB" <Icon name="external" />
                                </a>
                                {imdb_href.map(|h| view! {
                                    <a href=h target="_blank" rel="noopener noreferrer" class="card-link">"IMDb" <Icon name="external" /></a>
                                })}
                                {d.homepage.clone().filter(|h| h.starts_with("http")).map(|h| view! {
                                    <a href=h target="_blank" rel="noopener noreferrer" class="card-link">"Website" <Icon name="external" /></a>
                                })}
                            </div>
                        </div>
                    </section>
                </aside>
            </div>

            {cast}
            {rec_row("Recommendations", d.recommendations.clone())}
            {rec_row("Similar titles", d.similar.clone())}
        </div>
        <RequestModal open=request_open detail=detail_sig on_requested=on_requested />
        <ReportIssueModal open=issue_open detail=detail_sig />
    }
}

#[component]
fn SeasonLine(s: SeasonDto) -> impl IntoView {
    let name = s
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("Season {}", s.season_number));
    let state = match (s.availability, s.request_status) {
        (AvailabilityStatus::Available | AvailabilityStatus::PartiallyAvailable | AvailabilityStatus::Processing, _)
        | (_, None) => view! { <StatusBadge tone=availability_tone(s.availability) label=availability_label(s.availability) /> }.into_any(),
        (_, Some(r)) => view! { <StatusBadge tone=request_status_tone(r) label=request_status_label(r) /> }.into_any(),
    };
    view! {
        <li class="season-line">
            <span class="season-name">{name}</span>
            <span class="muted">{format!("{} episodes", s.episode_count)}</span>
            <span class="muted">{date(s.air_date)}</span>
            <span class="season-state">
                {state}
                {s.request_status_4k.map(|r| view! {
                    <StatusBadge tone=request_status_tone(r) label=format!("4K · {}", request_status_label(r)) />
                })}
            </span>
        </li>
    }
}
