//! `/settings/mediamanagement` — root folders (with free space) and import / file handling settings.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::library::{
    CreateRootFolderRequest, ImportMode, MediaKind, MediaManagementSettings, RescanAfterRefresh,
    RootFolderDto,
};

use crate::api;
use crate::components::library_common::{bytes, on_entity_changed};
use crate::components::library_forms::EnumSelect;
use crate::components::{
    Card, ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, Spinner, StatusBadge, Tone,
};
use crate::pages::system::require_admin;
use crate::state::Toasts;

const IMPORT_MODES: [(ImportMode, &str, &str); 3] = [
    (
        ImportMode::HardlinkOrCopy,
        "hardlink_or_copy",
        "Hardlink, or copy when not possible",
    ),
    (ImportMode::Copy, "copy", "Copy"),
    (ImportMode::Move, "move", "Move"),
];

const RESCAN: [(RescanAfterRefresh, &str, &str); 3] = [
    (RescanAfterRefresh::Always, "always", "Always"),
    (
        RescanAfterRefresh::AfterManual,
        "after_manual",
        "After a manual refresh",
    ),
    (RescanAfterRefresh::Never, "never", "Never"),
];

#[component]
pub fn MediaManagementPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Media management" subtitle="Where your library lives and how files are imported, replaced and deleted." />
        <div class="stack">
            <RootFoldersCard />
            <SettingsCard />
        </div>
    }
}

