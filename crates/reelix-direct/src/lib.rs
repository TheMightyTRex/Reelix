//! A durable, single-worker queue independent of torrent/Usenet clients.
mod process;
pub mod spotify;
#[cfg(test)]
mod tests;
mod transfer;
mod youtube;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reelix_api_types::direct::*;
use reelix_db::{Db, now_ms};
use sqlx::Row;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const SETTINGS: &str = "direct_downloads";

pub struct DirectService {
    db: Db,
    data_dir: PathBuf,
    /// Serialises mutations with claiming and process completion.
    gate: Mutex<()>,
    http: reqwest::Client,
}

impl DirectService {
    pub fn new(db: Db, data_dir: PathBuf) -> Self {
        let data_dir = std::path::absolute(&data_dir).unwrap_or(data_dir);
        Self {
            db,
            data_dir,
            gate: Mutex::new(()),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(30))
                .build()
                .expect("valid HTTP client configuration"),
        }
    }

    pub async fn settings(&self) -> Result<DirectSettings> {
        let mut settings = self
            .db
            .settings()
            .get::<DirectSettings>(SETTINGS)
            .await?
            .unwrap_or_default();
        if settings.output_dir.is_empty() {
            settings.output_dir = self.data_dir.join("downloads").to_string_lossy().into_owned();
        }
        Ok(settings)
    }

    pub async fn save_settings(&self, settings: DirectSettings) -> Result<()> {
        if !Path::new(&settings.output_dir).is_absolute() {
            bail!("Download folder must be an absolute server path");
        }
        for tool in [&settings.ffmpeg] {
            if tool.trim().is_empty() || tool.contains(['\n', '\r', '\0']) {
                bail!("Executable paths must be nonempty paths or executable names");
            }
        }
        if !settings.spotify_credentials.is_empty() && !Path::new(&settings.spotify_credentials).is_absolute()
        {
            bail!("Spotify credentials must use an absolute server path");
        }
        self.db.settings().set(SETTINGS, &settings).await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<DirectDownload>> {
        let rows = sqlx::query("SELECT * FROM direct_downloads ORDER BY id DESC LIMIT 200")
            .fetch_all(self.db.pool())
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok(DirectDownload {
                    id: r.try_get("id")?,
                    request: serde_json::from_str(r.try_get("request")?)?,
                    state: r.try_get("state")?,
                    progress: r.try_get("progress")?,
                    message: r.try_get("message")?,
                    output_dir: r.try_get("output_dir")?,
                    created_at: r.try_get("created_at")?,
                    updated_at: r.try_get("updated_at")?,
                })
            })
            .collect()
    }

    pub async fn enqueue(&self, mut request: DirectRequest) -> Result<DirectDownload> {
        let settings = self.settings().await?;
        if !settings.enabled {
            bail!("Enable direct downloads in settings first");
        }
        request.url = validate(&request)?;
        if request.source == DirectSource::Spotify && request.spotify_quality.is_none() {
            request.spotify_quality = Some("auto".into());
        }
        if request.source == DirectSource::Spotify && !Path::new(&settings.spotify_credentials).is_file() {
            bail!("Configure a Spotify credentials file; create one with reelix spotify-login");
        }
        let key = serde_json::to_string(&request)?;
        let _guard = self.gate.lock().await;
        let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM direct_downloads WHERE dedup_key = ? AND state IN ('queued','running','completed')")
            .bind(&key).fetch_optional(self.db.pool()).await?;
        if existing.is_some() {
            bail!("This download is already queued or completed");
        }
        let now = now_ms();
        let mut tx = self.db.pool().begin().await?;
        let id = sqlx::query("INSERT INTO direct_downloads (request,dedup_key,output_dir,created_at,updated_at) VALUES (?,?,?,?,?)")
            .bind(&key).bind(&key).bind(&settings.output_dir).bind(now).bind(now)
            .execute(&mut *tx).await?.last_insert_rowid();
        let folder = Path::new(&settings.output_dir).join(format!("reelix-{id}"));
        sqlx::query("UPDATE direct_downloads SET output_dir = ? WHERE id = ?")
            .bind(folder.to_string_lossy().as_ref())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.get(id).await
    }

    async fn get(&self, id: i64) -> Result<DirectDownload> {
        self.list()
            .await?
            .into_iter()
            .find(|j| j.id == id)
            .context("Download not found in recent queue")
    }

    pub async fn cancel(&self, id: i64) -> Result<()> {
        let _guard = self.gate.lock().await;
        let result = sqlx::query("UPDATE direct_downloads SET state='cancelled', message='Cancelled', updated_at=? WHERE id=? AND state IN ('queued','running')")
            .bind(now_ms()).bind(id).execute(self.db.pool()).await?;
        if result.rows_affected() == 0 {
            bail!("Download is missing or is no longer active");
        }
        Ok(())
    }

    pub async fn retry(&self, id: i64) -> Result<()> {
        let _guard = self.gate.lock().await;
        if !self.settings().await?.enabled {
            bail!("Direct downloads are disabled");
        }
        let row = sqlx::query(
            "SELECT dedup_key FROM direct_downloads WHERE id=? AND state IN ('failed','cancelled')",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?
        .context("Only failed or cancelled downloads can be retried")?;
        let key: String = row.try_get("dedup_key")?;
        let duplicate: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM direct_downloads WHERE dedup_key=? AND state IN ('queued','running','completed')")
            .bind(key).fetch_one(self.db.pool()).await?;
        if duplicate > 0 {
            bail!("Another copy of this download is queued or completed");
        }
        sqlx::query(
            "UPDATE direct_downloads SET state='queued', progress=NULL, message='', updated_at=? WHERE id=?",
        )
        .bind(now_ms())
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn health(&self) -> Result<Vec<DirectHealth>> {
        let s = self.settings().await?;
        let ffmpeg = process::probe(&s.ffmpeg, "-version").await;
        let credentials = spotify::read_credentials(Path::new(&s.spotify_credentials))
            .await
            .is_ok();
        Ok(vec![
            DirectHealth {
                tool: "Native YouTube extractor".into(),
                available: true,
                detail: "RustyPipe is compiled into Reelix; no yt-dlp or Python process".into(),
            },
            DirectHealth {
                tool: "Native Spotify client".into(),
                available: true,
                detail: "librespot is compiled into Reelix".into(),
            },
            DirectHealth {
                tool: "FFmpeg".into(),
                available: ffmpeg,
                detail: "Used only for conversion and video/audio muxing".into(),
            },
            DirectHealth {
                tool: "Spotify session".into(),
                available: credentials,
                detail: "Credentials file must parse; account access is verified when downloading".into(),
            },
        ])
    }

    pub async fn preview(&self, mut request: DirectRequest) -> Result<DirectPreview> {
        let s = self.settings().await?;
        if !s.enabled {
            bail!("Direct downloads are disabled");
        }
        request.url = validate(&request)?;
        tokio::time::timeout(Duration::from_secs(45), async {
            match request.source {
                DirectSource::Youtube => self.youtube_preview(&request).await,
                DirectSource::Spotify => self.spotify_preview(&s, &request).await,
            }
        })
        .await
        .context("Preview timed out")?
    }
    /// Requeue interrupted work on startup, then process one download at a time.
    pub async fn run(self: Arc<Self>, stop: CancellationToken) -> Result<()> {
        sqlx::query("UPDATE direct_downloads SET state='queued', progress=NULL, message='Resuming after restart' WHERE state='running'")
            .execute(self.db.pool()).await?;
        loop {
            if stop.is_cancelled() {
                return Ok(());
            }
            match self.next().await {
                Ok(Some((job, settings))) => {
                    let outcome = self.execute(&job, &settings, &stop).await;
                    let _guard = self.gate.lock().await;
                    let (state, message) = if stop.is_cancelled() {
                        ("queued", "Interrupted by shutdown; will resume".into())
                    } else {
                        match outcome {
                            Ok(()) => ("completed", "Download complete".into()),
                            Err(e) => ("failed", e.to_string()),
                        }
                    };
                    sqlx::query("UPDATE direct_downloads SET state=?, message=?, updated_at=? WHERE id=? AND state='running'")
                        .bind(state).bind(message).bind(now_ms()).bind(job.id).execute(self.db.pool()).await?;
                }
                Ok(None) => {}
                Err(e) => tracing::error!(error=%e, "direct download worker could not claim job"),
            }
            tokio::select! { _ = stop.cancelled() => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
        }
    }

    async fn next(&self) -> Result<Option<(DirectDownload, DirectSettings)>> {
        let _guard = self.gate.lock().await;
        let settings = self.settings().await?;
        if !settings.enabled {
            return Ok(None);
        }
        let id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM direct_downloads WHERE state='queued' ORDER BY id LIMIT 1")
                .fetch_optional(self.db.pool())
                .await?;
        let Some(id) = id else {
            return Ok(None);
        };
        // Read the exact row rather than the bounded UI history.
        let row =
            sqlx::query("SELECT request,output_dir,created_at,updated_at FROM direct_downloads WHERE id=?")
                .bind(id)
                .fetch_one(self.db.pool())
                .await?;
        let job = DirectDownload {
            id,
            request: serde_json::from_str(row.try_get("request")?)?,
            output_dir: row.try_get("output_dir")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            state: "running".into(),
            progress: None,
            message: String::new(),
        };
        sqlx::query("UPDATE direct_downloads SET state='running',updated_at=? WHERE id=?")
            .bind(now_ms())
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(Some((job, settings)))
    }
}

/// Accept only known public provider URLs; never forward arbitrary URLs or CLI options.
pub fn validate(r: &DirectRequest) -> Result<String> {
    if r.spotify_quality
        .as_deref()
        .is_some_and(|q| !["auto", "normal", "high", "very_high"].contains(&q))
    {
        bail!("Unsupported Spotify quality");
    }
    if r.source != DirectSource::Spotify && r.spotify_quality.is_some() {
        bail!("Spotify quality only applies to Spotify downloads");
    }
    let mut url = url::Url::parse(r.url.trim()).context("Enter a full HTTPS provider URL")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("Use an HTTPS provider URL without credentials or a custom port");
    }
    let host = url.host_str().unwrap_or_default().to_owned();
    let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
    match r.source {
        DirectSource::Youtube => {
            if ![
                "youtube.com",
                "www.youtube.com",
                "m.youtube.com",
                "music.youtube.com",
                "youtu.be",
            ]
            .contains(&host.as_str())
            {
                bail!("Enter a youtube.com or youtu.be URL");
            }
            let pairs: Vec<_> = url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let video = if host == "youtu.be" {
                parts.first().copied().map(str::to_owned)
            } else if parts.first() == Some(&"watch") {
                pairs.iter().find(|(k, _)| k == "v").map(|(_, v)| v.clone())
            } else if matches!(parts.first(), Some(&"shorts" | &"live")) {
                parts.get(1).map(|v| (*v).to_owned())
            } else {
                None
            };
            let list = pairs.iter().find(|(k, _)| k == "list").map(|(_, v)| v.clone());
            let identifier = |v: &str| {
                !v.is_empty()
                    && v.len() <= 150
                    && v.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            };
            url = url::Url::parse("https://www.youtube.com/watch")?;
            if r.playlist {
                let list = list
                    .filter(|s| identifier(s))
                    .context("Playlist mode requires a YouTube playlist URL")?;
                url.set_path("/playlist");
                url.query_pairs_mut().append_pair("list", &list);
            } else {
                let video = video
                    .filter(|s| s.len() == 11 && identifier(s))
                    .context("Enter a YouTube video URL, or enable playlist mode")?;
                url.query_pairs_mut().append_pair("v", &video);
            }
            match r.mode {
                DownloadMode::Audio
                    if !["original", "mp3", "m4a", "opus", "flac"].contains(&r.format.as_str()) =>
                {
                    bail!("Unsupported YouTube audio format")
                }
                DownloadMode::Video if !["mkv", "mp4"].contains(&r.format.as_str()) => {
                    bail!("Video format must be mkv or mp4")
                }
                _ => {}
            }
            if r.mode == DownloadMode::Video
                && !matches!(r.max_height, None | Some(480 | 720 | 1080 | 1440 | 2160))
            {
                bail!("Unsupported video resolution");
            }
        }
        DirectSource::Spotify => {
            if host != "open.spotify.com"
                || parts.len() != 2
                || !["track", "album", "playlist"].contains(&parts[0])
                || parts[1].len() != 22
                || !parts[1].bytes().all(|c| c.is_ascii_alphanumeric())
            {
                bail!("Enter an open.spotify.com track, album, or playlist URL");
            }
            if r.mode != DownloadMode::Audio {
                bail!("Spotify supports audio only");
            }
            if !["original", "ogg", "mp3", "m4a", "flac"].contains(&r.format.as_str()) {
                bail!("Unsupported Spotify audio format");
            }
            url.set_query(None);
        }
    }
    if r.mode == DownloadMode::Audio && r.max_height.is_some() {
        bail!("Audio downloads do not take a video resolution");
    }
    url.set_fragment(None);
    Ok(url.to_string())
}
