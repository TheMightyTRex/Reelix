//! Native YouTube extraction and format selection. The two-stream pipeline follows
//! youtube-dl/yt-dlp's best-video + best-audio approach; extraction uses RustyPipe.
use crate::{
    DirectService,
    process::{convert, safe_name},
};
use anyhow::{Context, Result, bail};
use reelix_api_types::direct::*;
use rustypipe::{
    client::{ClientType, RustyPipe},
    model::{AudioFormat, VideoPlayer},
    param::StreamFilter,
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

impl DirectService {
    fn youtube_client(&self) -> Result<RustyPipe> {
        Ok(RustyPipe::builder()
            .storage_dir(self.data_dir.join("youtube"))
            .no_botguard()
            .no_reporter()
            .unauthenticated()
            .build()?)
    }

    async fn youtube_player(&self, client: &RustyPipe, id: &str) -> Result<VideoPlayer> {
        rustypipe::validate::video_id(id)?;
        let player = client.query().player_from_clients(id, &[ClientType::Ios, ClientType::Android]).await
            .map_err(|_| anyhow::anyhow!("Native YouTube extraction failed. The video may be unavailable, require login, or require a client update."))?;
        if player.drm.is_some() {
            bail!("This YouTube video requires DRM and is not supported");
        }
        if player.details.is_live {
            bail!("Live YouTube streams are not supported by direct downloads");
        }
        Ok(player)
    }

    async fn youtube_ids(
        &self,
        client: &RustyPipe,
        r: &DirectRequest,
        preview: bool,
    ) -> Result<(String, Vec<(String, String)>, bool)> {
        let url = url::Url::parse(&r.url)?;
        if r.playlist {
            let id = url
                .query_pairs()
                .find(|(k, _)| k == "list")
                .context("Missing playlist ID")?
                .1
                .into_owned();
            let query = client.query();
            let mut playlist = query
                .playlist(&id)
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read YouTube playlist"))?;
            if preview {
                playlist
                    .videos
                    .extend_limit(&query, 51)
                    .await
                    .map_err(|_| anyhow::anyhow!("Cannot read YouTube playlist page"))?;
            } else {
                while !playlist.videos.is_exhausted() {
                    if playlist.videos.items.len() >= 10_000 {
                        bail!("YouTube playlist exceeds the 10,000-item limit");
                    }
                    playlist
                        .videos
                        .extend(&query)
                        .await
                        .map_err(|_| anyhow::anyhow!("Cannot read YouTube playlist page; retry the job"))?;
                }
            }
            let truncated = !playlist.videos.is_exhausted() || playlist.videos.items.len() > 50;
            let limit = if preview { 50 } else { 10_000 };
            let mut seen = HashSet::new();
            let items = playlist
                .videos
                .items
                .into_iter()
                .filter(|v| seen.insert(v.id.clone()))
                .take(limit)
                .map(|v| (v.id, v.name))
                .collect();
            Ok((playlist.name, items, truncated))
        } else {
            let id = url
                .query_pairs()
                .find(|(k, _)| k == "v")
                .context("Missing video ID")?
                .1
                .into_owned();
            Ok((String::new(), vec![(id, String::new())], false))
        }
    }

    pub(crate) async fn youtube_preview(&self, r: &DirectRequest) -> Result<DirectPreview> {
        let client = self.youtube_client()?;
        let (mut title, mut items, truncated) = self.youtube_ids(&client, r, true).await?;
        if !r.playlist {
            let player = self.youtube_player(&client, &items[0].0).await?;
            title = player.details.name.unwrap_or_else(|| items[0].0.clone());
            items[0].1 = title.clone();
        }
        Ok(DirectPreview {
            title,
            items: items.into_iter().map(|(_, title)| title).collect(),
            truncated,
        })
    }

    pub(crate) async fn download_youtube(
        &self,
        job: &DirectDownload,
        settings: &DirectSettings,
    ) -> Result<()> {
        let client = self.youtube_client()?;
        let (_, items, _) = self.youtube_ids(&client, &job.request, false).await?;
        if items.is_empty() {
            bail!("YouTube playlist has no downloadable videos");
        }
        let total = items.len();
        for (index, (id, _)) in items.into_iter().enumerate() {
            self.progress(job.id, None, &format!("Resolving video {} of {total}", index + 1))
                .await?;
            let player = self.youtube_player(&client, &id).await?;
            let user_agent = client.query().user_agent(player.client_type).into_owned();
            let title = player.details.name.as_deref().unwrap_or(&id);
            let folder = Path::new(&job.output_dir);
            let name = format!("{} [{id}]", safe_name(title));
            let audio = player
                .select_audio_stream(&StreamFilter::new())
                .context("No unprotected audio stream available")?;
            let original = match audio.format {
                AudioFormat::M4a => "m4a",
                AudioFormat::Webm => "webm",
                _ => bail!("Unsupported YouTube audio container"),
            };
            let extension = if job.request.mode == DownloadMode::Audio && job.request.format == "original" {
                original
            } else {
                &job.request.format
            };
            let output = folder.join(format!("{name}.{extension}"));
            if tokio::fs::try_exists(&output).await? {
                continue;
            }
            let audio_part = folder.join(format!(".{id}-{}.audio.part", audio.itag));
            let mut inputs = Vec::<PathBuf>::new();
            if job.request.mode == DownloadMode::Video {
                let stream = player
                    .video_only_streams
                    .iter()
                    .chain(player.video_streams.iter())
                    .filter(|v| v.drm_track_type.is_none() && v.drm_systems.is_empty())
                    .filter(|v| job.request.max_height.is_none_or(|h| v.height <= h))
                    .max_by_key(|v| (v.height, v.fps, v.bitrate))
                    .context("No video stream fits the selected resolution")?;
                let video_part = folder.join(format!(".{id}-{}.video.part", stream.itag));
                self.transfer_youtube(&stream.url, &video_part, job.id, &user_agent)
                    .await?;
                inputs.push(video_part);
            }
            self.transfer_youtube(&audio.url, &audio_part, job.id, &user_agent)
                .await?;
            inputs.push(audio_part.clone());
            self.progress(job.id, Some(100.0), "Processing downloaded streams")
                .await?;
            let staging = folder.join(format!(".{id}.{}.staging.{extension}", reelix_db::now_ms()));
            if job.request.mode == DownloadMode::Audio && job.request.format == "original" {
                tokio::fs::copy(&audio_part, &staging).await?;
            } else {
                convert(
                    &settings.ffmpeg,
                    &inputs,
                    &staging,
                    job.request.mode == DownloadMode::Video,
                    &job.request.format,
                )
                .await?;
            }
            if job.request.mode == DownloadMode::Audio && extension != "webm" {
                crate::spotify::write_tags(
                    &staging,
                    title,
                    player.details.channel_name.as_deref().unwrap_or("YouTube"),
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
            }
            tokio::fs::rename(&staging, &output).await?;
            for input in inputs {
                tokio::fs::remove_file(input).await?;
            }
        }
        Ok(())
    }
}