#[component]
pub fn RootFoldersCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let roots = LocalResource::new(|| api::get::<Vec<RootFolderDto>>("/rootfolders"));
    on_entity_changed(&["root_folder", "rootfolder"], false, move || roots.refetch());
    let path = RwSignal::new(String::new());
    let kind = RwSignal::new(MediaKind::Movie);
    let busy = RwSignal::new(false);
    let delete_target = RwSignal::new(None::<RootFolderDto>);
    let delete_open = RwSignal::new(false);

    let add = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let p = path.get_untracked().trim().to_string();
        if p.is_empty() {
            return;
        }
        busy.set(true);
        let req = CreateRootFolderRequest {
            path: p,
            media_kind: kind.get_untracked(),
        };
        spawn_local(async move {
            match api::post::<_, RootFolderDto>("/rootfolders", &req).await {
                Ok(r) => {
                    toasts.success(format!("Added root folder {}.", r.path));
                    path.set(String::new());
                    roots.refetch();
                }
                Err(e) => toasts.error(format!("Could not add the root folder: {e}")),
            }
            busy.set(false);
        });
    };
    let confirm_delete = Callback::new(move |()| {
        let Some(r) = delete_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/rootfolders/{}", r.id)).await {
                Ok(()) => {
                    toasts.success("Root folder removed.");
                    roots.refetch();
                }
                Err(e) => toasts.error(format!("Could not remove: {e}")),
            }
        });
    });

    let list = move || {
        match roots.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| roots.refetch()) /> }.into_any(),
        Some(Ok(l)) if l.is_empty() => view! {
            <EmptyState icon="folder" title="No root folders" description="Add the folders where your movies and series live (e.g. /media/movies)." compact=true />
        }
        .into_any(),
        Some(Ok(l)) => view! {
            <div class="root-list">
                {l.into_iter().map(|r| {
                    let del = r.clone();
                    let used = match (r.free_space, r.total_space) {
                        (Some(f), Some(t)) if t > 0 => Some(((t - f.min(t)) * 100 / t, f, t)),
                        _ => None,
                    };
                    let unmapped = r.unmapped_folders.len();
                    view! {
                        <div class="root-row">
                            <div>
                                <span class="root-path">{r.path.clone()}</span>
                                <div class="meta-line">
                                    <span class="chip">{if r.media_kind == MediaKind::Movie { "Movies" } else { "Series" }}</span>
                                    {if r.accessible {
                                        view! { <StatusBadge tone=Tone::Success label="Accessible" /> }.into_any()
                                    } else {
                                        view! { <StatusBadge tone=Tone::Danger label="Not accessible" dot=true /> }.into_any()
                                    }}
                                    {(unmapped > 0).then(|| view! {
                                        <a href=format!("{}/{}/import", api::base_path(), if r.media_kind == MediaKind::Movie { "movies" } else { "series" })>
                                            {format!("{unmapped} unmapped folder(s)")}
                                        </a>
                                    })}
                                </div>
                            </div>
                            <div class="root-free">
                                {match used {
                                    Some((pct, f, t)) => view! {
                                        <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow=pct
                                            aria-label="Disk usage">
                                            <div class="progress-bar" style=format!("width: {pct}%")></div>
                                        </div>
                                        <span class="cell-sub">{format!("{} free of {}", bytes(f), bytes(t))}</span>
                                    }.into_any(),
                                    None => view! { <span class="cell-sub">"Free space unknown"</span> }.into_any(),
                                }}
                            </div>
                            <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label=format!("Remove {}", r.path)
                                on:click=move |_| { delete_target.set(Some(del.clone())); delete_open.set(true); }>
                                <Icon name="trash" />
                            </button>
                        </div>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <Card title="Root folders" subtitle="Top-level folders that hold movie and series folders.">
            {list}
            <form class="lib-toolbar root-add" on:submit=add>
                <input class="input grow mono" type="text" placeholder="/media/movies" aria-label="Root folder path" required
                    prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e)) />
                <select class="input" aria-label="Media type" on:change=move |e| {
                    kind.set(if event_target_value(&e) == "series" { MediaKind::Series } else { MediaKind::Movie })
                }>
                    <option value="movie" selected=move || kind.get() == MediaKind::Movie>"Movies"</option>
                    <option value="series" selected=move || kind.get() == MediaKind::Series>"Series"</option>
                </select>
                <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                    <Icon name="plus" />
                    "Add root folder"
                </button>
            </form>
        </Card>
        <ConfirmDialog open=delete_open title="Remove root folder?"
            message=Signal::derive(move || delete_target.with(|r| r.as_ref().map(|r| format!("{} will be removed from Reelix. Files on disk are not touched.", r.path)).unwrap_or_default()))
            confirm_label="Remove" on_confirm=confirm_delete />
    }
}

fn opt_str(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[component]
fn SettingsCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let loaded = LocalResource::new(|| api::get::<MediaManagementSettings>("/settings/mediamanagement"));
    let s = RwSignal::new(MediaManagementSettings::default());
    let import_mode = RwSignal::new(ImportMode::HardlinkOrCopy);
    let rescan = RwSignal::new(RescanAfterRefresh::Always);
    let exts = RwSignal::new(String::new());
    let saving = RwSignal::new(false);

    Effect::new(move |_| {
        if let Some(Ok(v)) = loaded.get() {
            import_mode.set(v.import_mode);
            rescan.set(v.rescan_after_refresh);
            exts.set(v.extra_file_extensions.join(", "));
            s.set(v);
        }
    });

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let mut v = s.get_untracked();
        v.import_mode = import_mode.get_untracked();
        v.rescan_after_refresh = rescan.get_untracked();
        v.extra_file_extensions = exts
            .get_untracked()
            .split([',', ' '])
            .map(|x| x.trim().trim_start_matches('.').to_string())
            .filter(|x| !x.is_empty())
            .collect();
        for m in [&v.file_mode, &v.dir_mode].into_iter().flatten() {
            if !m.chars().all(|c| ('0'..='7').contains(&c)) || m.len() > 4 {
                toasts.error(format!("“{m}” is not an octal permission like 644."));
                return;
            }
        }
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, MediaManagementSettings>("/settings/mediamanagement", &v).await {
                Ok(saved) => {
                    toasts.success("Media management settings saved.");
                    s.set(saved);
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    // Small binding helpers.
    let check = move |label: &'static str,
                      hint: &'static str,
                      get: fn(&MediaManagementSettings) -> bool,
                      set: fn(&mut MediaManagementSettings, bool)| {
        view! {
            <div class="field">
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.with(get) on:change=move |e| { let v = event_target_checked(&e); s.update(|x| set(x, v)); } />
                    {label}
                </label>
                <span class="field-hint">{hint}</span>
            </div>
        }
    };
    let text = move |id: &'static str,
                     label: &'static str,
                     placeholder: &'static str,
                     get: fn(&MediaManagementSettings) -> String,
                     set: fn(&mut MediaManagementSettings, String)| {
        view! {
            <div class="field">
                <label for=id>{label}</label>
                <input id=id class="input" type="text" placeholder=placeholder prop:value=move || s.with(get)
                    on:input=move |e| { let v = event_target_value(&e); s.update(|x| set(x, v)); } />
            </div>
        }
    };
    let num = move |id: &'static str,
                    label: &'static str,
                    get: fn(&MediaManagementSettings) -> String,
                    set: fn(&mut MediaManagementSettings, &str)| {
        view! {
            <div class="field">
                <label for=id>{label}</label>
                <input id=id class="input" type="number" min="0" prop:value=move || s.with(get)
                    on:input=move |e| { let v = event_target_value(&e); s.update(|x| set(x, &v)); } />
            </div>
        }
    };

    let body = move || {
        match loaded.get() {
        None => view! { <Skeleton lines=6 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| loaded.refetch()) /> }.into_any(),
        Some(Ok(_)) => view! {
            <form class="form" on:submit=save>
                <h3 class="card-title">"Importing"</h3>
                <div class="field-row">
                    <div class="field">
                        <label for="mm-mode">"Import mode"</label>
                        <EnumSelect table=&IMPORT_MODES value=import_mode id="mm-mode" />
                        <span class="field-hint">"Hardlinks keep torrents seeding without using extra space."</span>
                    </div>
                    <div class="field">
                        <label for="mm-exts">"Extra file extensions"</label>
                        <input id="mm-exts" class="input" type="text" placeholder="srt, ass, nfo" prop:value=move || exts.get()
                            on:input=move |e| exts.set(event_target_value(&e)) />
                        <span class="field-hint">"Imported next to the video when “Import extra files” is on."</span>
                    </div>
                </div>
                <div class="field-row">
                    {check("Import extra files", "Subtitles, NFOs and other sidecar files.", |x| x.import_extra_files, |x, v| x.import_extra_files = v)}
                    {check("Skip free space check", "Import even when a root folder reports too little space.", |x| x.skip_free_space_check, |x, v| x.skip_free_space_check = v)}
                </div>
                <div class="field-row">
                    {num("mm-free", "Minimum free space (MB)", |x| x.minimum_free_space_mb.to_string(),
                        |x, v| x.minimum_free_space_mb = v.trim().parse().unwrap_or(0))}
                    <div class="field">
                        <label for="mm-ffprobe">"ffprobe path"</label>
                        <input id="mm-ffprobe" class="input mono" type="text" placeholder="ffprobe" prop:value=move || s.with(|x| x.ffprobe_path.clone())
                            on:input=move |e| { let v = event_target_value(&e); s.update(|x| x.ffprobe_path = v); } />
                    </div>
                </div>
                {check("Analyse video files", "Read codecs, resolution, HDR and audio with ffprobe during disk scans.", |x| x.analyze_video_files, |x, v| x.analyze_video_files = v)}

                <h3 class="card-title">"Folders & files"</h3>
                <div class="field-row">
                    {check("Create empty folders", "Create movie/series folders when items are added.", |x| x.create_empty_folders, |x, v| x.create_empty_folders = v)}
                    {check("Delete empty folders", "Remove folders left empty after a file is deleted or moved.", |x| x.delete_empty_folders, |x, v| x.delete_empty_folders = v)}
                </div>
                <div class="field-row">
                    {check("Unmonitor deleted files", "Unmonitor items whose files disappear outside Reelix.", |x| x.unmonitor_deleted, |x, v| x.unmonitor_deleted = v)}
                    <div class="field">
                        <label for="mm-rescan">"Rescan folders after refresh"</label>
                        <EnumSelect table=&RESCAN value=rescan id="mm-rescan" />
                    </div>
                </div>

                <h3 class="card-title">"Recycle bin"</h3>
                <div class="field-row">
                    {text("mm-bin", "Recycle bin path", "/media/.recycle (empty = delete permanently)",
                        |x| x.recycle_bin_path.clone().unwrap_or_default(), |x, v| x.recycle_bin_path = opt_str(&v))}
                    {num("mm-binret", "Cleanup after (days, 0 = never)", |x| x.recycle_bin_retention_days.to_string(),
                        |x, v| x.recycle_bin_retention_days = v.trim().parse().unwrap_or(0))}
                </div>

                <h3 class="card-title">"Permissions"</h3>
                {check("Set permissions", "chmod / chown imported files and folders (Linux / macOS).", |x| x.set_permissions, |x, v| x.set_permissions = v)}
                <Show when=move || s.with(|x| x.set_permissions)>
                    <div class="field-row">
                        {text("mm-fmode", "File mode", "644", |x| x.file_mode.clone().unwrap_or_default(), |x, v| x.file_mode = opt_str(&v))}
                        {text("mm-dmode", "Folder mode", "755", |x| x.dir_mode.clone().unwrap_or_default(), |x, v| x.dir_mode = opt_str(&v))}
                    </div>
                    <div class="field-row">
                        {text("mm-uid", "Owner user id", "1000", |x| x.chown_uid.map(|v| v.to_string()).unwrap_or_default(), |x, v| x.chown_uid = v.trim().parse().ok())}
                        {text("mm-gid", "Owner group id", "1000", |x| x.chown_gid.map(|v| v.to_string()).unwrap_or_default(), |x, v| x.chown_gid = v.trim().parse().ok())}
                    </div>
                </Show>
                <div class="modal-footer">
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        {move || if saving.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                        "Save changes"
                    </button>
                </div>
            </form>
        }
        .into_any(),
    }
    };

    view! { <Card title="File handling">{body}</Card> }
}
