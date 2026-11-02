//! Shared native orchestration and the optional FFmpeg media-processing boundary.
use crate::DirectService;
use anyhow::{Context, Result, bail};
use reelix_api_types::direct::*;
use reelix_db::now_ms;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub(crate) fn safe_name(value: &str) -> String {
    let mut byte_length = 0;
    let value: String = value
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take_while(|c| {
            byte_length += c.len_utf8();
            byte_length <= 120
        })
        .collect();
    let value = value.trim_matches([' ', '.']);
    let value = if value.is_empty() { "untitled" } else { value };
    let stem = value.split('.').next().unwrap_or(value).to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        format!("_{value}")
    } else {
        value.to_owned()
    }
}

fn command(exe: &str) -> Command {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    cmd
}

pub(crate) async fn probe(exe: &str, flag: &str) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(10), command(exe).arg(flag)
        .stdout(Stdio::null()).stderr(Stdio::null()).status()).await, Ok(Ok(s)) if s.success())
}

pub(crate) async fn convert(
    ffmpeg: &str,
    inputs: &[PathBuf],
    output: &Path,
    video: bool,
    format: &str,
) -> Result<()> {
    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    for input in inputs {
        cmd.arg("-i").arg(input);
    }
    if video {
        cmd.args([
            "-map",
            "0:v:0",
            "-map",
            if inputs.len() > 1 { "1:a:0" } else { "0:a:0?" },
            "-c",
            "copy",
        ]);
    } else {
        let codec = match format {
            "mp3" => "libmp3lame",
            "m4a" => "aac",
            "opus" => "libopus",
            "flac" => "flac",
            _ => "copy",
        };
        cmd.args(["-vn", "-c:a", codec]);
    }
    let status = cmd
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .context("Cannot start FFmpeg; check the executable path")?;
    if !status.success() {
        bail!("FFmpeg could not convert/mux the downloaded streams");
    }
    Ok(())
}

impl DirectService {
    pub(crate) async fn progress(&self, id: i64, percent: Option<f64>, message: &str) -> Result<()> {
        sqlx::query(
            "UPDATE direct_downloads SET progress=?,message=?,updated_at=? WHERE id=? AND state='running'",
        )
        .bind(percent)
        .bind(message)
        .bind(now_ms())
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub(crate) async fn execute(
        &self,
        job: &DirectDownload,
        settings: &DirectSettings,
        stop: &CancellationToken,
    ) -> Result<()> {
        tokio::fs::create_dir_all(&job.output_dir).await?;
        let work = async {
            match job.request.source {
                DirectSource::Youtube => self.download_youtube(job, settings).await,
                DirectSource::Spotify => self.download_spotify(job, settings).await,
            }
        };
        self.run_work(job.id, stop, work).await
    }

    pub(crate) async fn run_work(
        &self,
        id: i64,
        stop: &CancellationToken,
        work: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        tokio::pin!(work);
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = stop.cancelled() => bail!("Server stopping"),
                result = &mut work => return result,
                _ = tick.tick() => {
                    let state: String = sqlx::query_scalar("SELECT state FROM direct_downloads WHERE id=?")
                        .bind(id).fetch_one(self.db.pool()).await?;
                    if state != "running" { bail!("Cancelled"); }
                }
            }
        }
    }
}
