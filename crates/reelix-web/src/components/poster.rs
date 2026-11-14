//! Posters, backdrops and poster cards for the movie / series library.

use leptos::prelude::*;
use leptos_router::components::A;

/// TMDB image sizes used by the UI.
pub const POSTER_SM: &str = "w185";
pub const POSTER_MD: &str = "w342";
pub const BACKDROP: &str = "w1280";
pub const PROFILE: &str = "w185";

/// Turns a TMDB image path (`/abc.jpg`) or absolute URL into a URL served by `image.tmdb.org`.
pub fn tmdb_image(path: Option<&str>, size: &str) -> Option<String> {
    let path = path?.trim();
    if path.is_empty() {
        None
    } else if path.starts_with("http://") || path.starts_with("https://") {
        Some(path.to_string())
    } else if path.starts_with("/api/") {
        // A picture Reelix serves itself (an item added with its own details).
        Some(local_image(crate::api::base_path(), path))
    } else {
        let slash = if path.starts_with('/') { "" } else { "/" };
        Some(format!("https://image.tmdb.org/t/p/{size}{slash}{path}"))
    }
}

/// A Reelix-served image path under the UI's URL base.
fn local_image(base: &str, path: &str) -> String {
    format!("{base}{path}")
}

/// An image address as given by the API, with a picture Reelix serves itself (`/api/v1/…`, an
/// item added with its own details) put under the UI's URL base. Other addresses, and ones
/// already under the base, are kept as they are.
fn under_base(base: &str, url: String) -> String {
    if url.starts_with("/api/v1/") {
        local_image(base, &url)
    } else {
        url
    }
}

/// Up to two initials for an image placeholder.
fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// A 2:3 poster image with a lazy load and a lettered placeholder when there's no artwork.
#[component]
pub fn Poster(
    #[prop(into)] title: String,
    #[prop(into)] url: Option<String>,
    #[prop(optional, into)] class: Option<String>,
) -> impl IntoView {
    let failed = RwSignal::new(false);
    // Book and music pages pass the API's address as it is.
    let url = url.map(|u| under_base(crate::api::base_path(), u));
    let class = format!("poster {}", class.unwrap_or_default());
    let letters = initials(&title);
    let alt = format!("Poster for {title}");
    view! {
        <div class=class>
            {move || match url.clone().filter(|_| !failed.get()) {
                Some(src) => view! {
                    <img src=src alt=alt.clone() loading="lazy" decoding="async" on:error=move |_| failed.set(true) />
                }
                .into_any(),
                None => view! {
                    <div class="poster-fallback" role="img" aria-label=alt.clone()>
                        <span>{letters.clone()}</span>
                    </div>
                }
                .into_any(),
            }}
        </div>
    }
}

/// Visual state shown as a coloured strip / dot on posters and calendar entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaState {
    /// Has a file that meets the cutoff.
    Downloaded,
    /// Has a file but wants an upgrade.
    Upgrade,
    /// Monitored, released, no file.
    Missing,
    /// Monitored, not released / aired yet.
    Upcoming,
    /// Not monitored.
    Unmonitored,
    /// Some but not all episodes present (series).
    Partial,
}

impl MediaState {
    pub fn class(self) -> &'static str {
        match self {
            Self::Downloaded => "state-downloaded",
            Self::Upgrade => "state-upgrade",
            Self::Missing => "state-missing",
            Self::Upcoming => "state-upcoming",
            Self::Unmonitored => "state-unmonitored",
            Self::Partial => "state-partial",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Downloaded => "Downloaded",
            Self::Upgrade => "Cutoff unmet",
            Self::Missing => "Missing",
            Self::Upcoming => "Upcoming",
            Self::Unmonitored => "Unmonitored",
            Self::Partial => "Partial",
        }
    }

    pub fn tone(self) -> crate::components::Tone {
        use crate::components::Tone;
        match self {
            Self::Downloaded => Tone::Success,
            Self::Upgrade => Tone::Warning,
            Self::Missing => Tone::Danger,
            Self::Upcoming => Tone::Info,
            Self::Unmonitored => Tone::Neutral,
            Self::Partial => Tone::Accent,
        }
    }
}

/// Have/total bar, e.g. episodes on disk vs monitored episodes.
#[component]
pub fn EpisodeProgress(have: u32, total: u32, #[prop(optional)] compact: bool) -> impl IntoView {
    let pct = (have.min(total) * 100).checked_div(total).unwrap_or(0);
    let tone = if total == 0 {
        "tone-neutral"
    } else if have >= total {
        "tone-success"
    } else if have == 0 {
        "tone-danger"
    } else {
        "tone-warning"
    };
    let label = format!("{have} / {total}");
    let aria = format!("{label} episodes");
    view! {
        <div class=format!("ep-progress {tone}") class:compact=compact role="progressbar" aria-valuemin="0"
            aria-valuemax=total aria-valuenow=have aria-label=aria>
            <div class="ep-progress-bar" style=format!("width: {pct}%")></div>
            <span class="ep-progress-label">{label}</span>
        </div>
    }
}

