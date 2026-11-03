//! Optional yt-dlp backend for YouTube. yt-dlp lists playlists and downloads the
//! selected streams; Reelix still converts, tags and publishes the files itself.
//! URLs are normalized by `validate` and passed after `--`, so they cannot become options.
use crate::{
    DirectService,
    process::{command, safe_name},
    youtube::channel_artist,
};
use anyhow::{Context, Result, anyhow, bail};
use reelix_api_types::direct::*;
use serde::Deserialize;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    sync::mpsc,
};

const PROGRESS: &str = "REELIX-PROGRESS ";
const PROGRESS_TEMPLATE: &str = "download:REELIX-PROGRESS %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s";
/// Printed once a stream is in its final place, so the path reflects the real extension.
const FILE_TEMPLATE: &str = "after_move:%(.{filepath,title,channel,uploader,artists,artist,album,track})j";
/// Original-audio containers Reelix publishes unchanged.
const ORIGINAL_AUDIO: [&str; 2] = ["m4a", "webm"];

/// The subset of yt-dlp's info JSON Reelix reads, for videos, playlists and entries.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Info {
    pub id: Option<String>,
    pub title: Option<String>,
    pub channel: Option<String>,
    pub uploader: Option<String>,
    /// Music metadata, set for YouTube Music tracks.
    pub artists: Option<Vec<String>>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track: Option<String>,
    pub filepath: Option<String>,
    pub entries: Option<Vec<Option<Info>>>,
}

impl Info {
    /// The primary artist, falling back to the channel.
    fn artist_name(&self) -> &str {
        let artist = self.artists.as_ref().and_then(|a| a.first()).map(String::as_str);
        let artist = artist.or_else(|| self.artist.as_deref().and_then(|a| a.split(',').next()));
        match artist.map(str::trim).filter(|a| !a.is_empty()) {
            Some(artist) => artist,
            None => channel_artist(
                self.channel
                    .as_deref()
                    .or(self.uploader.as_deref())
                    .unwrap_or("YouTube"),
            ),
        }
    }
}

pub(crate) fn video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// `downloaded total estimate`, each a byte count or `NA`.
pub(crate) fn parse_progress(value: &str) -> Option<f64> {
    let mut parts = value.split_whitespace().map(|v| v.parse::<f64>().ok());
    let downloaded = parts.next()??;
    let (total, estimate) = (parts.next().flatten(), parts.next().flatten());
    let total = total.or(estimate).filter(|t| *t > 0.0)?;
    Some((downloaded / total * 100.0).clamp(0.0, 100.0))
}

/// Find a published file for `id` from an earlier attempt, whatever its title was,
/// in the job folder or its `Artist/Album` subfolders.
async fn existing_output(folder: &Path, id: &str, extensions: &[&str]) -> Result<Option<PathBuf>> {
    let mut pending = vec![(folder.to_path_buf(), 0)];
    while let Some((folder, depth)) = pending.pop() {
        let mut dir = match tokio::fs::read_dir(&folder).await {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().await?.is_dir() {
                if depth < 2 {
                    pending.push((entry.path(), depth + 1));
                }
            } else if extensions
                .iter()
                .any(|ext| name.ends_with(&format!(" [{id}].{ext}")))
            {
                return Ok(Some(entry.path()));
            }
        }
    }
    Ok(None)
}

fn forward(
    pipe: Option<impl AsyncRead + Unpin + Send + 'static>,
    is_err: bool,
    tx: mpsc::UnboundedSender<(bool, String)>,
) {
    let Some(pipe) = pipe else { return };
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut line = Vec::new();
        while matches!(reader.read_until(b'\n', &mut line).await, Ok(n) if n > 0) {
            let text = String::from_utf8_lossy(&line).trim_end().to_owned();
            line.clear();
            if tx.send((is_err, text)).is_err() {
                break;
            }
        }
    });
}

