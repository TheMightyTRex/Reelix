//! Resumable bounded-memory HTTP transfers. No provider CLI or shell is invoked.
use crate::DirectService;
use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use reqwest::{
    StatusCode,
    header::{CONTENT_RANGE, RANGE},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

type AudioCipher = ctr::Ctr128BE<aes::Aes128>;
// Spotify's audio IV, as implemented in librespot/audio/src/decrypt.rs (MIT).
const AUDIO_IV: [u8; 16] = [
    0x72, 0xe0, 0x67, 0xfb, 0xdd, 0xcb, 0xcf, 0x77, 0xeb, 0xe8, 0xbc, 0x64, 0x3f, 0x63, 0x0d, 0x93,
];

impl DirectService {
    /// Resume `destination` using HTTP byte ranges. Spotify's 167-byte container prefix
    /// is omitted while seeking the cipher at the original encrypted byte position.
    pub(crate) async fn transfer(
        &self,
        url: &str,
        destination: &Path,
        id: i64,
        key: Option<[u8; 16]>,
    ) -> Result<()> {
        self.transfer_with_agent(url, destination, id, key, None).await
    }

    pub(crate) async fn transfer_youtube(
        &self,
        url: &str,
        destination: &Path,
        id: i64,
        user_agent: &str,
    ) -> Result<()> {
        self.transfer_with_agent(url, destination, id, None, Some(user_agent))
            .await
    }

    async fn transfer_with_agent(
        &self,
        url: &str,
        destination: &Path,
        id: i64,
        key: Option<[u8; 16]>,
        user_agent: Option<&str>,
    ) -> Result<()> {
        while !self.transfer_range(url, destination, id, key, user_agent).await? {}
        Ok(())
    }

    async fn transfer_range(
        &self,
        url: &str,
        destination: &Path,
        id: i64,
        key: Option<[u8; 16]>,
        user_agent: Option<&str>,
    ) -> Result<bool> {
        let existing = tokio::fs::metadata(destination)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let prefix = if key.is_some() { 167 } else { 0 };
        let offset = existing + prefix;
        let range = match user_agent {
            Some(_) => format!("bytes={offset}-{}", offset.saturating_add(8 * 1024 * 1024 - 1)),
            None => format!("bytes={offset}-"),
        };
        let mut request = self.http.get(url).header(RANGE, range);
        if let Some(user_agent) = user_agent {
            request = request.header(reqwest::header::USER_AGENT, user_agent);
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Media transfer connection failed"))?;
        if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            let size = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes */"))
                .and_then(|v| v.parse::<u64>().ok());
            if existing > 0 && size == Some(offset) {
                return Ok(true);
            }
            bail!("Server rejected the resume position");
        }
        if !response.status().is_success() {
            bail!("Media server returned HTTP {}", response.status().as_u16());
        }
        let partial = response.status() == StatusCode::PARTIAL_CONTENT;
        let range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok());
        let (start, total) = if partial {
            let (start, total) = parse_range(range.context("Partial response has no Content-Range")?)?;
            if start != offset {
                bail!("Media server returned the wrong byte range");
            }
            (offset, Some(total))
        } else {
            (0, response.content_length())
        };
        let append = partial && existing > 0;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(destination)
            .await?;
        let mut cipher = key.map(|key| AudioCipher::new(&key.into(), &AUDIO_IV.into()));
        if let Some(cipher) = &mut cipher {
            cipher.seek(start);
        }
        let mut position = start;
        let mut received = 0u64;
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let mut chunk = chunk
                .map_err(|_| anyhow::anyhow!("Media transfer interrupted; retry to resume"))?
                .to_vec();
            if let Some(cipher) = &mut cipher {
                cipher.apply_keystream(&mut chunk);
            }
            let skip = prefix.saturating_sub(position).min(chunk.len() as u64) as usize;
            file.write_all(&chunk[skip..]).await?;
            position += chunk.len() as u64;
            received += chunk.len() as u64;
            if last.elapsed() >= Duration::from_millis(500) {
                self.progress(
                    id,
                    total
                        .filter(|t| *t > 0)
                        .map(|t| (position as f64 / t as f64 * 100.0).min(100.0)),
                    "Downloading media",
                )
                .await?;
                last = Instant::now();
            }
        }
        if received == 0 || total.is_some_and(|t| position > t) {
            bail!("Media transfer was incomplete");
        }
        file.flush().await?;
        file.sync_all().await?;
        let complete = total.is_none_or(|t| position == t);
        if complete {
            self.progress(id, Some(100.0), "Media transfer complete").await?;
        }
        Ok(complete)
    }
}

pub(crate) fn parse_range(value: &str) -> Result<(u64, u64)> {
    let (range, total) = value
        .strip_prefix("bytes ")
        .and_then(|v| v.split_once('/'))
        .context("Invalid Content-Range")?;
    let (start, end) = range.split_once('-').context("Invalid Content-Range")?;
    let (start, end, total): (u64, u64, u64) = (start.parse()?, end.parse()?, total.parse()?);
    if end < start || end >= total {
        bail!("Invalid Content-Range bounds");
    }
    Ok((start, total))
}
