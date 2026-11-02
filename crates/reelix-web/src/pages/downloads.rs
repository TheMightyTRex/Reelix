//! Direct downloads, provider setup and persistent queue.
use crate::{
    api,
    components::{Card, PageHeader},
    state::Toasts,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::direct::*;

#[component]
pub fn DownloadsPage() -> impl IntoView {
    super::system::require_admin();
    let toasts = Toasts::expect();
    let jobs = LocalResource::new(|| api::get::<Vec<DirectDownload>>("/downloads"));
    let settings = LocalResource::new(|| api::get::<DirectSettings>("/downloads/settings"));
    let timer = set_interval_with_handle(move || jobs.refetch(), std::time::Duration::from_secs(2)).ok();
    on_cleanup(move || {
        if let Some(t) = timer {
            t.clear();
        }
    });
    let url = RwSignal::new(String::new());
    let source = RwSignal::new("youtube".to_owned());
    let mode = RwSignal::new("audio".to_owned());
    let format = RwSignal::new("original".to_owned());
    let height = RwSignal::new("1080".to_owned());
    let spotify_quality = RwSignal::new("auto".to_owned());
    let playlist = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let preview = RwSignal::new(None::<DirectPreview>);
    let request = move || DirectRequest {
        url: url.get_untracked(),
        source: if source.get_untracked() == "spotify" {
            DirectSource::Spotify
        } else {
            DirectSource::Youtube
        },
        mode: if mode.get_untracked() == "video" {
            DownloadMode::Video
        } else {
            DownloadMode::Audio
        },
        format: format.get_untracked(),
        max_height: if mode.get_untracked() == "video" {
            height.get_untracked().parse().ok()
        } else {
            None
        },
        playlist: playlist.get_untracked(),
        spotify_quality: (source.get_untracked() == "spotify").then(|| spotify_quality.get_untracked()),
    };
    let submit = move |_| {
        busy.set(true);
        let body = request();
        spawn_local(async move {
            match api::post::<_, DirectDownload>("/downloads", &body).await {
                Ok(_) => {
                    toasts.success("Download queued");
                    jobs.refetch();
                    preview.set(None);
                }
                Err(e) => toasts.error(e.to_string()),
            }
            busy.set(false);
        });
    };
    let inspect = move |_| {
        busy.set(true);
        let body = request();
        preview.set(None);
        spawn_local(async move {
            match api::post::<_, DirectPreview>("/downloads/preview", &body).await {
                Ok(p) => preview.set(Some(p)),
                Err(e) => toasts.error(e.to_string()),
            }
            busy.set(false);
        });
    };
    view! {
        <PageHeader title="Downloads" subtitle="Download Spotify music and YouTube audio or video to a server folder." />
        <div class="stack">
            <Card title="New download">
                <div class="field-row">
                    <label class="field">"Source"
                        <select class="input" prop:value=move || source.get() on:change=move |e| {
                            source.set(event_target_value(&e)); mode.set("audio".into()); format.set("original".into()); playlist.set(false); preview.set(None);
                        }><option value="youtube">"YouTube"</option><option value="spotify">"Spotify"</option></select>
                    </label>
                    <label class="field">"URL"
                        <input class="input" type="url" placeholder="Paste a video, track, album, or playlist URL" prop:value=move || url.get()
                            on:input=move |e| { url.set(event_target_value(&e)); preview.set(None); } />
                    </label>
                    <Show when=move || source.get() == "youtube">
                        <label class="field">"Download as"
                            <select class="input" prop:value=move || mode.get() on:change=move |e| {
                                let m = event_target_value(&e); format.set(if m == "video" { "mkv" } else { "original" }.into()); mode.set(m);
                            }><option value="audio">"Audio"</option><option value="video">"Video"</option></select>
                        </label>
                        <label><input type="checkbox" prop:checked=move || playlist.get() on:change=move |e| { playlist.set(event_target_checked(&e)); preview.set(None); } />" Download entire playlist"</label>
                    </Show>
                    <Show when=move || source.get() == "spotify">
                        <label class="field">"Spotify quality"
                            <select class="input" prop:value=move || spotify_quality.get() on:change=move |e| spotify_quality.set(event_target_value(&e))>
                                <option value="auto">"Best for account"</option><option value="normal">"Normal"</option>
                                <option value="high">"High"</option><option value="very_high">"Very high (Premium)"</option>
                            </select>
                        </label>
                    </Show>
                    <label class="field">"Format"
                        {move || if mode.get() == "video" {
                            view! { <select class="input" on:change=move |e| format.set(event_target_value(&e))>
                                <option value="mkv">"MKV"</option><option value="mp4">"MP4"</option>
                            </select> }.into_any()
                        } else {
                            view! { <select class="input" prop:value=move || format.get() on:change=move |e| format.set(event_target_value(&e))>
                                <option value="original">"Original audio"</option><option value="mp3">"MP3"</option>
                                <option value="m4a">"M4A"</option><option value="flac">"FLAC (conversion)"</option>
                                <Show when=move || source.get() == "youtube"><option value="opus">"Opus"</option></Show>
                                <Show when=move || source.get() == "spotify"><option value="ogg">"OGG"</option></Show>
                            </select> }.into_any()
                        }}
                    </label>
                    <Show when=move || mode.get() == "video">
                        <label class="field">"Maximum resolution"
                            <select class="input" prop:value=move || height.get() on:change=move |e| height.set(event_target_value(&e))>
                                <option value="480">"480p"</option><option value="720">"720p"</option><option value="1080">"1080p"</option>
                                <option value="1440">"1440p"</option><option value="2160">"2160p / 4K"</option><option value="">"Best available"</option>
                            </select>
                        </label>
                    </Show>
                </div>
                <p class="muted">"Spotify downloads the tracks in the supplied album or playlist. Converting lossy audio to FLAC does not improve its quality."</p>
                <div class="form-actions">
                    <button class="btn btn-secondary" disabled=move || busy.get() on:click=inspect>"Preview"</button>
                    <button class="btn btn-primary" disabled=move || busy.get() || url.get().trim().is_empty() on:click=submit>{move || if busy.get() { "Working…" } else { "Download" }}</button>
                </div>
                {move || preview.get().map(|p| view! {
                    <h3>{p.title}</h3><ol>{p.items.into_iter().map(|title| view! { <li>{title}</li> }).collect_view()}</ol>
                    <Show when=move || p.truncated><p class="muted">"Showing the first 50 items. Download includes the entire playlist."</p></Show>
                })}
            </Card>
            <Card title="Download queue" subtitle="Latest 200 jobs. Percentage is for the current file; playlists contain multiple files.">
                {move || match jobs.get() {
                    None => view! { <p>"Loading…"</p> }.into_any(),
                    Some(Err(e)) => view! { <p role="alert">{e.to_string()}</p> }.into_any(),
                    Some(Ok(list)) if list.is_empty() => view! { <p class="muted">"No downloads yet."</p> }.into_any(),
                    Some(Ok(list)) => view! { <div class="table-wrap"><table class="data-table">
                        <thead><tr><th>"Download"</th><th>"Status"</th><th>"Destination"</th><th>"Actions"</th></tr></thead>
                        <tbody>{list.into_iter().map(|j| {
                            let active = matches!(j.state.as_str(), "queued" | "running");
                            let retryable = matches!(j.state.as_str(), "failed" | "cancelled");
                            let id = j.id;
                            let action = move |_| { spawn_local(async move {
                                let path = format!("/downloads/{id}/{}", if active { "cancel" } else { "retry" });
                                if let Err(e) = api::post_empty::<()>(&path).await { toasts.error(e.to_string()); }
                                jobs.refetch();
                            }); };
                            view! { <tr>
                                <td><span class="cell-strong">{format!("#{} · {:?} · {:?}", j.id, j.request.source, j.request.mode)}</span><span class="cell-sub">{j.request.url}</span></td>
                                <td>{j.state}{j.progress.map(|p| format!(" · {p:.0}%"))}<span class="cell-sub">{j.message}</span></td>
                                <td class="mono">{j.output_dir}</td>
                                <td><Show when=move || active || retryable><button class="btn btn-secondary btn-sm" on:click=action>{if active { "Cancel" } else { "Retry" }}</button></Show></td>
                            </tr> }
                        }).collect_view()}</tbody>
                    </table></div> }.into_any(),
                }}
            </Card>
            {move || match settings.get() {
                Some(Ok(s)) => view! { <DownloadSettings s=s /> }.into_any(),
                Some(Err(e)) => view! { <p role="alert">{e.to_string()}</p> }.into_any(),
                None => view! { <p>"Loading settings…"</p> }.into_any(),
            }}
        </div>
    }
}

#[component]
fn DownloadSettings(s: DirectSettings) -> impl IntoView {
    let toasts = Toasts::expect();
    let enabled = RwSignal::new(s.enabled);
    let folder = RwSignal::new(s.output_dir);
    let ffmpeg = RwSignal::new(s.ffmpeg);
    let yt_dlp = RwSignal::new(s.yt_dlp);
    let credentials = RwSignal::new(s.spotify_credentials);
    let busy = RwSignal::new(false);
    let health = RwSignal::new(Vec::<DirectHealth>::new());
    let save = move |_| {
        busy.set(true);
        let s = DirectSettings {
            enabled: enabled.get_untracked(),
            output_dir: folder.get_untracked(),
            ffmpeg: ffmpeg.get_untracked(),
            yt_dlp: yt_dlp.get_untracked(),
            spotify_credentials: credentials.get_untracked(),
        };
        spawn_local(async move {
            match api::put::<_, ()>("/downloads/settings", &s).await {
                Ok(_) => {
                    toasts.success("Download settings saved");
                    match api::get::<Vec<DirectHealth>>("/downloads/health").await {
                        Ok(h) => health.set(h),
                        Err(e) => toasts.error(e.to_string()),
                    }
                }
                Err(e) => toasts.error(e.to_string()),
            }
            busy.set(false);
        });
    };
    view! {
        <Card title="Download settings" subtitle="Paths are on the Reelix server. Disabling pauses queued jobs; cancel running jobs separately.">
            <label><input type="checkbox" prop:checked=move || enabled.get() on:change=move |e| enabled.set(event_target_checked(&e)) />" Enable direct downloads"</label>
            <div class="field-row">
                <label class="field">"Download folder"<input class="input" prop:value=move || folder.get() on:input=move |e| folder.set(event_target_value(&e)) /></label>
                <label class="field">"FFmpeg executable"<input class="input" prop:value=move || ffmpeg.get() on:input=move |e| ffmpeg.set(event_target_value(&e)) /></label>
                <label class="field">"yt-dlp executable"<input class="input" placeholder="Empty: built-in extractor" prop:value=move || yt_dlp.get() on:input=move |e| yt_dlp.set(event_target_value(&e)) /></label>
                <label class="field">"Spotify credentials file"<input class="input" prop:value=move || credentials.get() on:input=move |e| credentials.set(event_target_value(&e)) /></label>
            </div>
            <p class="muted">"YouTube uses yt-dlp when a path is set; keep it current with yt-dlp -U, and install Deno so it can read every YouTube format. Clear the path to use the built-in extractor, which breaks more often when YouTube changes."</p>
            <p class="muted">"Run reelix spotify-login --credentials /absolute/path/credentials.json to connect your account, then enter that file path. Spotify list imports remain configured separately under Import Lists."</p>
            <p class="muted">"Files are saved in a separate folder for each job. Use Music → Import Library to match downloaded music into your library."</p>
            <button class="btn btn-primary" disabled=move || busy.get() on:click=save>{move || if busy.get() { "Checking…" } else { "Save and check tools" }}</button>
            <ul>{move || health.get().into_iter().map(|h| view! { <li>{format!("{}: {} — {}", h.tool, if h.available { "OK" } else { "Needs setup" }, h.detail)}</li> }).collect_view()}</ul>
        </Card>
    }
}
