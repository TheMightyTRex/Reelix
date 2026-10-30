//! `/settings/profiles` — quality profiles (ordering, groups, allowed qualities, cutoff,
//! custom-format scores), delay profiles and release profiles.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::profiles::{
    CustomFormat, DelayProfile, FormatItem, Language, ProfileItem, Quality, QualityFamily, QualityProfile,
    ReleaseProfile,
};
use reelix_core::Protocol;

use crate::api;
use crate::components::{
    Card, ConfirmDialog, DataTable, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton,
};
use crate::pages::system::require_admin;
use crate::state::Toasts;

const GROUP_ID_BASE: u32 = 1000;

fn parse_i32(s: &str) -> i32 {
    s.trim().parse().unwrap_or(0)
}

/// "Size: 1–15 GB" style summary of a profile's limits.
fn size_summary(min: Option<u64>, max: Option<u64>, family: QualityFamily) -> Option<String> {
    let unit = if size_in_gb(family) { "GB" } else { "MB" };
    let t = |v| size_text(Some(v), family);
    Some(match (min, max) {
        (None, None) => return None,
        (Some(a), Some(b)) => format!("Size: {}–{} {unit}", t(a), t(b)),
        (Some(a), None) => format!("Size: at least {} {unit}", t(a)),
        (None, Some(b)) => format!("Size: at most {} {unit}", t(b)),
    })
}

/// Size limits are edited in GB for video and in MB for books and music.
fn size_in_gb(family: QualityFamily) -> bool {
    family == QualityFamily::Video
}

/// A size limit (MB) as the editor shows it; empty = no limit.
fn size_text(mb: Option<u64>, family: QualityFamily) -> String {
    match mb {
        None => String::new(),
        Some(mb) if size_in_gb(family) => {
            let gb = format!("{:.2}", mb as f64 / 1024.0);
            gb.trim_end_matches('0').trim_end_matches('.').to_string()
        }
        Some(mb) => mb.to_string(),
    }
}

/// Parses an editor size back to MB; empty, zero or invalid = no limit.
fn parse_size(s: &str, family: QualityFamily) -> Option<u64> {
    let v: f64 = s.trim().replace(',', ".").parse().ok()?;
    let mb = if size_in_gb(family) { v * 1024.0 } else { v };
    (mb.is_finite() && mb >= 1.0).then(|| mb.round() as u64)
}

/// Moves `items[from]` to index `to`.
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from < items.len() && to < items.len() && from != to {
        let it = items.remove(from);
        items.insert(to, it);
    }
}

/// Merges internal rows `i` and `i - 1` into one group (at `i - 1`).
fn merge_down(items: &mut Vec<ProfileItem>, i: usize) {
    if i == 0 || i >= items.len() {
        return;
    }
    let upper = items.remove(i);
    let lower = items.remove(i - 1);
    let members = |it: &ProfileItem| -> Vec<Quality> {
        if it.items.is_empty() {
            it.quality.into_iter().collect()
        } else {
            it.items.clone()
        }
    };
    let mut qs = members(&lower);
    qs.extend(members(&upper));
    let id = lower.id.or(upper.id).unwrap_or_else(|| {
        items
            .iter()
            .filter_map(|x| x.id)
            .max()
            .map_or(GROUP_ID_BASE, |m| m + 1)
            .max(GROUP_ID_BASE)
    });
    let name = match (&lower.name, &upper.name) {
        (Some(n), _) | (None, Some(n)) => n.clone(),
        _ => qs.iter().map(|q| q.name()).collect::<Vec<_>>().join(" / "),
    };
    items.insert(
        i - 1,
        ProfileItem::group(id, &name, qs, lower.allowed || upper.allowed),
    );
}

/// Splits group row `i` back into single qualities.
fn ungroup(items: &mut Vec<ProfileItem>, i: usize) {
    let Some(g) = items.get(i).cloned() else { return };
    if g.items.is_empty() {
        return;
    }
    items.remove(i);
    for (k, q) in g.items.iter().enumerate() {
        items.insert(i + k, ProfileItem::quality(*q, g.allowed));
    }
}

