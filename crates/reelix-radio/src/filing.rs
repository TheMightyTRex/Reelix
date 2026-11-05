//! Filing a finished download into the podcast library folder.
//!
//! 1. Pick the episode's audio file in the download (one file; several only when exactly one
//!    names the episode or one holds at least 80 % of the audio).
//! 2. Work out the destination from the radio naming settings (`radio.naming`) under the
//!    show's radio root folder, else the radio settings' `library_folder`:
//!    `{Show}/{Show} - S{series:00}E{episode:00} - {Title}.{ext}` by default, specials as
//!    series 0. An existing file is never overwritten: the import fails and says so.
//! 3. Bring the file in next to the destination (`.{name}.reelix-partial`): copied when tags
//!    are written (the copy is Reelix's own, so a seeding source is never changed), else hard
//!    linked or copied as the pipeline's transfer mode says. Audio is never re-encoded; a
//!    container players cannot read (MPEG-TS captures) is remuxed with `ffmpeg -c copy` into
//!    Matroska audio, nothing else is touched.
//! 4. Write the tags into the staged copy (lofty), then link it into place without replacing
//!    anything and remove the staged name. `Move` removes the source afterwards.
//! 5. Remember the filed path on the episode (it is not searched again while the file exists),
//!    and ask the library sync to scan ([`crate::RadioLibrarySync::request_scan`]); the next
//!    sync matches the file and the episode becomes In library.

use std::path::{Path, PathBuf};

use reelix_api_types::radio::{RadioEpisodeDto, RadioShowDto};
use reelix_import::content::ContentDownload;
use reelix_import::files::TransferMode;
use reelix_parser::{QualityModel, parse_radio, radio_codec, radio_quality};

use crate::error::{RadioError, Result};
use crate::naming::EpisodeNaming;
use crate::search::episode_matches;
use crate::status::filing_series_number;
use crate::tags::{self, EpisodeTags};
use crate::wanted::WantedEpisode;
use crate::{RadioService, entities, repo};

/// Audio files a radio download may hold.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "m4b", "aac", "opus", "ogg", "oga", "flac", "wav", "wma", "mka", "mp2", "ts",
];

/// Containers that are remuxed (audio copied as is) into Matroska audio (`.mka`).
const REMUX: &[&str] = &["ts", "mts", "m2ts"];

/// One filed episode.
#[derive(Debug, Clone, PartialEq)]
pub struct FiledEpisode {
    pub episode_id: i64,
    pub show_id: i64,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub size: u64,
    pub quality: QualityModel,
    pub release_group: Option<String>,
    /// Hard linked rather than copied (only when tags are not written).
    pub hardlinked: bool,
    pub tags_written: bool,
    /// The container was remuxed with `ffmpeg -c copy`.
    pub remuxed: bool,
}

/// What filing a download did.
#[derive(Debug, Clone, PartialEq)]
pub enum FilingOutcome {
    Filed(FiledEpisode),
    /// Nothing was filed; the reasons are for the queue / history.
    Rejected(Vec<String>),
}

