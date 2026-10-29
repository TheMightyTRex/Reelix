//! `/settings/parse` — paste a release name, see what the parser extracted, which custom
//! formats match, the score and whether a quality profile would accept it.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::profiles::{ParseRequest, ParseResponse, ParsedRelease, QualityProfile, ReleaseKind};

use crate::api;
use crate::components::{Card, EmptyState, ErrorBox, Icon, PageHeader, StatusBadge, Tone};

fn join<T: serde::Serialize>(items: &[T]) -> String {
    if items.is_empty() {
        return "—".into();
    }
    items
        .iter()
        .map(|i| match serde_json::to_value(i) {
            Ok(serde_json::Value::String(s)) => s,
            Ok(v) => v.to_string(),
            Err(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn opt(s: Option<String>) -> String {
    s.filter(|s| !s.is_empty()).unwrap_or_else(|| "—".into())
}

fn nums(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

/// (label, value) rows describing a parsed release.
fn fields(p: &ParsedRelease) -> Vec<(&'static str, String)> {
    let mut rows = vec![
        ("Title", opt(Some(p.title.clone()))),
        ("Year", opt(p.year.map(|y| y.to_string()))),
    ];
    match &p.kind {
        ReleaseKind::Movie => rows.push(("Type", "Movie".into())),
        ReleaseKind::Unknown => rows.push(("Type", "Unknown".into())),
        ReleaseKind::Episode(e) => {
            let kind = if e.air_date.is_some() {
                "Daily episode"
            } else if e.full_season {
                "Season pack"
            } else if !e.absolute_episodes.is_empty() {
                "Anime (absolute)"
            } else {
                "Episode"
            };
            rows.push(("Type", kind.into()));
            rows.push(("Season", opt(e.season.map(|s| s.to_string()))));
            if let Some((a, b)) = e.season_range {
                rows.push(("Seasons", format!("{a}–{b}")));
            }
            if !e.episodes.is_empty() {
                rows.push(("Episodes", nums(&e.episodes)));
            }
            if !e.absolute_episodes.is_empty() {
                rows.push(("Absolute", nums(&e.absolute_episodes)));
            }
            if let Some(d) = e.air_date {
                rows.push(("Air date", d.to_string()));
            }
            if e.is_special {
                rows.push(("Special", "Yes".into()));
            }
        }
    }
    let rev = p.quality.revision;
    let mut revision = format!("v{}", rev.version);
    if rev.real > 0 {
        revision.push_str(&format!(" · REAL×{}", rev.real));
    }
    if rev.is_repack {
        revision.push_str(" · repack");
    }
    rows.extend([
        ("Quality", p.quality.quality.name().to_string()),
        ("Revision", revision),
        (
            "Languages",
            p.languages
                .iter()
                .map(|l| l.name())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ("Release group", opt(p.release_group.clone())),
        ("Edition", opt(p.edition.clone())),
        ("HDR", join(&p.hdr)),
        ("Video codec", opt(p.video_codec.map(|c| join(&[c])))),
        ("Audio", join(&p.audio_codecs)),
        ("Channels", opt(p.audio_channels.map(|c| format!("{c:.1}")))),
        ("Streaming service", opt(p.streaming_service.clone())),
        ("3D", if p.is_3d { "Yes".into() } else { "No".into() }),
        (
            "Hardcoded subs",
            if p.is_hardcoded_subs {
                "Yes".into()
            } else {
                "No".into()
            },
        ),
        ("Hash", opt(p.hash.clone())),
    ]);
    rows
}

#[component]
pub fn ParsePage() -> impl IntoView {
    let profiles = LocalResource::new(|| api::get::<Vec<QualityProfile>>("/qualityprofiles"));
    let title = RwSignal::new(String::new());
    let profile_id = RwSignal::new(None::<i64>);
    let result = RwSignal::new(None::<Result<ParseResponse, api::ApiError>>);
    let busy = RwSignal::new(false);

    let run = move || {
        let t = title.get_untracked().trim().to_string();
        if t.is_empty() {
            return;
        }
        busy.set(true);
        let req = ParseRequest {
            title: t,
            profile_id: profile_id.get_untracked(),
            size: None,
        };
        spawn_local(async move {
            result.set(Some(api::post::<_, ParseResponse>("/parse", &req).await));
            busy.set(false);
        });
    };
    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        run();
    };

    let output = move || {
        match result.get() {
        None => view! {
            <EmptyState icon="search" title="Paste a release name" description="Parsed fields, matched custom formats and the profile decision appear here." compact=true />
        }
        .into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(r)) => {
            let decision = r.decision.clone().map(|d| {
                let (tone, label) = if d.accepted { (Tone::Success, "Accepted") } else { (Tone::Danger, "Rejected") };
                view! {
                    <Card title=format!("Profile: {}", d.profile_name)>
                        <StatusBadge tone=tone label=label dot=true />
                        {(!d.rejections.is_empty()).then(|| view! {
                            <ul class="reason-list">
                                {d.rejections.iter().map(|x| view! {
                                    <li>
                                        <span class=format!("badge {}", if x.kind == "temporary" { "tone-warning" } else { "tone-danger" })>{x.kind.clone()}</span>
                                        " " {x.reason.clone()}
                                    </li>
                                }).collect_view()}
                            </ul>
                        })}
                    </Card>
                }
            });
            view! {
                <div class="split-grid">
                    <Card title="Parsed">
                        <dl class="kv">
                            {fields(&r.parsed).into_iter().map(|(k, v)| view! {
                                <div class="kv-row"><dt>{k}</dt><dd>{v}</dd></div>
                            }).collect_view()}
                        </dl>
                    </Card>
                    <div class="stack">
                        <Card title="Custom formats" subtitle=format!("Total score: {}", r.format_score)>
                            {if r.matched_formats.is_empty() {
                                view! { <p class="muted">"No custom formats match."</p> }.into_any()
                            } else {
                                view! {
                                    <div class="chip-list">
                                        {r.matched_formats.iter().map(|f| view! {
                                            <span class="chip">{format!("{} ({:+})", f.name, f.score)}</span>
                                        }).collect_view()}
                                    </div>
                                }.into_any()
                            }}
                        </Card>
                        {decision}
                    </div>
                </div>
            }
            .into_any()
        }
    }
    };

    view! {
        <PageHeader title="Parse Release" subtitle="Test how Reelix reads a release name and how a profile scores it." />
        <div class="stack">
            <Card>
                <form class="form parse-form" on:submit=submit>
                    <div class="field">
                        <label for="parse-title">"Release name"</label>
                        <input
                            id="parse-title"
                            class="input mono"
                            type="text"
                            autofocus
                            placeholder="Show.Name.S02E05.1080p.WEB-DL.DDP5.1.Atmos.H.264-GROUP"
                            prop:value=move || title.get()
                            on:input=move |e| title.set(event_target_value(&e))
                        />
                    </div>
                    <div class="field-row">
                        <div class="field">
                            <label for="parse-profile">"Quality profile"</label>
                            <select
                                id="parse-profile"
                                class="input"
                                on:change=move |e| {
                                    profile_id.set(event_target_value(&e).parse().ok());
                                    if !title.get_untracked().trim().is_empty() {
                                        run();
                                    }
                                }
                            >
                                <option value="">"None (parse only)"</option>
                                {move || profiles.get().and_then(Result::ok).unwrap_or_default().into_iter().map(|p| {
                                    view! { <option value=p.id.to_string() selected=move || profile_id.get() == Some(p.id)>{p.name}</option> }
                                }).collect_view()}
                            </select>
                        </div>
                        <div class="field field-check">
                            <button type="submit" class="btn btn-primary" disabled=move || busy.get() || title.with(|t| t.trim().is_empty())>
                                <Icon name="search" />
                                {move || if busy.get() { "Parsing…" } else { "Parse" }}
                            </button>
                        </div>
                    </div>
                </form>
            </Card>
            {output}
        </div>
    }
}
