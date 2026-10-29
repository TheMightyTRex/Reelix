//! Dashboard tiles fed by `/library/stats` and `/calendar`.

use chrono::{Duration, Utc};
use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::library::{CalendarEntryDto, CalendarKind, LibraryStats, MediaKind};

use super::library_common::{bytes, on_entity_changed};
use super::poster::{POSTER_SM, Poster, tmdb_image};
use super::{EmptyState, Skeleton, StatCard, Tone};
use crate::api::{self, ApiResult};

fn load_stats() -> LocalResource<ApiResult<LibraryStats>> {
    let stats = LocalResource::new(|| api::get::<LibraryStats>("/library/stats"));
    on_entity_changed(
        &["movie", "series", "episode", "movie_file", "episode_file"],
        false,
        move || stats.refetch(),
    );
    stats
}

/// "Missing" and "Movies" / "Series" stat tiles.
#[component]
pub fn LibraryStatTiles() -> impl IntoView {
    let stats = load_stats();
    let get = move |f: fn(&LibraryStats) -> String| {
        Signal::derive(move || {
            stats
                .get()
                .and_then(Result::ok)
                .map(|s| f(&s))
                .unwrap_or_else(|| "–".into())
        })
    };
    view! {
        <StatCard icon="wanted" label="Missing" tone=Tone::Warning hint="Movies + episodes"
            value=get(|s| (s.movies.missing + s.series.missing_episodes).to_string()) />
        <StatCard icon="movies" label="Movies · Series" tone=Tone::Success
            value=get(|s| format!("{} · {}", s.movies.total, s.series.total)) />
    }
}

/// Next 7 days from the calendar.
#[component]
pub fn UpcomingList() -> impl IntoView {
    let upcoming = LocalResource::new(|| {
        let today = Utc::now().date_naive();
        let end = today + Duration::days(7);
        let url = format!(
            "/calendar?start={}&end={}",
            today.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );
        async move { api::get::<Vec<CalendarEntryDto>>(&url).await }
    });
    move || {
        match upcoming.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(_)) => view! { <EmptyState icon="calendar" title="Calendar unavailable" compact=true /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="calendar" title="Nothing scheduled" description="No monitored releases or airings this week." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <ul class="dash-list">
                {list.into_iter().take(8).map(|e| {
                    let href = match (e.movie_id, e.series_id) {
                        (Some(m), _) => format!("/movies/{m}"),
                        (_, Some(s)) => format!("/series/{s}"),
                        _ => "/calendar".into(),
                    };
                    let label = match (e.kind, e.season_number, e.episode_number) {
                        (CalendarKind::Episode, Some(s), Some(n)) => format!("{} S{s:02}E{n:02}", e.title),
                        _ => e.title.clone(),
                    };
                    view! {
                        <li>
                            <A href=href>{label}</A>
                            <span class="when">{e.date.format("%a %-d %b").to_string()}</span>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <A href="/calendar" attr:class="card-link">"Open calendar"</A>
        }
        .into_any(),
    }
    }
}

/// Newest additions as small posters, plus disk usage.
#[component]
pub fn RecentlyAdded() -> impl IntoView {
    let stats = load_stats();
    move || {
        match stats.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(_)) => view! { <EmptyState icon="movies" title="Library unavailable" compact=true /> }.into_any(),
        Some(Ok(s)) if s.recently_added.is_empty() => view! {
            <EmptyState icon="movies" title="Your library is empty" compact=true>
                <A href="/movies/add" attr:class="btn btn-sm btn-primary">"Add movie"</A>
                <A href="/series/add" attr:class="btn btn-sm btn-secondary">"Add series"</A>
            </EmptyState>
        }
        .into_any(),
        Some(Ok(s)) => view! {
            <div class="mini-posters">
                {s.recently_added.into_iter().take(8).map(|r| {
                    let href = match r.kind {
                        MediaKind::Movie => format!("/movies/{}", r.id),
                        MediaKind::Series => format!("/series/{}", r.id),
                    };
                    let title = format!("{}{}", r.title, r.year.map(|y| format!(" ({y})")).unwrap_or_default());
                    let tip = title.clone();
                    view! {
                        <A href=href attr:title=tip>
                            <Poster title=title url=tmdb_image(r.poster.as_deref(), POSTER_SM) />
                        </A>
                    }
                }).collect_view()}
            </div>
            <p class="field-hint">{format!("{} on disk", bytes(s.total_size_on_disk))}</p>
        }
        .into_any(),
    }
    }
}
