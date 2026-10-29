//! `/settings/metadataprofiles` — which release-group types (albums, EPs, singles, live, …)
//! and release statuses an artist gets.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::music::{AlbumType, MetadataProfileDto, ReleaseStatus, SecondaryAlbumType};

use crate::api;
use crate::components::music::{
    ALBUM_TYPES, RELEASE_STATUSES, SECONDARY_TYPES, album_type_label, secondary_label, status_label,
};
use crate::components::{
    Card, ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, Spinner,
};
use crate::pages::system::require_admin;
use crate::state::Toasts;

fn blank() -> MetadataProfileDto {
    MetadataProfileDto {
        id: 0,
        name: String::new(),
        primary_types: vec![AlbumType::Album, AlbumType::Ep],
        secondary_types: Vec::new(),
        release_statuses: vec![ReleaseStatus::Official],
    }
}

#[component]
pub fn MetadataProfilesPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let profiles = LocalResource::new(|| api::get::<Vec<MetadataProfileDto>>("/metadataprofiles"));
    let editing = RwSignal::new(None::<MetadataProfileDto>);
    let edit_open = RwSignal::new(false);
    let to_delete = RwSignal::new(None::<MetadataProfileDto>);
    let delete_open = RwSignal::new(false);

    let delete = Callback::new(move |()| {
        let Some(p) = to_delete.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/metadataprofiles/{}", p.id)).await {
                Ok(()) => {
                    toasts.success("Profile deleted.");
                    profiles.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let list = move || {
        match profiles.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| profiles.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="music" title="No metadata profiles" description="Create one to choose which album types artists get." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">"Metadata profiles"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"Name"</th>
                            <th scope="col">"Primary types"</th>
                            <th scope="col">"Secondary types"</th>
                            <th scope="col">"Release statuses"</th>
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {list.into_iter().map(|p| {
                            let (p2, p3) = (p.clone(), p.clone());
                            view! {
                                <tr>
                                    <td class="cell-strong">{p.name.clone()}</td>
                                    <td>{p.primary_types.iter().map(|t| album_type_label(*t)).collect::<Vec<_>>().join(", ")}</td>
                                    <td>{if p.secondary_types.is_empty() { "None (studio releases only)".to_string() } else {
                                        p.secondary_types.iter().map(|t| secondary_label(*t)).collect::<Vec<_>>().join(", ")
                                    }}</td>
                                    <td>{p.release_statuses.iter().map(|s| status_label(*s)).collect::<Vec<_>>().join(", ")}</td>
                                    <td class="cell-actions">
                                        <button type="button" class="btn btn-sm btn-ghost" aria-label=format!("Edit {}", p.name)
                                            on:click=move |_| { editing.set(Some(p2.clone())); edit_open.set(true); }>
                                            "Edit"
                                        </button>
                                        <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label=format!("Delete {}", p.name)
                                            on:click=move |_| { to_delete.set(Some(p3.clone())); delete_open.set(true); }>
                                            <Icon name="trash" />
                                        </button>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Metadata profiles" subtitle="Which MusicBrainz release groups are added to an artist: primary types, allowed secondary types and release statuses.">
            <button type="button" class="btn btn-primary" on:click=move |_| { editing.set(Some(blank())); edit_open.set(true); }>
                <Icon name="plus" />
                "New profile"
            </button>
        </PageHeader>
        <Card>{list}</Card>
        <ProfileEditor open=edit_open profile=editing on_saved=Callback::new(move |()| profiles.refetch()) />
        <ConfirmDialog open=delete_open title="Delete metadata profile?"
            message=Signal::derive(move || to_delete.get().map(|p| format!("“{}” is deleted. Profiles still used by artists can't be deleted.", p.name)).unwrap_or_default())
            confirm_label="Delete" on_confirm=delete />
    }
}

fn toggle<T: PartialEq + Copy>(v: &mut Vec<T>, x: T, on: bool) {
    v.retain(|y| *y != x);
    if on {
        v.push(x);
    }
}

#[component]
fn ProfileEditor(
    open: RwSignal<bool>,
    profile: RwSignal<Option<MetadataProfileDto>>,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let busy = RwSignal::new(false);
    let edit = move |f: &dyn Fn(&mut MetadataProfileDto)| {
        profile.update(|p| {
            if let Some(p) = p.as_mut() {
                f(p)
            }
        })
    };
    let has_primary =
        move |t: AlbumType| profile.with(|p| p.as_ref().is_some_and(|p| p.primary_types.contains(&t)));
    let has_secondary = move |t: SecondaryAlbumType| {
        profile.with(|p| p.as_ref().is_some_and(|p| p.secondary_types.contains(&t)))
    };
    let has_status =
        move |s: ReleaseStatus| profile.with(|p| p.as_ref().is_some_and(|p| p.release_statuses.contains(&s)));

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(p) = profile.get_untracked() else { return };
        busy.set(true);
        spawn_local(async move {
            let res = if p.id == 0 {
                api::post::<_, MetadataProfileDto>("/metadataprofiles", &p).await
            } else {
                api::put::<_, MetadataProfileDto>(&format!("/metadataprofiles/{}", p.id), &p).await
            };
            busy.set(false);
            match res {
                Ok(_) => {
                    toasts.success("Profile saved.");
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    view! {
        <Modal open=open title="Metadata profile">
            <form class="form" on:submit=submit>
                <div class="field">
                    <label for="mp-name">"Name"</label>
                    <input id="mp-name" class="input" type="text" required
                        prop:value=move || profile.with(|p| p.as_ref().map(|p| p.name.clone()).unwrap_or_default())
                        on:input=move |e| { let v = event_target_value(&e); edit(&|p| p.name = v.clone()); } />
                </div>
                <fieldset class="field">
                    <legend class="field-label">"Primary types"</legend>
                    <div class="chip-list">
                        {ALBUM_TYPES.iter().map(|&(t, l)| view! {
                            <label class="checkbox">
                                <input type="checkbox" prop:checked=move || has_primary(t)
                                    on:change=move |e| { let on = event_target_checked(&e); edit(&|p| toggle(&mut p.primary_types, t, on)); } />
                                {l}
                            </label>
                        }).collect_view()}
                    </div>
                </fieldset>
                <fieldset class="field">
                    <legend class="field-label">"Allowed secondary types"</legend>
                    <span class="field-hint">"Release groups with any other secondary type (e.g. Live, Compilation) are skipped."</span>
                    <div class="chip-list">
                        {SECONDARY_TYPES.iter().map(|&(t, l)| view! {
                            <label class="checkbox">
                                <input type="checkbox" prop:checked=move || has_secondary(t)
                                    on:change=move |e| { let on = event_target_checked(&e); edit(&|p| toggle(&mut p.secondary_types, t, on)); } />
                                {l}
                            </label>
                        }).collect_view()}
                    </div>
                </fieldset>
                <fieldset class="field">
                    <legend class="field-label">"Release statuses"</legend>
                    <div class="chip-list">
                        {RELEASE_STATUSES.iter().map(|&(s, l)| view! {
                            <label class="checkbox">
                                <input type="checkbox" prop:checked=move || has_status(s)
                                    on:change=move |e| { let on = event_target_checked(&e); edit(&|p| toggle(&mut p.release_statuses, s, on)); } />
                                {l}
                            </label>
                        }).collect_view()}
                    </div>
                </fieldset>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
