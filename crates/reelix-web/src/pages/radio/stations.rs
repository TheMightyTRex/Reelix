//! `/radio/stations` — the stations and podcast networks of the saved shows.

use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::radio::RadioStationDto;

use crate::api;
use crate::components::library_common::on_entity_changed;
use crate::components::library_forms::require_library;
use crate::components::radio::{RADIO_ENTITIES, StationLogo};
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton};

#[component]
pub fn RadioStationsPage() -> impl IntoView {
    require_library();
    let stations = LocalResource::new(|| api::get::<Vec<RadioStationDto>>("/radio/stations"));
    on_entity_changed(RADIO_ENTITIES, false, move || stations.refetch());

    let content = move || {
        let mut list = match stations.get() {
            None => return view! { <Skeleton lines=5 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| stations.refetch()) /> }
                    .into_any();
            }
            Some(Ok(l)) => l,
        };
        if list.is_empty() {
            return view! {
                <div class="card">
                    <EmptyState icon="radio" title="No stations yet"
                        description="Stations appear here once you follow shows that name one.">
                        <A href="/radio/add" attr:class="btn btn-primary"><Icon name="plus" />"Add show"</A>
                    </EmptyState>
                </div>
            }
            .into_any();
        }
        list.sort_by_key(|s| s.name.to_lowercase());
        view! {
            <div class="card">
                <div class="card-body">
                    <div class="table-wrap">
                        <table class="table">
                            <caption class="visually-hidden">"Stations"</caption>
                            <thead>
                                <tr>
                                    <th scope="col"><span class="visually-hidden">"Logo"</span></th>
                                    <th scope="col">"Station"</th>
                                    <th scope="col">"Source"</th>
                                    <th scope="col">"Shows"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {list.into_iter().map(|s| {
                                    let href = format!("/radio?station={}", s.id);
                                    view! {
                                        <tr>
                                            <td class="station-logo-cell">
                                                {if s.has_logo {
                                                    view! { <StationLogo station_id=s.id name=s.name.clone() /> }.into_any()
                                                } else {
                                                    view! { <span class="station-logo-fallback" aria-hidden="true"><Icon name="radio" /></span> }.into_any()
                                                }}
                                            </td>
                                            <td class="cell-strong"><A href=href.clone()>{s.name.clone()}</A></td>
                                            <td>{s.provider.clone()}</td>
                                            <td><A href=href>{format!("{} show(s)", s.show_count)}</A></td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    view! {
        <PageHeader title="Stations" subtitle="Broadcasters' services and podcast networks of the shows you follow." />
        {content}
    }
}