fn reject(msg: impl Into<String>) -> Result<FilingOutcome> {
    Ok(FilingOutcome::Rejected(vec![msg.into()]))
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn io(path: &Path, e: std::io::Error) -> RadioError {
    RadioError::Internal(format!("{}: {e}", path.display()))
}

/// Audio files under `source` (a file or a folder, four levels deep), skipping hidden files
/// and samples, with their sizes.
fn audio_files(source: &Path) -> std::io::Result<Vec<(PathBuf, u64)>> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, u64)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_dir() && depth < 4 {
                walk(&path, depth + 1, out)?;
            } else if ft.is_file()
                && AUDIO_EXTENSIONS.contains(&ext_of(&path).as_str())
                && !name.contains("sample")
            {
                out.push((path, entry.metadata()?.len()));
            }
        }
        Ok(())
    }
    let meta = std::fs::metadata(source)?;
    let mut out = Vec::new();
    if meta.is_file() {
        if AUDIO_EXTENSIONS.contains(&ext_of(source).as_str()) {
            out.push((source.to_path_buf(), meta.len()));
        }
    } else {
        walk(source, 0, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// The episode's file among several: the one whose name says it is the episode, else one with
/// at least 80 % of the bytes.
fn pick(files: &[(PathBuf, u64)], e: &WantedEpisode) -> std::result::Result<(PathBuf, u64), String> {
    match files {
        [] => return Err("no audio files in the download".into()),
        [one] => return Ok(one.clone()),
        _ => {}
    }
    let named: Vec<&(PathBuf, u64)> = files
        .iter()
        .filter(|(p, _)| {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            episode_matches(e, &parse_radio(&stem)).is_ok()
        })
        .collect();
    if let [one] = named.as_slice() {
        return Ok((*one).clone());
    }
    let total: u64 = files.iter().map(|(_, s)| *s).sum();
    if let Some(big) = files.iter().max_by_key(|(_, s)| *s)
        && total > 0
        && big.1.saturating_mul(10) >= total.saturating_mul(8)
    {
        return Ok(big.clone());
    }
    Err(format!(
        "the download has {} audio files and none of them clearly is the episode",
        files.len()
    ))
}

/// Links `tmp` to `dst` without replacing anything, then drops the `tmp` name. Falls back to
/// a rename on file systems without hard links (after checking `dst` is free).
fn commit(tmp: &Path, dst: &Path) -> std::result::Result<(), String> {
    match std::fs::hard_link(tmp, dst) {
        Ok(()) => {
            let _ = std::fs::remove_file(tmp);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists_msg(dst)),
        Err(_) if dst.exists() => Err(exists_msg(dst)),
        Err(_) => std::fs::rename(tmp, dst).map_err(|e| format!("{}: {e}", dst.display())),
    }
}

fn exists_msg(dst: &Path) -> String {
    format!(
        "{} already exists; Reelix never overwrites files in the podcast library (remove or rename it, then retry the import)",
        dst.display()
    )
}

/// Brings `src` to `tmp`: a copy when it will be changed (tags), else as `mode` says.
/// Returns whether it was hard linked.
fn stage(src: &Path, tmp: &Path, mode: TransferMode, will_modify: bool) -> std::io::Result<bool> {
    if will_modify {
        if mode == TransferMode::Move && std::fs::rename(src, tmp).is_ok() {
            return Ok(false);
        }
        std::fs::copy(src, tmp)?;
        return Ok(false);
    }
    match mode {
        TransferMode::Hardlink => std::fs::hard_link(src, tmp).map(|()| true),
        TransferMode::HardlinkOrCopy => match std::fs::hard_link(src, tmp) {
            Ok(()) => Ok(true),
            Err(_) => std::fs::copy(src, tmp).map(|_| false),
        },
        TransferMode::Copy => std::fs::copy(src, tmp).map(|_| false),
        TransferMode::Move => match std::fs::rename(src, tmp) {
            Ok(()) => Ok(false),
            Err(_) => std::fs::copy(src, tmp).map(|_| false),
        },
    }
}

/// `ffmpeg -c copy` of the first audio stream into Matroska (`REELIX_FFMPEG` or `ffmpeg`).
async fn remux(src: &Path, tmp: &Path) -> std::result::Result<(), String> {
    let exe = std::env::var("REELIX_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
        .arg(src)
        .args(["-vn", "-map", "0:a:0", "-c", "copy", "-f", "matroska"])
        .arg(tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("cannot start FFmpeg ({exe}): {e}; install it or set REELIX_FFMPEG"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(tmp);
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "FFmpeg could not remux {}: {}",
            src.display(),
            msg.trim().lines().last().unwrap_or("unknown error")
        ));
    }
    Ok(())
}

/// Tags of a filed episode.
fn episode_tags(show: &RadioShowDto, e: &RadioEpisodeDto, series: u32) -> EpisodeTags {
    EpisodeTags {
        album: show.title.clone(),
        title: e.title.clone(),
        disc: series,
        track: e.number,
        date: e.first_broadcast.map(|d| d.date_naive().to_string()),
        genre: "Radio".into(),
        artist: show.station_name.clone().or_else(|| show.author.clone()),
        comment: Some(format!(
            "{}:{}",
            show.provider,
            e.guid.as_deref().unwrap_or(&e.external_id)
        )),
    }
}

impl RadioService {
    /// The podcast library folder (as Reelix sees it) for a show: its radio root folder, else
    /// the radio settings' `library_folder`.
    pub async fn library_folder_for(&self, show: &RadioShowDto) -> Result<Option<PathBuf>> {
        if let Some(id) = show.root_folder_id {
            return Ok(Some(PathBuf::from(self.radio_root(id).await?.path)));
        }
        Ok(self.settings().await?.library_folder.map(PathBuf::from))
    }

    /// Files one finished download as `episode_id` (see the module docs). Never overwrites and
    /// never changes the downloaded file unless the transfer mode is `Move`.
    pub async fn file_download(&self, d: &ContentDownload, episode_id: i64) -> Result<FilingOutcome> {
        let ep = repo::episodes::require(&self.db, episode_id).await?;
        let show = repo::shows::require(&self.db, ep.show_id).await?;
        let Some(root) = self.library_folder_for(&show).await? else {
            return reject(
                "no podcast library folder: set the library folder in the radio settings or give the show a radio root folder",
            );
        };
        if let Err(msg) = crate::settings::check_folder(&root) {
            return reject(msg);
        }
        let naming = self.naming().await?;
        let series = filing_series_number(ep.series_kind, ep.series_number);
        let wanted = WantedEpisode::of(&show, ep.clone());

        let source = d.source.clone();
        let files = tokio::task::spawn_blocking(move || audio_files(&source))
            .await
            .map_err(|e| RadioError::Internal(e.to_string()))?
            .map_err(|e| io(&d.source, e))?;
        let (src, size) = match pick(&files, &wanted) {
            Ok(f) => f,
            Err(msg) => return reject(format!("{msg} ({})", d.source.display())),
        };
        let src_ext = ext_of(&src);
        let remuxed = REMUX.contains(&src_ext.as_str());
        let ext = if remuxed {
            "mka".to_owned()
        } else {
            src_ext.clone()
        };
        let quality = QualityModel::new(radio_quality(radio_codec(&src_ext)));
        let facts = EpisodeNaming {
            show_title: show.title.clone(),
            station: show.station_name.clone(),
            series,
            episode: ep.number,
            title: ep.title.clone(),
            air_date: ep.first_broadcast.map(|t| t.date_naive()),
            quality: Some(quality.quality),
        };
        let dest = root.join(facts.path(&naming, &ext));
        if dest.exists() {
            return reject(exists_msg(&dest));
        }
        let Some(dir) = dest.parent().map(Path::to_path_buf) else {
            return reject(format!("{} has no folder", dest.display()));
        };
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let file_name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = dir.join(format!(".{file_name}.reelix-partial"));
        let _ = std::fs::remove_file(&tmp);

        let write_tags = naming.write_tags;
        let mut hardlinked = false;
        if remuxed {
            if let Err(msg) = remux(&src, &tmp).await {
                return reject(msg);
            }
        } else {
            let (s, t, mode) = (src.clone(), tmp.clone(), d.mode);
            hardlinked = tokio::task::spawn_blocking(move || stage(&s, &t, mode, write_tags))
                .await
                .map_err(|e| RadioError::Internal(e.to_string()))?
                .map_err(|e| io(&tmp, e))?;
        }
        let mut tags_written = false;
        if write_tags && !hardlinked {
            let t = episode_tags(&show, &ep, series);
            let path = tmp.clone();
            match tokio::task::spawn_blocking(move || tags::write(&path, &t)).await {
                Ok(Ok(())) => tags_written = true,
                Ok(Err(e)) => tracing::warn!(error = %e, "radio filing: tags not written"),
                Err(e) => tracing::warn!(error = %e, "radio filing: tag writer failed"),
            }
        }
        let (t, dst) = (tmp.clone(), dest.clone());
        let committed = tokio::task::spawn_blocking(move || commit(&t, &dst))
            .await
            .map_err(|e| RadioError::Internal(e.to_string()))?;
        if let Err(msg) = committed {
            let _ = std::fs::remove_file(&tmp);
            return reject(msg);
        }
        if d.mode == TransferMode::Move
            && src.exists()
            && let Err(e) = std::fs::remove_file(&src)
        {
            tracing::warn!(path = %src.display(), error = %e, "radio filing: source not removed");
        }
        let dest_str = dest.to_string_lossy().into_owned();
        repo::episodes::set_filed(&self.db, episode_id, Some(&dest_str)).await?;
        self.emit(entities::EPISODE, "updated", episode_id);
        tracing::info!(episode_id, path = %dest.display(), "radio episode filed");
        if let Err(e) = self.request_library_scan().await {
            tracing::warn!(error = %e, "radio filing: library scan request failed");
        }
        let release_group = d
            .release_title
            .as_deref()
            .and_then(|t| parse_radio(t).release_group);
        Ok(FilingOutcome::Filed(FiledEpisode {
            episode_id,
            show_id: show.id,
            source: src,
            destination: dest,
            size,
            quality,
            release_group,
            hardlinked,
            tags_written,
            remuxed,
        }))
    }

    /// The file Reelix filed for an episode, if any.
    pub async fn filed_path(&self, episode_id: i64) -> Result<Option<PathBuf>> {
        Ok(repo::episodes::filed(&self.db, Some(&[episode_id]))
            .await?
            .pop()
            .map(|(_, p)| PathBuf::from(p)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted() -> WantedEpisode {
        WantedEpisode {
            episode_id: 1,
            show_id: 1,
            show_title: "The News Quiz".into(),
            provider: "rss".into(),
            station_name: None,
            series_number: 20,
            number: Some(3),
            title: "Episode 3".into(),
            first_broadcast: None,
            external_id: "x".into(),
            quality_profile_id: None,
            root_folder_id: None,
        }
    }

    #[test]
    fn picks_the_episode() {
        let f = |n: &str, s: u64| (PathBuf::from(n), s);
        assert_eq!(
            pick(&[f("a.mp3", 1)], &wanted()).unwrap().0,
            PathBuf::from("a.mp3")
        );
        let files = [
            f("The News Quiz S20E02.mp3", 10),
            f("The News Quiz S20E03.mp3", 10),
        ];
        assert_eq!(
            pick(&files, &wanted()).unwrap().0,
            PathBuf::from("The News Quiz S20E03.mp3")
        );
        let files = [f("intro.mp3", 1), f("main.mp3", 90)];
        assert_eq!(pick(&files, &wanted()).unwrap().0, PathBuf::from("main.mp3"));
        let files = [f("a.mp3", 50), f("b.mp3", 50)];
        assert!(pick(&files, &wanted()).unwrap_err().contains("2 audio files"));
        assert!(pick(&[], &wanted()).is_err());
    }

    #[test]
    fn commit_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let (tmp, dst) = (dir.path().join(".x.partial"), dir.path().join("x.mp3"));
        std::fs::write(&dst, b"old").unwrap();
        std::fs::write(&tmp, b"new").unwrap();
        assert!(commit(&tmp, &dst).unwrap_err().contains("never overwrites"));
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");
        std::fs::remove_file(&dst).unwrap();
        commit(&tmp, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        assert!(!tmp.exists());
    }
}
