//! The "Request" dialog (season picker, 4K, advanced options) and the "Report issue" dialog.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::requests::{
    AvailabilityStatus, CreateIssueDto, CreateRequestDto, IssueDto, IssueType, MediaDetailDto, QuotaDto,
    QuotaUsageDto, RequestDto, RequestOptionsDto, SeasonDto,
};
use reelix_core::MediaKind;

use super::request_status::{ISSUE_TYPES, issue_type_label, kind_param, request_status_label};
use super::{Icon, Modal};
use crate::api;
use crate::state::{AuthState, Toasts};

fn season_blocked(s: &SeasonDto, is_4k: bool) -> Option<&'static str> {
    if !is_4k && s.availability == AvailabilityStatus::Available {
        return Some("Available");
    }
    let req = if is_4k {
        s.request_status_4k
    } else {
        s.request_status
    };
    req.filter(|r| r.is_active()).map(request_status_label)
}

fn season_name(s: &SeasonDto) -> String {
    match &s.name {
        Some(n) if !n.trim().is_empty() => n.clone(),
        _ => format!("Season {}", s.season_number),
    }
}

/// One line of quota usage, e.g. "2 of 5 movie requests left this week".
pub fn quota_line(q: &QuotaUsageDto, noun: &str) -> Option<String> {
    let limit = q.limit?;
    let left = q.remaining.unwrap_or_else(|| limit.saturating_sub(q.used));
    let window = match q.days {
        1 => "today".to_string(),
        7 => "this week".to_string(),
        d => format!("in the last {d} days"),
    };
    Some(format!("{left} of {limit} {noun} left {window}"))
}

