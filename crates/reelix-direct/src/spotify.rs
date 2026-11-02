//! Native Spotify session/metadata/audio pipeline. Adapted from Zotify's
//! resolve-track -> select-quality -> decrypt -> tag -> publish workflow.
use crate::{
    DirectService,
    process::{convert, safe_name},
};
use anyhow::{Context, Result, bail};
use librespot_core::{
    FileId, Session, SessionConfig, SpotifyId, SpotifyUri, audio_key::AudioKey, authentication::Credentials,
    cdn_url::CdnUrl, error::ErrorKind,
};
use librespot_metadata::{Album, Metadata, Track, audio::file::AudioFileFormat, playlist::Playlist};
use librespot_oauth::OAuthClientBuilder;
use reelix_api_types::direct::*;
use std::{path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct SpotifySession(Session);
impl Drop for SpotifySession {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

pub(crate) async fn read_credentials(path: &Path) -> Result<Credentials> {
    let size = tokio::fs::metadata(path).await?.len();
    if size > 64 * 1024 {
        bail!("Spotify credentials file is too large");
    }
    let mut value: serde_json::Value = serde_json::from_slice(&tokio::fs::read(path).await?)
        .map_err(|_| anyhow::anyhow!("Invalid Spotify credentials JSON"))?;
    // Zotify/librespot-python use named `type` and base64 `credentials` fields.
    if value.get("credentials").is_some() && value.get("auth_type").is_none() {
        if value["type"] != "AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS" {
            bail!("Only reusable Spotify session credentials are supported");
        }
        value["auth_type"] = serde_json::json!(1);
        value["auth_data"] = value["credentials"].clone();
    }
    let credentials: Credentials = serde_json::from_value(value)
        .map_err(|_| anyhow::anyhow!("Invalid librespot/Spotify credentials file"))?;
    if credentials.auth_data.is_empty() {
        bail!("Spotify credentials file contains no authentication data");
    }
    if credentials.auth_type
        != librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS
    {
        bail!("Use reusable session credentials created by reelix spotify-login");
    }
    Ok(credentials)
}

async fn connect(path: &Path) -> Result<SpotifySession> {
    let credentials = read_credentials(path).await?;
    let session = SpotifySession(Session::new(SessionConfig::default(), None));
    tokio::time::timeout(Duration::from_secs(45), session.0.connect(credentials, false))
        .await
        .context("Spotify login timed out")?
        .map_err(|_| {
            anyhow::anyhow!("Spotify authentication failed; run reelix spotify-login to renew the session")
        })?;
    Ok(session)
}

/// Native PKCE login for the CLI. Only the reusable session is saved, never a password.
/// Run on a machine where the browser can reach the configured loopback callback.
pub async fn login(path: &Path, client_id: Option<&str>, redirect_uri: &str) -> Result<()> {
    let mut config = SessionConfig::default();
    if let Some(client_id) = client_id {
        config.client_id = client_id.to_owned();
    }
    let oauth = OAuthClientBuilder::new(
        client_id.unwrap_or(&config.client_id),
        redirect_uri,
        vec!["streaming", "user-read-private"],
    )
    .build()?;
    let token = oauth
        .get_access_token_async()
        .await
        .context("Spotify OAuth login failed")?;
    save_session(path, config, &token.access_token).await?;
    Ok(())
}

/// Exchange an OAuth access token for reusable session credentials, written atomically
/// to `path`. Returns the account's username.
pub(crate) async fn save_session(path: &Path, config: SessionConfig, access_token: &str) -> Result<String> {
    let session = SpotifySession(Session::new(config, None));
    tokio::time::timeout(
        Duration::from_secs(45),
        session
            .0
            .connect(Credentials::with_access_token(access_token), false),
    )
    .await
    .context("Spotify login timed out")?
    .map_err(|_| anyhow::anyhow!("Spotify rejected the authenticated session"))?;
    let credentials = Credentials {
        username: Some(session.0.username()),
        auth_type:
            librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
        auth_data: session.0.auth_data(),
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let temporary = parent.join(format!(
        ".spotify-{}-{}.partial",
        std::process::id(),
        reelix_db::now_ms()
    ));
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await?;
    file.write_all(&serde_json::to_vec(&credentials)?).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&temporary, path).await?;
    Ok(session.0.username())
}

fn uri(r: &DirectRequest) -> Result<SpotifyUri> {
    let url = url::Url::parse(&r.url)?;
    let parts: Vec<_> = url.path_segments().context("Invalid Spotify URL")?.collect();
    SpotifyUri::from_uri(&format!("spotify:{}:{}", parts[0], parts[1])).map_err(Into::into)
}

async fn tracks(session: &Session, r: &DirectRequest) -> Result<(String, Vec<SpotifyUri>)> {
    let uri = uri(r)?;
    match uri {
        SpotifyUri::Track { .. } => Ok((String::new(), vec![uri])),
        SpotifyUri::Album { .. } => {
            let album = Album::get(session, &uri)
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read Spotify album"))?;
            Ok((album.name.clone(), album.tracks().cloned().collect()))
        }
        SpotifyUri::Playlist { .. } => {
            let playlist = Playlist::get(session, &uri)
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read Spotify playlist"))?;
            let items: Vec<_> = playlist.tracks().cloned().collect();
            if items.len() != playlist.length.max(0) as usize {
                bail!("Spotify returned an incomplete playlist; refusing a partial download");
            }
            Ok((playlist.name().to_owned(), items))
        }
        _ => bail!("Only Spotify tracks, albums and playlists are supported"),
    }
}

/// Asks for a file's decryption key, retrying when the reply is late (librespot gives up
/// after 1.5 s) or the channel dropped. A refusal (`Unavailable`) returns at once.
async fn request_key(
    session: &Session,
    id: SpotifyId,
    file: FileId,
) -> Result<AudioKey, librespot_core::Error> {
    let mut attempt = 0;
    loop {
        match session.audio_key().request(id, file).await {
            Err(e) if e.kind == ErrorKind::Aborted && attempt < 2 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            other => return other,
        }
    }
}

/// The audio file and key for `track`, falling back to Spotify's alternative (relinked)
/// tracks when the track has no file at the selected quality or its key is refused,
/// as Spotify's own clients do for tracks not licensed in the account's country.
async fn audio_source(
    session: &Session,
    track: &Track,
    qualities: &[AudioFileFormat],
    product: Option<&str>,
) -> Result<(FileId, AudioKey)> {
    let mut last_error = None;
    let mut has_file = false;
    for candidate in std::iter::once(None).chain(track.alternatives.iter().take(5).map(Some)) {
        let fetched;
        let t = match candidate {
            None => track,
            Some(uri) => match Track::get(session, uri).await {
                Ok(t) => {
                    fetched = t;
                    &fetched
                }
                Err(_) => continue,
            },
        };
        let Some(file) = qualities.iter().find_map(|f| t.files.get(f)).copied() else {
            continue;
        };
        has_file = true;
        let SpotifyUri::Track { id } = &t.id else {
            continue;
        };
        match request_key(session, *id, file).await {
            Ok(key) => return Ok((file, key)),
            Err(e) => last_error = Some(e),
        }
    }
    if !has_file {
        bail!("No audio file at the selected quality is available for this track/account");
    }
    match (last_error.map(|e| e.kind), product) {
        (Some(ErrorKind::Unavailable), Some(p)) if p != "premium" => bail!(
            "Spotify refused this track's audio key. Spotify downloads need a Premium account, and this account is {p}"
        ),
        (Some(ErrorKind::Unavailable), _) => bail!(
            "Spotify refused this track's audio key; it may not be available in your account's country ({})",
            session.country()
        ),
        _ => bail!("Spotify did not answer the audio key request in time; retry the job"),
    }
}

pub(crate) fn quality_order(quality: &str, premium: bool) -> Result<Vec<AudioFileFormat>> {
    use AudioFileFormat::*;
    match quality {
        "normal" => Ok(vec![OGG_VORBIS_96]),
        "high" => Ok(vec![OGG_VORBIS_160, OGG_VORBIS_96]),
        "very_high" if !premium => bail!("Very high Spotify quality requires an eligible Premium account"),
        "very_high" => Ok(vec![OGG_VORBIS_320]),
        "auto" if premium => Ok(vec![OGG_VORBIS_320, OGG_VORBIS_160, OGG_VORBIS_96]),
        "auto" => Ok(vec![OGG_VORBIS_160, OGG_VORBIS_96]),
        _ => bail!("Unsupported Spotify quality"),
    }
}

impl DirectService {
    pub(crate) async fn spotify_preview(
        &self,
        settings: &DirectSettings,
        r: &DirectRequest,
    ) -> Result<DirectPreview> {
        let session = connect(Path::new(&settings.spotify_credentials)).await?;
        let (mut title, items) = tracks(&session.0, r).await?;
        let truncated = items.len() > 50;
        let mut names = Vec::new();
        for uri in items.into_iter().take(50) {
            let track = Track::get(&session.0, &uri)
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read Spotify track metadata"))?;
            names.push(track.name);
        }
        if title.is_empty() {
            title = names.first().cloned().unwrap_or_default();
        }
        Ok(DirectPreview {
            title,
            items: names,
            truncated,
        })
    }

    pub(crate) async fn download_spotify(
        &self,
        job: &DirectDownload,
        settings: &DirectSettings,
    ) -> Result<()> {
        let session = connect(Path::new(&settings.spotify_credentials)).await?;
        let (_, tracks) = tracks(&session.0, &job.request).await?;
        if tracks.is_empty() {
            bail!("Spotify collection contains no tracks");
        }
        if tracks.len() > 10_000 {
            bail!("Spotify collection exceeds the 10,000-track limit");
        }
        // Product attributes arrive asynchronously after the login welcome packet.
        let product = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(product) = session.0.get_user_attribute("type") {
                    break product;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .ok();
        let premium = product.as_deref() == Some("premium");
        let qualities = quality_order(job.request.spotify_quality.as_deref().unwrap_or("auto"), premium)?;
        let total = tracks.len();
        for (index, uri) in tracks.into_iter().enumerate() {
            self.progress(job.id, None, &format!("Resolving track {} of {total}", index + 1))
                .await?;
            if !matches!(uri, SpotifyUri::Track { .. }) {
                bail!("Spotify playlist contains an unsupported local track or episode");
            }
            let track = Track::get(&session.0, &uri)
                .await
                .map_err(|_| anyhow::anyhow!("Spotify track metadata is unavailable"))?;
            let (file_id, key) = audio_source(&session.0, &track, &qualities, product.as_deref()).await?;
            let SpotifyUri::Track { id } = track.id else {
                bail!("Unexpected Spotify track ID");
            };
            let track_id = id.to_base62()?;
            let artist = track
                .artists
                .first()
                .map(|a| a.name.as_str())
                .unwrap_or("Unknown Artist");
            let folder = Path::new(&job.output_dir)
                .join(safe_name(artist))
                .join(safe_name(&track.album.name));
            tokio::fs::create_dir_all(&folder).await?;
            let extension = if ["original", "ogg"].contains(&job.request.format.as_str()) {
                "ogg"
            } else {
                &job.request.format
            };
            let name = format!(
                "{:02}-{:02} {} [{track_id}]",
                track.disc_number,
                track.number,
                safe_name(&track.name)
            );
            let output = folder.join(format!("{name}.{extension}"));
            if tokio::fs::try_exists(&output).await? {
                continue;
            }
            let cdn = CdnUrl::new(file_id)
                .resolve_audio(&session.0)
                .await
                .map_err(|_| anyhow::anyhow!("Cannot resolve Spotify audio storage"))?;
            let part = folder.join(format!(".{track_id}-{file_id}.part"));
            let mut success = false;
            for url in cdn.try_get_urls()? {
                if self.transfer(url, &part, job.id, Some(key.0)).await.is_ok() {
                    success = true;
                    break;
                }
            }
            if !success {
                bail!("Spotify audio transfer failed; retry to resume");
            }
            let mut header = [0u8; 4];
            tokio::fs::File::open(&part)
                .await?
                .read_exact(&mut header)
                .await?;
            if &header != b"OggS" {
                bail!("Spotify audio container validation failed");
            }
            self.progress(job.id, Some(100.0), "Processing audio and metadata")
                .await?;
            let staging = folder.join(format!(".{track_id}.{}.staging.{extension}", reelix_db::now_ms()));
            if extension == "ogg" {
                tokio::fs::copy(&part, &staging).await?;
            } else {
                convert(
                    &settings.ffmpeg,
                    std::slice::from_ref(&part),
                    &staging,
                    false,
                    extension,
                )
                .await?;
            }
            let cover = if let Some(image) = track.album.covers.iter().max_by_key(|i| i.width) {
                session
                    .0
                    .spclient()
                    .get_image(&image.id)
                    .await
                    .ok()
                    .filter(|b| b.len() <= 10_000_000)
                    .map(|b| b.to_vec())
            } else {
                None
            };
            write_tags(
                &staging,
                &track.name,
                artist,
                Some(&track.album.name),
                Some(track.number as u32),
                Some(track.disc_number as u32),
                cover,
            )
            .await?;
            tokio::fs::rename(&staging, &output).await?;
            tokio::fs::remove_file(&part).await?;
        }
        Ok(())
    }
}

pub(crate) async fn write_tags(
    path: &Path,
    title: &str,
    artist: &str,
    album: Option<&str>,
    track: Option<u32>,
    disc: Option<u32>,
    cover: Option<Vec<u8>>,
) -> Result<()> {
    use lofty::{
        config::WriteOptions,
        file::{AudioFile, TaggedFileExt},
        picture::{Picture, PictureType},
        tag::{Accessor, Tag},
    };
    let path = path.to_path_buf();
    let (title, artist, album) = (title.to_owned(), artist.to_owned(), album.map(str::to_owned));
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut file = lofty::probe::Probe::open(&path)?.read()?;
        let tag_type = file.primary_tag_type();
        if file.primary_tag().is_none() {
            file.insert_tag(Tag::new(tag_type));
        }
        let tag = file.primary_tag_mut().context("Cannot create audio metadata")?;
        tag.set_title(title);
        tag.set_artist(artist);
        if let Some(album) = album {
            tag.set_album(album);
        }
        if let Some(track) = track {
            tag.set_track(track);
        }
        if let Some(disc) = disc {
            tag.set_disk(disc);
        }
        if let Some(cover) = cover {
            let mut picture = Picture::from_reader(&mut std::io::Cursor::new(cover))?;
            picture.set_pic_type(PictureType::CoverFront);
            tag.push_picture(picture);
        }
        file.save_to_path(&path, WriteOptions::default())?;
        Ok(())
    })
    .await??;
    Ok(())
}
