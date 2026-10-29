//! `/music/add` — search MusicBrainz for artists or single albums and add them.

use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_query_map;
use reelix_api_types::music::{AlbumLookupDto, ArtistLookupDto};

use crate::api;
use crate::components::library_common::Segmented;
use crate::components::library_forms::{enc, require_library};
use crate::components::music::{
    AddAlbumModal, AddArtistModal, MusicOptions, album_type_label, secondary_label,
};
use crate::components::poster::Poster;
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton};

#[derive(Clone)]
enum Results {
    Artists(Vec<ArtistLookupDto>),
    Albums(Vec<AlbumLookupDto>),
}

impl Results {
    fn is_empty(&self) -> bool {
        match self {
            Self::Artists(a) => a.is_empty(),
            Self::Albums(b) => b.is_empty(),
        }
    }
}

#[component]
pub fn AddArtistPage() -> impl IntoView {
    require_library();
    let query = use_query_map();
    let initial = query.with_untracked(|q| q.get("term").unwrap_or_default());
    let input = RwSignal::new(initial.clone());
    let term = RwSignal::new(initial);
    let what = RwSignal::new("artists".to_string());
    let opts = MusicOptions::load();
    let artist_target = RwSignal::new(None::<ArtistLookupDto>);
    let artist_open = RwSignal::new(false);
    let album_target = RwSignal::new(None::<AlbumLookupDto>);
    let album_open = RwSignal::new(false);

    let pending = StoredValue::new(0u32);
    let on_input = move |e: ev::Event| {
        let v = event_target_value(&e);
        input.set(v.clone());
        let n = pending.get_value() + 1;
        pending.set_value(n);
        // MusicBrainz allows one request per second: wait for a pause in typing.
        set_timeout(
            move || {
                if pending.try_get_value() == Some(n) {
                    let _ = term.try_set(v.trim().to_string());
                }
            },
            Duration::from_millis(600),
        );
    };

    let results = LocalResource::new(move || {
        let t = term.get();
        let w = what.get();
        async move {
            if t.trim().len() < 2 {
                return None;
            }
            let q = enc(&t);
            Some(if w == "albums" {
                api::get::<Vec<AlbumLookupDto>>(&format!("/albums/lookup?term={q}"))
                    .await
                    .map(Results::Albums)
            } else {
                api::get::<Vec<ArtistLookupDto>>(&format!("/artists/lookup?term={q}"))
                    .await
                    .map(Results::Artists)
            })
        }
    });

    let list = move || {
        match results.get() {
        None => view! { <Skeleton lines=4 /> }.into_any(),
        Some(None) => view! {
            <div class="card">
                <EmptyState icon="search" title="Find an artist or an album"
                    description="Results come from MusicBrainz. Use mbid:… to look up a MusicBrainz id directly." />
            </div>
        }
        .into_any(),
        Some(Some(Err(e))) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| results.refetch()) /> }.into_any(),
        Some(Some(Ok(r))) if r.is_empty() => view! {
            <div class="card">
                <EmptyState icon="search" title="No matches" description="Try a different spelling or a MusicBrainz id." />
            </div>
        }
        .into_any(),
        Some(Some(Ok(Results::Artists(list)))) => view! {
            <div class="lookup-list">
                {list.into_iter().map(|a| {
                    let action = match a.artist_id {
                        Some(id) => view! {
                            <A href=format!("/music/artists/{id}") attr:class="btn btn-sm btn-secondary"><Icon name="check" />"In library"</A>
                        }.into_any(),
                        None => {
                            let a2 = a.clone();
                            view! {
                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| {
                                    artist_target.set(Some(a2.clone()));
                                    artist_open.set(true);
                                }>
                                    <Icon name="plus" />
                                    "Add"
                                </button>
                            }.into_any()
                        }
                    };
                    view! {
                        <article class="lookup-item">
                            <Poster title=a.name.clone() url=a.image.clone() />
                            <div class="overview-body">
                                <span class="overview-title">{a.name.clone()}</span>
                                <div class="meta-line">
                                    {a.artist_type.clone().map(|t| view! { <span>{t}</span> })}
                                    {a.country.clone().map(|c| view! { <span>{c}</span> })}
                                    {a.disambiguation.clone().map(|d| view! { <span>{d}</span> })}
                                    <span class="mono">{a.musicbrainz_id.clone()}</span>
                                </div>
                            </div>
                            <div>{action}</div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
        Some(Some(Ok(Results::Albums(list)))) => view! {
            <div class="lookup-list">
                {list.into_iter().map(|b| {
                    let action = match b.album_id {
                        Some(id) => view! {
                            <A href=format!("/music/albums/{id}") attr:class="btn btn-sm btn-secondary"><Icon name="check" />"In library"</A>
                        }.into_any(),
                        None => {
                            let b2 = b.clone();
                            view! {
                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| {
                                    album_target.set(Some(b2.clone()));
                                    album_open.set(true);
                                }>
                                    <Icon name="plus" />
                                    "Add"
                                </button>
                            }.into_any()
                        }
                    };
                    let kind = std::iter::once(album_type_label(b.album_type))
                        .chain(b.secondary_types.iter().map(|t| secondary_label(*t)))
                        .collect::<Vec<_>>()
                        .join(" · ");
                    view! {
                        <article class="lookup-item">
                            <Poster title=b.title.clone() url=b.cover.clone() />
                            <div class="overview-body">
                                <span class="overview-title">{b.title.clone()}{b.year.map(|y| format!(" ({y})"))}</span>
                                <div class="meta-line">
                                    <span>{format!("by {}", b.artist_name)}</span>
                                    <span>{kind}</span>
                                </div>
                            </div>
                            <div>{action}</div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Add music" subtitle="Search MusicBrainz and add an artist (with their albums) or a single album." />
        <div class="lib-toolbar">
            <div class="search add-search grow">
                <label for="music-add-term" class="visually-hidden">"Search"</label>
                <input id="music-add-term" class="input grow" type="search" placeholder="Artist name, album title, or mbid:…" autofocus
                    prop:value=move || input.get() on:input=on_input
                    on:keydown=move |e: ev::KeyboardEvent| if e.key() == "Enter" { term.set(input.get_untracked().trim().to_string()) } />
            </div>
            <Segmented value=what options=vec![("artists", "Artists"), ("albums", "Albums")] label="Search for" />
        </div>
        {list}
        <AddArtistModal open=artist_open target=artist_target opts=opts />
        <AddAlbumModal open=album_open target=album_target opts=opts />
    }
}