#[component]
pub fn RequestModal(
    open: RwSignal<bool>,
    #[prop(into)] detail: Signal<Option<MediaDetailDto>>,
    /// Called with the created request.
    #[prop(optional)]
    on_requested: Option<Callback<RequestDto>>,
) -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let is_4k = RwSignal::new(false);
    let selected = RwSignal::new(Vec::<u32>::new());
    let advanced = RwSignal::new(false);
    let profile_id = RwSignal::new(String::new());
    let root_id = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let kind = move || detail.with(|d| d.as_ref().map(|d| d.kind).unwrap_or(MediaKind::Movie));

    // Reset choices every time the dialog opens: preselect every requestable season.
    Effect::new(move |_| {
        if open.get() {
            is_4k.set(false);
            advanced.set(false);
            profile_id.set(String::new());
            root_id.set(String::new());
            let seasons = detail.with_untracked(|d| {
                d.as_ref()
                    .map(|d| {
                        d.seasons
                            .iter()
                            .filter(|s| season_blocked(s, false).is_none())
                            .map(|s| s.season_number)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            selected.set(seasons);
        }
    });
    // Seasons blocked in the newly chosen quality drop out of the selection.
    Effect::new(move |_| {
        let k = is_4k.get();
        detail.with_untracked(|d| {
            if let Some(d) = d {
                selected.update(|sel| {
                    sel.retain(|n| {
                        d.seasons
                            .iter()
                            .find(|s| s.season_number == *n)
                            .is_some_and(|s| season_blocked(s, k).is_none())
                    })
                });
            }
        });
    });

    let options = LocalResource::new(move || {
        let want = open.get() && advanced.get();
        let k = kind_param(kind());
        let four_k = is_4k.get();
        async move {
            if !want {
                return None;
            }
            Some(api::get::<RequestOptionsDto>(&format!("/requests/options?kind={k}&is_4k={four_k}")).await)
        }
    });
    // Fill the selects with the server defaults once options arrive.
    Effect::new(move |_| {
        if let Some(Some(Ok(o))) = options.get() {
            if profile_id.get_untracked().is_empty()
                && let Some(p) = o.default_profile_id.or(o.profiles.first().map(|p| p.id))
            {
                profile_id.set(p.to_string());
            }
            if root_id.get_untracked().is_empty()
                && let Some(r) = o.default_root_folder_id.or(o.root_folders.first().map(|r| r.id))
            {
                root_id.set(r.to_string());
            }
        }
    });

    let quota = LocalResource::new(move || {
        let id = auth.user.with(|u| u.as_ref().map(|u| u.id));
        let want = open.get();
        async move {
            match (want, id) {
                (true, Some(id)) => api::get::<QuotaDto>(&format!("/users/{id}/quota")).await.ok(),
                _ => None,
            }
        }
    });

    let quota_view = move || {
        let q = quota.get().flatten()?;
        if q.exempt {
            return None;
        }
        let (usage, noun) = match kind() {
            MediaKind::Movie => (q.movie, "movie requests"),
            MediaKind::Series => (q.tv, "season requests"),
        };
        let line = quota_line(&usage, noun)?;
        Some(view! {
            <div class="alert" class:alert-warning=usage.restricted class:alert-info=!usage.restricted role="status">
                <Icon name="info" />
                <div class="alert-body"><span>{line}</span></div>
            </div>
        })
    };

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(d) = detail.get_untracked() else { return };
        let series = d.kind == MediaKind::Series;
        let partial = d.request_ability.partial_seasons;
        let seasons = if series && partial {
            let mut s = selected.get_untracked();
            if s.is_empty() {
                toasts.error("Pick at least one season to request.");
                return;
            }
            s.sort_unstable();
            Some(s)
        } else {
            None
        };
        let adv = advanced.get_untracked();
        let req = CreateRequestDto {
            kind: d.kind,
            tmdb_id: d.tmdb_id,
            seasons,
            is_4k: is_4k.get_untracked(),
            profile_id: adv.then(|| profile_id.get_untracked().parse().ok()).flatten(),
            root_folder_id: adv.then(|| root_id.get_untracked().parse().ok()).flatten(),
            tags: Vec::new(),
            user_id: None,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, RequestDto>("/requests", &req).await {
                Ok(r) => {
                    let msg = match r.status {
                        reelix_api_types::requests::RequestStatus::Pending => {
                            format!("Requested “{}”. An admin will review it.", r.title)
                        }
                        _ => format!("Requested “{}” — it was approved automatically.", r.title),
                    };
                    toasts.success(msg);
                    open.set(false);
                    if let Some(cb) = on_requested {
                        cb.run(r);
                    }
                }
                Err(e) => toasts.error(format!("Could not request: {e}")),
            }
            busy.set(false);
        });
    };

    let title = Signal::derive(move || {
        detail.with(|d| {
            d.as_ref()
                .map(|d| format!("Request {}", d.title))
                .unwrap_or_else(|| "Request".into())
        })
    });

    let season_picker = move || {
        let d = detail.get()?;
        if d.kind != MediaKind::Series {
            return None;
        }
        if !d.request_ability.partial_seasons {
            return Some(
                view! {
                    <p class="muted">"The whole series will be requested, including future seasons."</p>
                }
                .into_any(),
            );
        }
        let rows = d
            .seasons
            .into_iter()
            .map(|s| {
                let n = s.season_number;
                let s2 = StoredValue::new(s.clone());
                let blocked = move || s2.with_value(|s| season_blocked(s, is_4k.get()));
                let checked = move || blocked().is_none() && selected.with(|v| v.contains(&n));
                let name = season_name(&s);
                let eps = format!("{} episodes", s.episode_count);
                view! {
                    <label class="season-row" class:disabled=move || blocked().is_some()>
                        <input
                            type="checkbox"
                            prop:checked=checked
                            disabled=move || blocked().is_some()
                            on:change=move |e| {
                                let on = event_target_checked(&e);
                                selected.update(|v| {
                                    v.retain(|x| *x != n);
                                    if on {
                                        v.push(n);
                                    }
                                });
                            }
                        />
                        <span class="season-name">{name}</span>
                        <span class="season-eps muted">{eps}</span>
                        <span class="season-state">
                            {move || blocked().map(|b| view! { <span class="badge tone-neutral">{b}</span> })}
                        </span>
                    </label>
                }
            })
            .collect_view();
        let select_all = move |_| {
            let k = is_4k.get_untracked();
            detail.with_untracked(|d| {
                if let Some(d) = d {
                    selected.set(
                        d.seasons
                            .iter()
                            .filter(|s| season_blocked(s, k).is_none())
                            .map(|s| s.season_number)
                            .collect(),
                    );
                }
            });
        };
        Some(
            view! {
                <fieldset class="season-picker">
                    <legend class="field-label">
                        "Seasons"
                        <span class="season-actions">
                            <button type="button" class="btn btn-sm btn-ghost" on:click=select_all>"All"</button>
                            <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| selected.set(Vec::new())>
                                "None"
                            </button>
                        </span>
                    </legend>
                    <div class="season-list">{rows}</div>
                </fieldset>
            }
            .into_any(),
        )
    };

    let advanced_fields = move || {
        if !advanced.get() {
            return None;
        }
        Some(match options.get() {
            None | Some(None) => view! { <p class="muted">"Loading options…"</p> }.into_any(),
            Some(Some(Err(e))) => view! { <p class="text-danger">{e.message}</p> }.into_any(),
            Some(Some(Ok(o))) => view! {
                <div class="field-row">
                    <div class="field">
                        <label for="req-profile">"Quality profile"</label>
                        <select id="req-profile" class="input" prop:value=move || profile_id.get()
                            on:change=move |e| profile_id.set(event_target_value(&e))>
                            {o.profiles.iter().map(|p| view! { <option value=p.id.to_string()>{p.name.clone()}</option> }).collect_view()}
                        </select>
                    </div>
                    <div class="field">
                        <label for="req-root">"Root folder"</label>
                        <select id="req-root" class="input" prop:value=move || root_id.get()
                            on:change=move |e| root_id.set(event_target_value(&e))>
                            {o.root_folders.iter().map(|r| view! { <option value=r.id.to_string()>{r.path.clone()}</option> }).collect_view()}
                        </select>
                    </div>
                </div>
            }
            .into_any(),
        })
    };

    let can_4k = move || detail.with(|d| d.as_ref().is_some_and(|d| d.request_ability.can_request_4k));
    let can_adv = move || detail.with(|d| d.as_ref().is_some_and(|d| d.request_ability.can_request_advanced));

    view! {
        <Modal open=open title=title.get_untracked()>
            <form class="form" on:submit=submit>
                {quota_view}
                {season_picker}
                <Show when=can_4k>
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || is_4k.get()
                            on:change=move |e| is_4k.set(event_target_checked(&e)) />
                        <span>"Request in 4K"</span>
                    </label>
                </Show>
                <Show when=can_adv>
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || advanced.get()
                            on:change=move |e| advanced.set(event_target_checked(&e)) />
                        <span>"Advanced: choose quality profile and root folder"</span>
                    </label>
                    {advanced_fields}
                </Show>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        <Icon name="requests" />
                        {move || if busy.get() { "Requesting…" } else { "Request" }}
                    </button>
                </div>
            </form>
        </Modal>
    }
}

#[component]
pub fn ReportIssueModal(
    open: RwSignal<bool>,
    #[prop(into)] detail: Signal<Option<MediaDetailDto>>,
    #[prop(optional)] on_reported: Option<Callback<IssueDto>>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let issue_type = RwSignal::new(IssueType::Video);
    let season = RwSignal::new(String::new());
    let episode = RwSignal::new(String::new());
    let message = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    Effect::new(move |_| {
        if open.get() {
            issue_type.set(IssueType::Video);
            season.set(String::new());
            episode.set(String::new());
            message.set(String::new());
        }
    });

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(d) = detail.get_untracked() else { return };
        let msg = message.get_untracked().trim().to_string();
        if msg.is_empty() {
            toasts.error("Describe the problem so an admin can fix it.");
            return;
        }
        let season_n = season.get_untracked().parse::<u32>().ok();
        let req = CreateIssueDto {
            kind: d.kind,
            tmdb_id: d.tmdb_id,
            issue_type: issue_type.get_untracked(),
            season: season_n,
            episode: season_n.and(episode.get_untracked().trim().parse::<u32>().ok()),
            message: msg,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, IssueDto>("/issues", &req).await {
                Ok(issue) => {
                    toasts.success("Thanks — the issue was reported.");
                    open.set(false);
                    if let Some(cb) = on_reported {
                        cb.run(issue);
                    }
                }
                Err(e) => toasts.error(format!("Could not report the issue: {e}")),
            }
            busy.set(false);
        });
    };

    let is_series = move || detail.with(|d| d.as_ref().is_some_and(|d| d.kind == MediaKind::Series));
    let seasons = move || detail.with(|d| d.as_ref().map(|d| d.seasons.clone()).unwrap_or_default());

    view! {
        <Modal open=open title="Report an issue">
            <form class="form" on:submit=submit>
                <fieldset class="segmented" aria-label="Issue type">
                    {ISSUE_TYPES
                        .into_iter()
                        .map(|t| {
                            view! {
                                <button
                                    type="button"
                                    class="seg-btn"
                                    class:active=move || issue_type.get() == t
                                    aria-pressed=move || (issue_type.get() == t).to_string()
                                    on:click=move |_| issue_type.set(t)
                                >
                                    {issue_type_label(t)}
                                </button>
                            }
                        })
                        .collect_view()}
                </fieldset>
                <Show when=is_series>
                    <div class="field-row">
                        <div class="field">
                            <label for="issue-season">"Season"</label>
                            <select id="issue-season" class="input" prop:value=move || season.get()
                                on:change=move |e| season.set(event_target_value(&e))>
                                <option value="">"All seasons"</option>
                                {move || seasons().into_iter().map(|s| {
                                    let n = s.season_number;
                                    view! { <option value=n.to_string()>{season_name(&s)}</option> }
                                }).collect_view()}
                            </select>
                        </div>
                        <div class="field">
                            <label for="issue-episode">"Episode " <span class="optional">"(optional)"</span></label>
                            <input id="issue-episode" class="input" type="number" min="1" inputmode="numeric"
                                disabled=move || season.get().is_empty()
                                prop:value=move || episode.get()
                                on:input=move |e| episode.set(event_target_value(&e)) />
                        </div>
                    </div>
                </Show>
                <div class="field">
                    <label for="issue-message">"What's wrong?"</label>
                    <textarea id="issue-message" class="input textarea" rows="4"
                        placeholder="e.g. Audio is out of sync from 12:30 onwards"
                        prop:value=move || message.get()
                        on:input=move |e| message.set(event_target_value(&e))></textarea>
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || if busy.get() { "Sending…" } else { "Report issue" }}
                    </button>
                </div>
            </form>
        </Modal>
    }
}
