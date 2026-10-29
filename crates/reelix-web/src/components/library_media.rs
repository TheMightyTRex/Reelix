//! Media file tables (with ffprobe media info) and the cast strip used on detail pages.

use leptos::prelude::*;
use reelix_api_types::library::{CastDto, MediaFileDto, MediaInfoDto};

use super::Icon;
use super::library_common::bytes;
use super::poster::{PROFILE, Poster, tmdb_image};

fn channels(c: f32) -> String {
    match c {
        c if (c - 2.0).abs() < 0.01 => "Stereo".into(),
        c if (c - 1.0).abs() < 0.01 => "Mono".into(),
        c => format!("{c:.1}"),
    }
}

/// One-line summary: `x265 · 3840×2160 · HDR10 · TrueHD Atmos 7.1`.
pub fn media_summary(mi: &MediaInfoDto) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &mi.video_codec {
        parts.push(v.clone());
    }
    if let (Some(w), Some(h)) = (mi.width, mi.height) {
        parts.push(format!("{w}×{h}"));
    }
    if !mi.hdr.is_empty() {
        parts.push(mi.hdr.join(" / "));
    }
    if let Some(a) = &mi.audio_codec {
        let ch = mi.audio_channels.map(channels).unwrap_or_default();
        parts.push(format!("{a} {ch}").trim().to_string());
    }
    parts.join(" · ")
}

fn kv(label: &'static str, value: Option<String>) -> impl IntoView {
    value.filter(|v| !v.is_empty()).map(|v| {
        view! {
            <div class="kv-row">
                <dt>{label}</dt>
                <dd>{v}</dd>
            </div>
        }
    })
}

/// Full media-info list for one file.
#[component]
pub fn MediaInfoList(file: MediaFileDto) -> impl IntoView {
    let mi = file.media_info.clone().unwrap_or_default();
    let langs: Vec<&str> = file.languages.iter().map(|l| l.name()).collect();
    view! {
        <dl class="kv media-info">
            {kv("Path", Some(file.path.clone()))}
            {kv("Release name", file.scene_name.clone())}
            {kv("Release group", file.release_group.clone())}
            {kv("Edition", file.edition.clone())}
            {kv("Languages", Some(langs.join(", ")))}
            {kv("Custom formats", Some(file.custom_formats.join(", ")))}
            {kv("Format score", Some(format!("{:+}", file.custom_format_score)))}
            {kv("Container", Some(mi.container.clone()))}
            {kv("Duration", mi.duration_secs.map(|d| crate::format::duration_secs(d as u64)))}
            {kv("Video", Some(
                [mi.video_codec.clone(), mi.bit_depth.map(|b| format!("{b}-bit")), mi.fps.map(|f| format!("{f:.3} fps")),
                 mi.video_bitrate.map(|b| format!("{} kbps", b / 1000))]
                    .into_iter().flatten().collect::<Vec<_>>().join(" · ")))}
            {kv("Resolution", mi.width.zip(mi.height).map(|(w, h)| format!("{w}×{h}")))}
            {kv("HDR", Some(mi.hdr.join(", ")))}
            {kv("Audio", Some(
                [mi.audio_codec.clone(), mi.audio_channels.map(channels),
                 (mi.audio_stream_count > 1).then(|| format!("{} streams", mi.audio_stream_count))]
                    .into_iter().flatten().collect::<Vec<_>>().join(" · ")))}
            {kv("Audio languages", Some(mi.audio_languages.join(", ")))}
            {kv("Subtitles", Some(mi.subtitle_languages.join(", ")))}
            {kv("Original path", file.original_path.clone())}
            {kv("Added", Some(crate::format::datetime(file.date_added)))}
        </dl>
    }
}

/// Files table. Each row expands to its media info. `label` is an optional first column
/// (e.g. `S01E02`).
#[component]
pub fn MediaFilesTable(
    files: Vec<(Option<String>, MediaFileDto)>,
    on_delete: Callback<i64>,
) -> impl IntoView {
    let has_label = files.iter().any(|(l, _)| l.is_some());
    let expanded = RwSignal::new(Vec::<i64>::new());
    view! {
        <div class="table-wrap">
            <table class="table files-table">
                <caption class="visually-hidden">"Files"</caption>
                <thead>
                    <tr>
                        {has_label.then(|| view! { <th scope="col">"Episode"</th> })}
                        <th scope="col">"File"</th>
                        <th scope="col">"Quality"</th>
                        <th scope="col">"Media info"</th>
                        <th scope="col">"Size"</th>
                        <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                    </tr>
                </thead>
                <tbody>
                    {files.into_iter().map(|(label, f)| {
                        let fid = f.id;
                        let open = move || expanded.with(|e| e.contains(&fid));
                        let summary = f.media_info.as_ref().map(media_summary).unwrap_or_else(|| "—".into());
                        let cols = if has_label { 6 } else { 5 };
                        let detail = f.clone();
                        view! {
                            <tr>
                                {has_label.then(|| view! { <td class="cell-strong">{label.clone().unwrap_or_default()}</td> })}
                                <td>
                                    <span class="mono file-name">{f.relative_path.clone()}</span>
                                    {f.release_group.clone().map(|g| view! { <span class="cell-sub">{g}</span> })}
                                </td>
                                <td>
                                    <span class="badge" class:tone-success=f.cutoff_met class:tone-warning=!f.cutoff_met
                                        title=if f.cutoff_met { "Cutoff met" } else { "Upgrade wanted" }>
                                        {f.quality_name.clone()}
                                    </span>
                                    {(!f.custom_formats.is_empty()).then(|| view! {
                                        <span class="cell-sub">{format!("{} ({:+})", f.custom_formats.join(", "), f.custom_format_score)}</span>
                                    })}
                                </td>
                                <td>{summary}</td>
                                <td>{bytes(f.size)}</td>
                                <td class="cell-actions">
                                    <button type="button" class="btn btn-sm btn-ghost" aria-expanded=move || open().to_string()
                                        on:click=move |_| super::library_common::toggle_id(expanded, fid)>
                                        <Icon name="info" />
                                        <span class="visually-hidden">"Media info"</span>
                                    </button>
                                    <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label="Delete file"
                                        on:click=move |_| on_delete.run(fid)>
                                        <Icon name="trash" />
                                    </button>
                                </td>
                            </tr>
                            <Show when=open>
                                <tr class="file-detail">
                                    <td colspan=cols>
                                        <MediaInfoList file=detail.clone() />
                                    </td>
                                </tr>
                            </Show>
                        }
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }
}

/// Horizontally scrolling cast strip.
#[component]
pub fn CastRow(cast: Vec<CastDto>) -> impl IntoView {
    view! {
        <div class="cast-row">
            {cast.into_iter().map(|c| view! {
                <div class="cast-card">
                    <Poster title=c.name.clone() url=tmdb_image(c.profile_url.as_deref(), PROFILE) />
                    <span class="cast-name">{c.name}</span>
                    {c.character.map(|ch| view! { <span class="cast-role">{ch}</span> })}
                </div>
            }).collect_view()}
        </div>
    }
}