/// Tab label of a quality family.
pub(crate) fn family_label(f: QualityFamily) -> &'static str {
    match f {
        QualityFamily::Video => "Video",
        QualityFamily::Ebook => "Ebook",
        QualityFamily::Audiobook => "Audiobook",
        QualityFamily::Music => "Music",
    }
}

/// `path` for one family: video keeps the plain path, other families add `?kind=`.
pub(crate) fn family_path(path: &str, f: QualityFamily) -> String {
    match f {
        QualityFamily::Video => path.to_string(),
        f => format!("{path}?kind={}", f.as_str()),
    }
}

/// Tab bar choosing a quality family (Video | Ebook | Audiobook | Music).
#[component]
pub(crate) fn FamilyTabs(family: RwSignal<QualityFamily>, #[prop(into)] label: String) -> impl IntoView {
    view! {
        <div class="tabs" role="tablist" aria-label=label>
            {QualityFamily::ALL
                .into_iter()
                .map(|f| {
                    view! {
                        <button type="button" role="tab" class="tab" class:active=move || family.get() == f
                            aria-selected=move || (family.get() == f).to_string()
                            on:click=move |_| family.set(f)>
                            {family_label(f)}
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
}

#[component]
pub fn ProfilesPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Profiles" subtitle="Which releases are wanted, how long to wait for them and which terms they must (not) contain." />
        <div class="stack">
            <QualityProfilesCard />
            <DelayProfilesCard />
            <ReleaseProfilesCard />
        </div>
    }
}

// ---------------- quality profiles ----------------

#[component]
fn QualityProfilesCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let family = RwSignal::new(QualityFamily::Video);
    let profiles = LocalResource::new(move || {
        let path = family_path("/qualityprofiles", family.get());
        async move { api::get::<Vec<QualityProfile>>(&path).await }
    });
    let formats = LocalResource::new(|| api::get::<Vec<CustomFormat>>("/customformats"));

    let open = RwSignal::new(false);
    let editing_id = RwSignal::new(None::<i64>);
    let editing_family = RwSignal::new(QualityFamily::Video);
    let name = RwSignal::new(String::new());
    let upgrade_allowed = RwSignal::new(true);
    let cutoff = RwSignal::new(0u32);
    let language = RwSignal::new(String::new());
    let min_score = RwSignal::new(String::from("0"));
    let cutoff_score = RwSignal::new(String::from("0"));
    let min_upgrade = RwSignal::new(String::from("1"));
    let min_size = RwSignal::new(String::new());
    let max_size = RwSignal::new(String::new());
    let items = RwSignal::new(Vec::<ProfileItem>::new());
    let scores = RwSignal::new(Vec::<(i64, String, RwSignal<String>)>::new());
    let dragging = RwSignal::new(None::<usize>);
    let saving = RwSignal::new(false);
    let delete_target = RwSignal::new(None::<QualityProfile>);
    let delete_open = RwSignal::new(false);

    let open_editor = move |p: QualityProfile, id: Option<i64>| {
        editing_id.set(id);
        editing_family.set(p.family);
        name.set(p.name.clone());
        upgrade_allowed.set(p.upgrade_allowed);
        cutoff.set(p.cutoff);
        language.set(p.language.map(|l| l.name().to_string()).unwrap_or_default());
        min_score.set(p.min_format_score.to_string());
        cutoff_score.set(p.cutoff_format_score.to_string());
        min_upgrade.set(p.min_upgrade_format_score.to_string());
        min_size.set(size_text(p.min_size_mb, p.family));
        max_size.set(size_text(p.max_size_mb, p.family));
        items.set(p.items.clone());
        let all = formats.get().and_then(Result::ok).unwrap_or_default();
        scores.set(
            all.iter()
                .map(|f| {
                    (
                        f.id,
                        f.name.clone(),
                        RwSignal::new(p.format_score(f.id).to_string()),
                    )
                })
                .collect(),
        );
        open.set(true);
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let format_items: Vec<FormatItem> = scores
            .get_untracked()
            .iter()
            .map(|(id, _, s)| FormatItem {
                format_id: *id,
                score: parse_i32(&s.get_untracked()),
            })
            .filter(|f| f.score != 0)
            .collect();
        let lang = language.get_untracked();
        let p = QualityProfile {
            id: editing_id.get_untracked().unwrap_or(0),
            name: name.get_untracked().trim().to_string(),
            items: items.get_untracked(),
            cutoff: cutoff.get_untracked(),
            upgrade_allowed: upgrade_allowed.get_untracked(),
            min_format_score: parse_i32(&min_score.get_untracked()),
            cutoff_format_score: parse_i32(&cutoff_score.get_untracked()),
            min_upgrade_format_score: parse_i32(&min_upgrade.get_untracked()).max(1),
            format_items,
            language: Language::ALL.iter().copied().find(|l| l.name() == lang),
            family: editing_family.get_untracked(),
            min_size_mb: parse_size(&min_size.get_untracked(), editing_family.get_untracked()),
            max_size_mb: parse_size(&max_size.get_untracked(), editing_family.get_untracked()),
        };
        if let Err(e) = p.validate() {
            toasts.error(e.to_string());
            return;
        }
        saving.set(true);
        spawn_local(async move {
            let res = match editing_id.get_untracked() {
                Some(id) => api::put::<_, QualityProfile>(&format!("/qualityprofiles/{id}"), &p).await,
                None => api::post::<_, QualityProfile>(&family_path("/qualityprofiles", p.family), &p).await,
            };
            match res {
                Ok(saved) => {
                    toasts.success(format!("Saved profile “{}”.", saved.name));
                    open.set(false);
                    profiles.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    let confirm_delete = Callback::new(move |()| {
        let Some(p) = delete_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/qualityprofiles/{}", p.id)).await {
                Ok(()) => {
                    toasts.success(format!("Deleted “{}”.", p.name));
                    profiles.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let new_profile = move |_| {
        let mut p = match family.get_untracked() {
            QualityFamily::Video => QualityProfile::with_allowed(
                "New profile",
                &[Quality::WebDl1080p, Quality::Bluray1080p],
                Quality::Bluray1080p,
            ),
            f => {
                // Start with the family's best quality allowed.
                let best = f.qualities().last().copied().unwrap_or(Quality::Unknown);
                QualityProfile::for_family("New profile", f, &[best], best)
            }
        };
        p.name = String::new();
        open_editor(p, None);
    };

    let list = move || {
        match profiles.get() {
        None => view! { <Skeleton lines=4 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| profiles.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="list" title="No quality profiles" description="Add a profile to decide which qualities to download." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <div class="cf-grid">
                {list
                    .into_iter()
                    .map(|p| {
                        let edit = p.clone();
                        let del = p.clone();
                        let cutoff_name = p
                            .items
                            .iter()
                            .find(|i| i.cutoff_id() == p.cutoff)
                            .map(ProfileItem::display_name)
                            .unwrap_or_default();
                        let allowed: Vec<String> = p.items.iter().rev().filter(|i| i.allowed).map(ProfileItem::display_name).collect();
                        view! {
                            <article class="cf-card">
                                <header class="cf-card-head">
                                    <h3 class="cf-name">{p.name.clone()}</h3>
                                    <span class="badge tone-neutral">{if p.upgrade_allowed { "Upgrades on" } else { "No upgrades" }}</span>
                                </header>
                                <p class="field-hint">{format!("Cutoff: {cutoff_name}")}</p>
                                {size_summary(p.min_size_mb, p.max_size_mb, p.family).map(|s| view! { <p class="field-hint">{s}</p> })}
                                <div class="chip-list">
                                    {allowed.into_iter().map(|a| view! { <span class="chip">{a}</span> }).collect_view()}
                                </div>
                                <div class="cf-actions">
                                    <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| { let id = edit.id; open_editor(edit.clone(), Some(id)) }>
                                        "Edit"
                                    </button>
                                    <button type="button" class="btn btn-sm btn-ghost" on:click={
                                        let src = p.clone();
                                        move |_| {
                                            let mut c = src.clone();
                                            c.name = format!("{} (copy)", src.name);
                                            open_editor(c, None);
                                        }
                                    }>
                                        <Icon name="copy" />
                                        "Clone"
                                    </button>
                                    <button
                                        type="button"
                                        class="btn btn-sm btn-ghost btn-danger-text"
                                        aria-label=format!("Delete {}", p.name)
                                        on:click=move |_| {
                                            delete_target.set(Some(del.clone()));
                                            delete_open.set(true);
                                        }
                                    >
                                        <Icon name="trash" />
                                    </button>
                                </div>
                            </article>
                        }
                    })
                    .collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    // Quality rows, best first. `i` is the internal (worst → best) index.
    let rows = move || {
        let list = items.get();
        let n = list.len();
        list.into_iter()
            .enumerate()
            .rev()
            .map(|(i, item)| {
                let label = item.display_name();
                let members: Vec<&'static str> = item.items.iter().map(|q| q.name()).collect();
                let is_group = !item.items.is_empty();
                let allowed = item.allowed;
                view! {
                    <li
                        class="q-row"
                        class:disallowed={!allowed}
                        class:drag-over=move || dragging.get().is_some_and(|d| d != i)
                        draggable="true"
                        on:dragstart=move |_| dragging.set(Some(i))
                        on:dragend=move |_| dragging.set(None)
                        on:dragover=|e: ev::DragEvent| e.prevent_default()
                        on:drop=move |e: ev::DragEvent| {
                            e.prevent_default();
                            if let Some(from) = dragging.get_untracked() {
                                items.update(|v| move_item(v, from, i));
                            }
                            dragging.set(None);
                        }
                    >
                        <span class="q-handle" aria-hidden="true">"⋮⋮"</span>
                        <label class="checkbox q-allowed">
                            <input
                                type="checkbox"
                                prop:checked=allowed
                                aria-label=format!("Allow {label}")
                                on:change=move |e| {
                                    let on = event_target_checked(&e);
                                    items.update(|v| if let Some(it) = v.get_mut(i) { it.allowed = on });
                                }
                            />
                            <span class="q-name">{label.clone()}</span>
                        </label>
                        {is_group.then(|| view! {
                            <span class="chip-list q-members">{members.into_iter().map(|m| view! { <span class="chip">{m}</span> }).collect_view()}</span>
                        })}
                        <span class="q-actions">
                            <button type="button" class="icon-btn icon-btn-sm" title="Move up" aria-label=format!("Move {label} up")
                                disabled={i + 1 >= n} on:click=move |_| items.update(|v| move_item(v, i, i + 1))>
                                <span aria-hidden="true">"▲"</span>
                            </button>
                            <button type="button" class="icon-btn icon-btn-sm" title="Move down" aria-label=format!("Move {label} down")
                                disabled={i == 0} on:click=move |_| items.update(|v| move_item(v, i, i - 1))>
                                <span aria-hidden="true">"▼"</span>
                            </button>
                            {if is_group {
                                view! {
                                    <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| items.update(|v| ungroup(v, i))>"Ungroup"</button>
                                }.into_any()
                            } else {
                                view! {
                                    <button type="button" class="btn btn-sm btn-ghost" disabled={i == 0} title="Group with the row below"
                                        on:click=move |_| items.update(|v| merge_down(v, i))>"Group ↓"</button>
                                }.into_any()
                            }}
                        </span>
                    </li>
                }
            })
            .collect_view()
    };

    let cutoff_options = move || {
        items
            .get()
            .iter()
            .rev()
            .filter(|i| i.allowed)
            .map(|i| {
                let id = i.cutoff_id();
                view! { <option value=id.to_string() selected=move || cutoff.get() == id>{i.display_name()}</option> }
            })
            .collect_view()
    };

    view! {
        <Card
            title="Quality profiles"
            actions=view! {
                <button type="button" class="btn btn-primary btn-sm" on:click=new_profile>
                    <Icon name="plus" />
                    "New profile"
                </button>
            }
            .into_any()
        >
            <FamilyTabs family=family label="Quality family" />
            {list}
        </Card>

        <Modal open=open title="Quality profile">
            <form class="form profile-form" on:submit=save>
                <div class="field-row">
                    <div class="field">
                        <label for="qp-name">"Name"</label>
                        <input id="qp-name" class="input" type="text" required maxlength="100"
                            prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="qp-lang">"Language"</label>
                        <select id="qp-lang" class="input" on:change=move |e| language.set(event_target_value(&e))>
                            <option value="" selected=move || language.get().is_empty()>"Any"</option>
                            {Language::ALL
                                .iter()
                                .filter(|l| **l != Language::Unknown)
                                .map(|l| {
                                    let n = l.name();
                                    view! { <option value=n selected=move || language.get() == n>{n}</option> }
                                })
                                .collect_view()}
                        </select>
                    </div>
                </div>
                <div class="field-row">
                    <div class="field field-check">
                        <label class="checkbox">
                            <input type="checkbox" prop:checked=move || upgrade_allowed.get()
                                on:change=move |e| upgrade_allowed.set(event_target_checked(&e)) />
                            "Upgrades allowed"
                        </label>
                    </div>
                    <div class="field">
                        <label for="qp-cutoff">"Upgrade until"</label>
                        <select id="qp-cutoff" class="input" on:change=move |e| cutoff.set(event_target_value(&e).parse().unwrap_or(0))>
                            {cutoff_options}
                        </select>
                    </div>
                </div>
                <div class="field">
                    <span class="field-label">"Qualities"</span>
                    <p class="field-hint">"Best first. Drag or use the arrows to reorder; grouped qualities rank equally."</p>
                    <ul class="q-list">{rows}</ul>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="qp-min-size">{move || format!("Minimum size ({})", if size_in_gb(editing_family.get()) { "GB" } else { "MB" })}</label>
                        <input id="qp-min-size" class="input" type="number" min="0" step="any" placeholder="No minimum"
                            prop:value=move || min_size.get() on:input=move |e| min_size.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="qp-max-size">{move || format!("Maximum size ({})", if size_in_gb(editing_family.get()) { "GB" } else { "MB" })}</label>
                        <input id="qp-max-size" class="input" type="number" min="0" step="any" placeholder="No maximum"
                            prop:value=move || max_size.get() on:input=move |e| max_size.set(event_target_value(&e)) />
                    </div>
                </div>
                <p class="field-hint">{move || if editing_family.get() == QualityFamily::Video {
                    "Releases outside these limits are never grabbed. For TV the limits are per episode, so season packs may be larger. Leave empty for no limit."
                } else {
                    "Releases outside these limits are never grabbed. Leave empty for no limit."
                }}</p>
                <div class="field-row field-row-3">
                    <div class="field">
                        <label for="qp-min">"Minimum format score"</label>
                        <input id="qp-min" class="input" type="number" prop:value=move || min_score.get()
                            on:input=move |e| min_score.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="qp-cut">"Upgrade until format score"</label>
                        <input id="qp-cut" class="input" type="number" prop:value=move || cutoff_score.get()
                            on:input=move |e| cutoff_score.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="qp-inc">"Minimum score increment"</label>
                        <input id="qp-inc" class="input" type="number" min="1" prop:value=move || min_upgrade.get()
                            on:input=move |e| min_upgrade.set(event_target_value(&e)) />
                    </div>
                </div>
                <div class="field">
                    <span class="field-label">"Custom format scores"</span>
                    {move || {
                        let list = scores.get();
                        if list.is_empty() {
                            view! { <p class="field-hint">"No custom formats yet — create them under Settings → Custom Formats."</p> }.into_any()
                        } else {
                            view! {
                                <div class="score-grid">
                                    {list.into_iter().map(|(id, n, s)| view! {
                                        <label class="score-row" for=format!("cf-score-{id}")>
                                            <span>{n}</span>
                                            <input id=format!("cf-score-{id}") class="input input-sm" type="number"
                                                prop:value=move || s.get() on:input=move |e| s.set(event_target_value(&e)) />
                                        </label>
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }
                    }}
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            </form>
        </Modal>

        <ConfirmDialog
            open=delete_open
            title="Delete quality profile?"
            message=Signal::derive(move || delete_target.with(|p| p.as_ref().map(|p| format!("“{}” will be removed.", p.name)).unwrap_or_default()))
            confirm_label="Delete"
            on_confirm=confirm_delete
        />
    }
}

// ---------------- delay profiles ----------------

fn parse_tags(s: &str) -> Vec<i64> {
    s.split(',').filter_map(|t| t.trim().parse().ok()).collect()
}

fn join_tags(t: &[i64]) -> String {
    t.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

#[component]
fn DelayProfilesCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let delays = LocalResource::new(|| api::get::<Vec<DelayProfile>>("/delayprofiles"));
    let open = RwSignal::new(false);
    let draft = RwSignal::new(DelayProfile::default());
    let tags = RwSignal::new(String::new());

    let edit = move |p: DelayProfile| {
        tags.set(join_tags(&p.tags));
        draft.set(p);
        open.set(true);
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let mut p = draft.get_untracked();
        p.tags = parse_tags(&tags.get_untracked());
        spawn_local(async move {
            let res = if p.id > 0 {
                api::put::<_, DelayProfile>(&format!("/delayprofiles/{}", p.id), &p).await
            } else {
                api::post::<_, DelayProfile>("/delayprofiles", &p).await
            };
            match res {
                Ok(_) => {
                    toasts.success("Delay profile saved.");
                    open.set(false);
                    delays.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    let remove = move |id: i64| {
        spawn_local(async move {
            match api::delete(&format!("/delayprofiles/{id}")).await {
                Ok(()) => delays.refetch(),
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    };

    let proto = |p: Protocol| {
        if p == Protocol::Usenet {
            "Usenet"
        } else {
            "Torrent"
        }
    };
    let list = move || {
        match delays.get() {
        None => view! { <Skeleton lines=2 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(list)) => view! {
            <DataTable caption="Delay profiles" headers=vec!["Preferred", "Usenet delay", "Torrent delay", "Bypass", "Tags", ""]>
                {list.into_iter().map(|p| {
                    let e = p.clone();
                    let id = p.id;
                    let is_default = p.tags.is_empty();
                    view! {
                        <tr>
                            <td class="cell-strong">{proto(p.preferred_protocol)}</td>
                            <td>{if p.enable_usenet { format!("{} min", p.usenet_delay) } else { "disabled".into() }}</td>
                            <td>{if p.enable_torrent { format!("{} min", p.torrent_delay) } else { "disabled".into() }}</td>
                            <td>{if p.bypass_if_highest_quality { "Highest quality" } else { "—" }}</td>
                            <td>{if is_default { "Default (all)".to_string() } else { join_tags(&p.tags) }}</td>
                            <td class="cell-actions">
                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| edit(e.clone())>"Edit"</button>
                                {(!is_default).then(|| view! {
                                    <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label="Delete delay profile" on:click=move |_| remove(id)>
                                        <Icon name="trash" />
                                    </button>
                                })}
                            </td>
                        </tr>
                    }
                }).collect_view()}
            </DataTable>
        }
        .into_any(),
    }
    };

    let num = move |get: fn(&DelayProfile) -> u32, set: fn(&mut DelayProfile, u32)| {
        (
            move || get(&draft.get()).to_string(),
            move |e: ev::Event| {
                let v = event_target_value(&e).trim().parse().unwrap_or(0);
                draft.update(|d| set(d, v));
            },
        )
    };
    let (usenet_val, usenet_set) = num(|d| d.usenet_delay, |d, v| d.usenet_delay = v);
    let (torrent_val, torrent_set) = num(|d| d.torrent_delay, |d, v| d.torrent_delay = v);
    let check = move |get: fn(&DelayProfile) -> bool, set: fn(&mut DelayProfile, bool)| {
        (
            move || get(&draft.get()),
            move |e: ev::Event| {
                let v = event_target_checked(&e);
                draft.update(|d| set(d, v));
            },
        )
    };
    let (u_on, u_set) = check(|d| d.enable_usenet, |d, v| d.enable_usenet = v);
    let (t_on, t_set) = check(|d| d.enable_torrent, |d, v| d.enable_torrent = v);
    let (b_on, b_set) = check(
        |d| d.bypass_if_highest_quality,
        |d, v| d.bypass_if_highest_quality = v,
    );

    view! {
        <Card
            title="Delay profiles"
            subtitle="Wait before grabbing so a better release (or the preferred protocol) can show up. Tagged profiles override the default."
            actions=view! {
                <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| edit(DelayProfile { tags: vec![], order: 1, ..Default::default() })>
                    <Icon name="plus" />
                    "Add"
                </button>
            }
            .into_any()
        >
            {list}
        </Card>
        <Modal open=open title="Delay profile">
            <form class="form" on:submit=save>
                <div class="field-row">
                    <label class="checkbox"><input type="checkbox" prop:checked=u_on on:change=u_set />"Enable Usenet"</label>
                    <label class="checkbox"><input type="checkbox" prop:checked=t_on on:change=t_set />"Enable Torrent"</label>
                </div>
                <div class="field">
                    <label for="dp-pref">"Preferred protocol"</label>
                    <select id="dp-pref" class="input" on:change=move |e| {
                        let p = if event_target_value(&e) == "torrent" { Protocol::Torrent } else { Protocol::Usenet };
                        draft.update(|d| d.preferred_protocol = p);
                    }>
                        <option value="usenet" selected=move || draft.get().preferred_protocol == Protocol::Usenet>"Usenet"</option>
                        <option value="torrent" selected=move || draft.get().preferred_protocol == Protocol::Torrent>"Torrent"</option>
                    </select>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="dp-u">"Usenet delay (minutes)"</label>
                        <input id="dp-u" class="input" type="number" min="0" prop:value=usenet_val on:change=usenet_set />
                    </div>
                    <div class="field">
                        <label for="dp-t">"Torrent delay (minutes)"</label>
                        <input id="dp-t" class="input" type="number" min="0" prop:value=torrent_val on:change=torrent_set />
                    </div>
                </div>
                <label class="checkbox"><input type="checkbox" prop:checked=b_on on:change=b_set />"Bypass when the release is the profile's highest quality"</label>
                <Show when=move || draft.with(|d| d.id == 0 || !d.tags.is_empty())>
                    <div class="field">
                        <label for="dp-tags">"Tag ids"</label>
                        <input id="dp-tags" class="input" type="text" placeholder="e.g. 1, 4"
                            prop:value=move || tags.get() on:input=move |e| tags.set(event_target_value(&e)) />
                        <p class="field-hint">"Comma-separated. Tag management arrives with the library (M2)."</p>
                    </div>
                </Show>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary">"Save"</button>
                </div>
            </form>
        </Modal>
    }
}

// ---------------- release profiles ----------------

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[component]
fn ReleaseProfilesCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let profiles = LocalResource::new(|| api::get::<Vec<ReleaseProfile>>("/releaseprofiles"));
    let open = RwSignal::new(false);
    let id = RwSignal::new(0i64);
    let name = RwSignal::new(String::new());
    let enabled = RwSignal::new(true);
    let required = RwSignal::new(String::new());
    let ignored = RwSignal::new(String::new());
    let tags = RwSignal::new(String::new());

    let edit = move |p: Option<ReleaseProfile>| {
        let p = p.unwrap_or(ReleaseProfile {
            id: 0,
            name: String::new(),
            enabled: true,
            required: vec![],
            ignored: vec![],
            indexer_id: None,
            tags: vec![],
        });
        id.set(p.id);
        name.set(p.name);
        enabled.set(p.enabled);
        required.set(p.required.join("\n"));
        ignored.set(p.ignored.join("\n"));
        tags.set(join_tags(&p.tags));
        open.set(true);
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let p = ReleaseProfile {
            id: id.get_untracked(),
            name: name.get_untracked(),
            enabled: enabled.get_untracked(),
            required: lines(&required.get_untracked()),
            ignored: lines(&ignored.get_untracked()),
            indexer_id: None,
            tags: parse_tags(&tags.get_untracked()),
        };
        if let Err(e) = p.validate() {
            toasts.error(e.to_string());
            return;
        }
        spawn_local(async move {
            let res = if p.id > 0 {
                api::put::<_, ReleaseProfile>(&format!("/releaseprofiles/{}", p.id), &p).await
            } else {
                api::post::<_, ReleaseProfile>("/releaseprofiles", &p).await
            };
            match res {
                Ok(_) => {
                    toasts.success("Release profile saved.");
                    open.set(false);
                    profiles.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    let remove = move |pid: i64| {
        spawn_local(async move {
            match api::delete(&format!("/releaseprofiles/{pid}")).await {
                Ok(()) => profiles.refetch(),
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    };

    let list = move || {
        match profiles.get() {
        None => view! { <Skeleton lines=2 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="shield" title="No release profiles" description="Require or forbid terms like “x265” or /\\bCAM\\b/i in release titles." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <DataTable caption="Release profiles" headers=vec!["Name", "Must contain", "Must not contain", "Enabled", ""]>
                {list.into_iter().map(|p| {
                    let e = p.clone();
                    let pid = p.id;
                    view! {
                        <tr>
                            <td class="cell-strong">{if p.name.is_empty() { "(unnamed)".to_string() } else { p.name.clone() }}</td>
                            <td><span class="chip-list">{p.required.iter().map(|t| view! { <span class="chip mono">{t.clone()}</span> }).collect_view()}</span></td>
                            <td><span class="chip-list">{p.ignored.iter().map(|t| view! { <span class="chip mono">{t.clone()}</span> }).collect_view()}</span></td>
                            <td>{if p.enabled { "Yes" } else { "No" }}</td>
                            <td class="cell-actions">
                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| edit(Some(e.clone()))>"Edit"</button>
                                <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label="Delete release profile" on:click=move |_| remove(pid)>
                                    <Icon name="trash" />
                                </button>
                            </td>
                        </tr>
                    }
                }).collect_view()}
            </DataTable>
        }
        .into_any(),
    }
    };

    view! {
        <Card
            title="Release profiles"
            subtitle="Terms are case-insensitive text or /regex/i. A release needs one “must contain” term and no “must not contain” term."
            actions=view! {
                <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| edit(None)>
                    <Icon name="plus" />
                    "Add"
                </button>
            }
            .into_any()
        >
            {list}
        </Card>
        <Modal open=open title="Release profile">
            <form class="form" on:submit=save>
                <div class="field-row">
                    <div class="field">
                        <label for="rp-name">"Name"</label>
                        <input id="rp-name" class="input" type="text" maxlength="100"
                            prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e)) />
                    </div>
                    <div class="field field-check">
                        <label class="checkbox">
                            <input type="checkbox" prop:checked=move || enabled.get() on:change=move |e| enabled.set(event_target_checked(&e)) />
                            "Enabled"
                        </label>
                    </div>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="rp-req">"Must contain (one per line)"</label>
                        <textarea id="rp-req" class="input textarea mono" rows="5"
                            prop:value=move || required.get() on:input=move |e| required.set(event_target_value(&e))></textarea>
                    </div>
                    <div class="field">
                        <label for="rp-ign">"Must not contain (one per line)"</label>
                        <textarea id="rp-ign" class="input textarea mono" rows="5"
                            prop:value=move || ignored.get() on:input=move |e| ignored.set(event_target_value(&e))></textarea>
                    </div>
                </div>
                <div class="field">
                    <label for="rp-tags">"Tag ids"</label>
                    <input id="rp-tags" class="input" type="text" placeholder="empty = all media"
                        prop:value=move || tags.get() on:input=move |e| tags.set(event_target_value(&e)) />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary">"Save"</button>
                </div>
            </form>
        </Modal>
    }
}