impl DirectService {
    /// Run yt-dlp and return its stdout lines. Progress lines update job `progress_id`;
    /// the last `ERROR:` line becomes the failure message.
    async fn run_ytdlp(
        &self,
        settings: &DirectSettings,
        args: Vec<OsString>,
        progress_id: Option<i64>,
    ) -> Result<Vec<String>> {
        let mut cmd = command(settings.yt_dlp.trim());
        cmd.args(["--ignore-config", "--newline", "--encoding", "utf-8"])
            .arg("--cache-dir")
            .arg(self.data_dir.join("yt-dlp"));
        if Path::new(&settings.ffmpeg).is_absolute() {
            cmd.arg("--ffmpeg-location").arg(&settings.ffmpeg);
        }
        let mut child = cmd
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                anyhow!("Cannot start yt-dlp ({e}); check its path in Download settings, or clear it to use the built-in extractor")
            })?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        forward(child.stdout.take(), false, tx.clone());
        forward(child.stderr.take(), true, tx);
        let mut stdout = Vec::new();
        let mut error = None;
        let mut last = Instant::now() - Duration::from_secs(1);
        while let Some((is_err, line)) = rx.recv().await {
            if let Some(value) = line.trim_start().strip_prefix(PROGRESS) {
                if let Some(id) = progress_id.filter(|_| last.elapsed() >= Duration::from_millis(500)) {
                    self.progress(id, parse_progress(value), "Downloading media")
                        .await?;
                    last = Instant::now();
                }
            } else if is_err {
                if let Some(message) = line.trim().strip_prefix("ERROR:") {
                    error = Some(message.trim().to_owned());
                }
            } else {
                stdout.push(line);
            }
        }
        let status = child.wait().await?;
        if !status.success() {
            bail!(
                "yt-dlp failed: {}",
                error.unwrap_or_else(|| format!("exited with {status}"))
            );
        }
        Ok(stdout)
    }

    /// The URL's title and videos (id, title, artist), and whether the list was cut short.
    async fn ytdlp_list(
        &self,
        settings: &DirectSettings,
        r: &DirectRequest,
        preview: bool,
    ) -> Result<(String, Vec<Info>, bool)> {
        let mut args: Vec<OsString> = vec![
            "--flat-playlist".into(),
            "--dump-single-json".into(),
            "--ignore-no-formats-error".into(),
        ];
        if !r.playlist {
            args.push("--no-playlist".into());
        } else if preview {
            args.extend(["--playlist-end".into(), "51".into()]);
        }
        args.extend(["--".into(), r.url.clone().into()]);
        let out = self.run_ytdlp(settings, args, None).await?;
        let json = out
            .iter()
            .rev()
            .find(|l| l.starts_with('{'))
            .context("yt-dlp returned no video information")?;
        let mut info: Info = serde_json::from_str(json).context("yt-dlp returned unreadable JSON")?;
        let title = info.title.clone().unwrap_or_default();
        let entries = match info.entries.take() {
            Some(entries) => entries.into_iter().flatten().collect(),
            None => vec![info],
        };
        let limit = if preview { 50 } else { 10_000 };
        if !preview && entries.len() > limit {
            bail!("YouTube playlist exceeds the 10,000-item limit");
        }
        let truncated = entries.len() > 50;
        let mut seen = std::collections::HashSet::new();
        let items = entries
            .into_iter()
            .filter(|e| e.id.as_deref().is_some_and(video_id))
            .filter(|e| seen.insert(e.id.clone()))
            .take(limit)
            .collect();
        Ok((title, items, truncated))
    }

    pub(crate) async fn ytdlp_preview(
        &self,
        settings: &DirectSettings,
        r: &DirectRequest,
    ) -> Result<DirectPreview> {
        let (title, items, truncated) = self.ytdlp_list(settings, r, true).await?;
        let titles: Vec<String> = items
            .into_iter()
            .map(|e| e.title.or(e.id).unwrap_or_default())
            .collect();
        let title = if r.playlist {
            title
        } else {
            titles.first().cloned().unwrap_or(title)
        };
        Ok(DirectPreview {
            title,
            items: titles,
            truncated,
        })
    }

    /// Download one stream for `id` as `.<id>.<kind>.<ext>` in the job folder.
    async fn ytdlp_stream(
        &self,
        settings: &DirectSettings,
        job: &DirectDownload,
        id: &str,
        kind: &str,
        format: String,
    ) -> Result<(PathBuf, Info)> {
        let folder = Path::new(&job.output_dir);
        let prefix = format!(".{id}.{kind}.");
        let args: Vec<OsString> = vec![
            "--no-playlist".into(),
            "-f".into(),
            format.into(),
            "-P".into(),
            folder.into(),
            "-o".into(),
            format!("{prefix}%(ext)s").into(),
            "--progress".into(),
            "--progress-template".into(),
            PROGRESS_TEMPLATE.into(),
            "--print".into(),
            FILE_TEMPLATE.into(),
            "--".into(),
            format!("https://www.youtube.com/watch?v={id}").into(),
        ];
        let out = self.run_ytdlp(settings, args, Some(job.id)).await?;
        let info: Info = out
            .iter()
            .rev()
            .find_map(|l| serde_json::from_str(l).ok())
            .context("yt-dlp did not report the downloaded file")?;
        // Only trust the file name, and only one we asked for, inside the job folder.
        let name = info
            .filepath
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .filter(|n| n.starts_with(&prefix))
            .context("yt-dlp reported an unexpected file name")?;
        Ok((folder.join(name), info))
    }

    pub(crate) async fn download_youtube_ytdlp(
        &self,
        job: &DirectDownload,
        settings: &DirectSettings,
    ) -> Result<()> {
        let r = &job.request;
        let mut playlist = String::new();
        let ids: Vec<String> = if r.playlist {
            let (title, items, _) = self.ytdlp_list(settings, r, false).await?;
            playlist = title.strip_prefix("Album - ").unwrap_or(&title).to_owned();
            items.into_iter().filter_map(|e| e.id).collect()
        } else {
            let url = url::Url::parse(&r.url)?;
            let id = url
                .query_pairs()
                .find(|(k, _)| k == "v")
                .context("Missing video ID")?
                .1
                .into_owned();
            vec![id]
        };
        if ids.is_empty() {
            bail!("YouTube playlist has no downloadable videos");
        }
        let original = r.mode == DownloadMode::Audio && r.format == "original";
        let extensions: Vec<&str> = if original {
            ORIGINAL_AUDIO.to_vec()
        } else {
            vec![r.format.as_str()]
        };
        let folder = Path::new(&job.output_dir);
        let total = ids.len();
        for (index, id) in ids.iter().enumerate() {
            if existing_output(folder, id, &extensions).await?.is_some() {
                continue;
            }
            self.progress(
                job.id,
                None,
                &format!("Downloading video {} of {total}", index + 1),
            )
            .await?;
            let mut inputs = Vec::new();
            if r.mode == DownloadMode::Video {
                let format = match r.max_height {
                    Some(h) => format!("bv*[height<={h}]"),
                    None => "bv*".into(),
                };
                let (video, _) = self.ytdlp_stream(settings, job, id, "video", format).await?;
                inputs.push(video);
            }
            let (audio, info) = self.ytdlp_stream(settings, job, id, "audio", "ba".into()).await?;
            let extension = if original {
                let ext = audio.extension().and_then(|e| e.to_str()).unwrap_or_default();
                if !ORIGINAL_AUDIO.contains(&ext) {
                    bail!("Unsupported YouTube audio container {ext}; choose a conversion format");
                }
                ext.to_owned()
            } else {
                r.format.clone()
            };
            inputs.push(audio);
            let title = info.title.as_deref().unwrap_or(id);
            let artist = info.artist_name();
            let album = info.album.as_deref().or(Some(playlist.as_str()));
            let output = Self::youtube_folder(job, artist, album)
                .join(format!("{} [{id}].{extension}", safe_name(title)));
            let tag_title = info.track.as_deref().unwrap_or(title);
            self.publish_youtube(job, settings, inputs, &output, id, tag_title, artist, album)
                .await?;
        }
        Ok(())
    }
}
