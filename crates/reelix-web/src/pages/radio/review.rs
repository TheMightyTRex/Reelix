//! `/radio/review` — what the Audiobookshelf podcast library has that Reelix doesn't cover:
//! podcasts no show maps to, and episode files matched to no episode.

use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::radio::RadioLibraryReviewDto;

use crate::api;
use crate::components::library_common::{bytes, on_entity_changed};
use crate::components::library_forms::{require_library, with_query};
use crate::components::radio::{RADIO_ENTITIES, length, rescan_library};
use crate::components::{Card, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};

#[component]
pub fn RadioReviewPage() -> impl IntoView {
    require_library();
    let review = LocalResource::new(|| api::get::<RadioLibraryReviewDto>("/radio/library/review"));
    on_entity_changed(RADIO_ENTITIES, true, move || review.refetch());

    let content = move || {
        let r = match review.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| review.refetch()) /> }
                    .into_any();
            }
            Some(Ok(r)) => r,
        };
        let synced = match r.synced_at {
            Some(t) => format!("Library last read {}.", crate::format::relative(t)),
            None => "The podcast library has not been read yet. Set the Audiobookshelf podcast library under Settings → Radio, then rescan.".into(),
        };
        let shows = r.unmatched_shows;
        let files = r.unmatched_files;
        let shows_view = if shows.is_empty() {
            view! { <EmptyState icon="check" title="Every podcast in the library is covered by a show" compact=true /> }.into_any()
        } else {
            view! {
                <div class="table-wrap">
                    <table class="table">
                        <caption class="visually-hidden">"Podcasts not covered by a show"</caption>
                        <thead>
                            <tr>
                                <th scope="col">"Podcast"</th>
                                <th scope="col">"Files"</th>
                                <th scope="col">"Audiobookshelf item"</th>
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            </tr>
                        </thead>
                        <tbody>
                            {shows.into_iter().map(|s| {
                                let title = s.title.clone().unwrap_or_default();
                                let add = with_query("/radio/add", &[("term", title.clone())]);
                                view! {
                                    <tr>
                                        <td class="cell-strong">{s.title.clone().unwrap_or_else(|| "Untitled".into())}</td>
                                        <td>{s.file_count}</td>
                                        <td class="mono">{s.abs_item_id.clone()}</td>
                                        <td class="cell-actions">
                                            {(!title.is_empty()).then(|| view! {
                                                <A href=add attr:class="btn btn-sm btn-secondary" attr:aria-label=format!("Find a show for {title}")>
                                                    <Icon name="search" />"Find show"
                                                </A>
                                            })}
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            }
            .into_any()
        };
        let files_view = if files.is_empty() {
            view! { <EmptyState icon="check" title="Every file is matched to an episode" compact=true /> }
                .into_any()
        } else {
            view! {
                <div class="table-wrap">
                    <table class="table">
                        <caption class="visually-hidden">"Files matching no episode"</caption>
                        <thead>
                            <tr>
                                <th scope="col">"Podcast"</th>
                                <th scope="col">"#"</th>
                                <th scope="col">"Title"</th>
                                <th scope="col">"Published"</th>
                                <th scope="col">"Length"</th>
                                <th scope="col">"Size"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {files.into_iter().map(|f| view! {
                                <tr>
                                    <td>{f.item_title.clone().unwrap_or_else(|| "—".into())}</td>
                                    <td class="mono nowrap">{match (f.season, f.episode) {
                                        (Some(s), Some(e)) => format!("S{s:02}E{e:02}"),
                                        (None, Some(e)) => format!("E{e:02}"),
                                        _ => "—".into(),
                                    }}</td>
                                    <td class="cell-strong">
                                        {f.title.clone().unwrap_or_else(|| "Untitled".into())}
                                        {f.path.clone().map(|p| view! { <span class="cell-sub mono">{p}</span> })}
                                    </td>
                                    <td class="nowrap">{f.published_at.map(crate::format::date).unwrap_or_else(|| "—".into())}</td>
                                    <td class="nowrap">{length(f.duration_secs)}</td>
                                    <td class="nowrap">{f.size.map(bytes).unwrap_or_else(|| "—".into())}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            }
            .into_any()
        };
        view! {
            <p class="muted" aria-live="polite">{synced}</p>
            <div class="stack">
                <Card title="Podcasts not covered by a show" subtitle="Items of the podcast library that no followed show maps to.">
                    {shows_view}
                </Card>
                <Card title="Files matching no episode" subtitle="Match one by hand from an episode's panel on its show page.">
                    {files_view}
                </Card>
            </div>
        }
        .into_any()
    };

    view! {
        <PageHeader title="Radio library review" subtitle="Compare the Audiobookshelf podcast library with the shows you follow.">
            <button type="button" class="btn btn-secondary" on:click=move |_| rescan_library()>
                <Icon name="refresh" />
                "Rescan library"
            </button>
        </PageHeader>
        {content}
    }
}