/// A poster tile linking to a detail page. In select mode a click toggles selection instead.
#[component]
pub fn PosterCard(
    #[prop(into)] href: String,
    #[prop(into)] title: String,
    #[prop(optional, into)] subtitle: Option<String>,
    #[prop(into)] poster: Option<String>,
    state: MediaState,
    #[prop(optional)] monitored: Option<bool>,
    /// `(have, total)` episode counts for series.
    #[prop(optional)]
    progress: Option<(u32, u32)>,
    #[prop(optional, into)] badge: Option<String>,
    /// When set, the card is selectable and this signal says whether it is selected.
    #[prop(optional, into)]
    selected: Option<Signal<bool>>,
    #[prop(optional)] on_toggle: Option<Callback<()>>,
    #[prop(optional, into)] select_mode: Signal<bool>,
) -> impl IntoView {
    let is_selected = move || selected.is_some_and(|s| s.get());
    let label = title.clone();
    let body = move || {
        let title = title.clone();
        view! {
            <div class="poster-wrap">
                <Poster title=title.clone() url=poster.clone() />
                <span class=format!("poster-state {}", state.class()) title=state.label()><span class="visually-hidden">{state.label()}</span></span>
                {badge.clone().map(|b| view! { <span class="poster-badge">{b}</span> })}
                {monitored.map(|m| view! {
                    <span class="poster-monitor" class:off=!m title=if m { "Monitored" } else { "Unmonitored" }>
                        <span class="visually-hidden">{if m { "Monitored" } else { "Unmonitored" }}</span>
                    </span>
                })}
                <Show when=move || select_mode.get()>
                    <span class="poster-check" class:on=is_selected aria-hidden="true">"✓"</span>
                </Show>
            </div>
            <div class="poster-meta">
                <span class="poster-title" title=title.clone()>{title.clone()}</span>
                {subtitle.clone().map(|s| view! { <span class="poster-sub">{s}</span> })}
                {progress.map(|(h, t)| view! { <EpisodeProgress have=h total=t compact=true /> })}
            </div>
        }
    };
    let body = StoredValue::new(body);
    view! {
        <div class="poster-card" class:selected=is_selected class:selecting=move || select_mode.get()>
            <Show
                when=move || select_mode.get()
                fallback=move || {
                    let href = href.clone();
                    view! { <A href=href attr:class="poster-link">{body.with_value(|b| b())}</A> }
                }
            >
                <button
                    type="button"
                    class="poster-link poster-select"
                    aria-pressed=move || is_selected().to_string()
                    aria-label=format!("Select {label}")
                    on:click=move |_| {
                        if let Some(cb) = on_toggle {
                            cb.run(());
                        }
                    }
                >
                    {body.with_value(|b| b())}
                </button>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_urls() {
        assert_eq!(
            tmdb_image(Some("/abc.jpg"), POSTER_SM).as_deref(),
            Some("https://image.tmdb.org/t/p/w185/abc.jpg")
        );
        assert_eq!(
            tmdb_image(Some("https://image.tmdb.org/t/p/original/x.jpg"), POSTER_SM).as_deref(),
            Some("https://image.tmdb.org/t/p/original/x.jpg")
        );
        assert_eq!(tmdb_image(Some("  "), POSTER_SM), None);
        assert_eq!(tmdb_image(None, POSTER_SM), None);
        assert_eq!(
            local_image("/reelix", "/api/v1/customitems/movie/1/image?v=ab"),
            "/reelix/api/v1/customitems/movie/1/image?v=ab"
        );
        assert_eq!(
            under_base("/reelix", "/api/v1/customitems/author/2/image?v=cd".into()),
            "/reelix/api/v1/customitems/author/2/image?v=cd"
        );
        // Already under the base (film and series pages), or another site's address: unchanged.
        assert_eq!(
            under_base("/reelix", "/reelix/api/v1/customitems/movie/1/image?v=ab".into()),
            "/reelix/api/v1/customitems/movie/1/image?v=ab"
        );
        assert_eq!(
            under_base("/api", "/api/api/v1/customitems/movie/1/image".into()),
            "/api/api/v1/customitems/movie/1/image"
        );
        assert_eq!(
            under_base("/reelix", "https://covers.openlibrary.org/a/id/1-L.jpg".into()),
            "https://covers.openlibrary.org/a/id/1-L.jpg"
        );
        assert_eq!(
            under_base("", "/api/v1/customitems/artist/3/image".into()),
            "/api/v1/customitems/artist/3/image"
        );
        assert_eq!(initials("the matrix reloaded"), "TM");
    }
}
