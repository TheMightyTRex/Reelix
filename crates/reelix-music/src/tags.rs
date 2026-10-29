//! Audio file properties and tags (lofty): reading for matching/scans, writing on import.
//!
//! Tags are only ever written to files Reelix owns (copies in the import staging folder),
//! never to files that are still seeding (design D14).

use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};
use serde::{Deserialize, Serialize};

use crate::error::{MusicError, Result};

/// Audio file extensions the music importer and scanner handle.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "m4b", "aac", "alac", "ogg", "oga", "opus", "wma", "wav", "aiff", "aif", "ape",
    "wv", "mpc",
];

pub fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

pub fn is_audio(path: &Path) -> bool {
    extension(path).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// Tags read from (or written to) a file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub year: Option<u32>,
    pub date: Option<String>,
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
}

impl FileTags {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Stream properties and tags of one audio file.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInfo {
    pub path: PathBuf,
    pub ext: String,
    pub size: u64,
    pub duration_ms: Option<u64>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub tags: FileTags,
}

impl AudioInfo {
    /// File quality from extension and stream properties.
    pub fn quality(&self) -> reelix_parser::Quality {
        let ext = match self.ext.as_str() {
            // ALAC lives in .m4a; lossless streams report a high bitrate and a bit depth.
            "m4a" if self.bit_depth.is_some() && self.bitrate_kbps.is_some_and(|b| b > 500) => "alac",
            e => e,
        };
        reelix_parser::music_quality(ext, self.bitrate_kbps, self.bit_depth, false)
    }
}

fn lofty_err(path: &Path, e: impl std::fmt::Display) -> MusicError {
    MusicError::Internal(format!("{}: {e}", path.display()))
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Reads properties and tags. Files lofty can't parse still return their size and extension.
pub fn read(path: &Path) -> Result<AudioInfo> {
    let size = std::fs::metadata(path)
        .map_err(|e| reelix_import::Error::io(path, e))?
        .len();
    let ext = extension(path).unwrap_or_default();
    let mut info = AudioInfo {
        path: path.to_path_buf(),
        ext,
        size,
        duration_ms: None,
        bitrate_kbps: None,
        sample_rate_hz: None,
        bit_depth: None,
        channels: None,
        tags: FileTags::default(),
    };
    let tagged = match Probe::open(path)
        .map_err(|e| lofty_err(path, e))?
        .guess_file_type()
        .map_err(|e| lofty_err(path, e))?
        .read()
    {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "unreadable audio file");
            return Ok(info);
        }
    };
    let p = tagged.properties();
    let ms = p.duration().as_millis() as u64;
    info.duration_ms = (ms > 0).then_some(ms);
    info.bitrate_kbps = p.audio_bitrate().or(p.overall_bitrate()).filter(|b| *b > 0);
    info.sample_rate_hz = p.sample_rate();
    info.bit_depth = p.bit_depth();
    info.channels = p.channels();
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        info.tags = tags_of(tag);
    }
    Ok(info)
}

fn tags_of(tag: &Tag) -> FileTags {
    let s = |k: ItemKey| non_empty(tag.get_string(&k));
    let date = s(ItemKey::RecordingDate)
        .or_else(|| s(ItemKey::ReleaseDate))
        .or_else(|| s(ItemKey::OriginalReleaseDate));
    FileTags {
        title: non_empty(tag.title().as_deref()),
        artist: non_empty(tag.artist().as_deref()),
        album: non_empty(tag.album().as_deref()),
        album_artist: s(ItemKey::AlbumArtist),
        track: tag.track(),
        track_total: tag.track_total(),
        disc: tag.disk(),
        disc_total: tag.disk_total(),
        year: tag.year().or_else(|| {
            date.as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok())
        }),
        date,
        musicbrainz_track_id: s(ItemKey::MusicBrainzTrackId),
        musicbrainz_recording_id: s(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_id: s(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: s(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_artist_id: s(ItemKey::MusicBrainzArtistId),
    }
}

/// Cover image to embed.
#[derive(Debug, Clone, PartialEq)]
pub struct Cover {
    pub data: Vec<u8>,
    /// `image/jpeg` or `image/png`.
    pub mime: String,
}

impl Cover {
    /// Detects JPEG/PNG from the magic bytes; other data is rejected.
    pub fn from_bytes(data: Vec<u8>) -> Option<Self> {
        let mime = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else {
            return None;
        };
        Some(Self {
            data,
            mime: mime.into(),
        })
    }

    /// `cover.jpg` / `cover.png`.
    pub fn file_name(&self) -> &'static str {
        if self.mime == "image/png" {
            "cover.png"
        } else {
            "cover.jpg"
        }
    }
}

/// Writes `t` (only its `Some` fields) and the optional front cover into `path`.
pub fn write(path: &Path, t: &FileTags, cover: Option<&Cover>) -> Result<()> {
    let mut tagged = Probe::open(path)
        .map_err(|e| lofty_err(path, e))?
        .guess_file_type()
        .map_err(|e| lofty_err(path, e))?
        .read()
        .map_err(|e| lofty_err(path, e))?;
    if tagged.primary_tag().is_none() {
        let tt = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tt));
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return Err(lofty_err(path, "file type does not support tags"));
    };
    if let Some(v) = &t.title {
        tag.set_title(v.clone());
    }
    if let Some(v) = &t.artist {
        tag.set_artist(v.clone());
    }
    if let Some(v) = &t.album {
        tag.set_album(v.clone());
    }
    if let Some(v) = t.track {
        tag.set_track(v);
    }
    if let Some(v) = t.track_total {
        tag.set_track_total(v);
    }
    if let Some(v) = t.disc {
        tag.set_disk(v);
    }
    if let Some(v) = t.disc_total {
        tag.set_disk_total(v);
    }
    let text = [
        (ItemKey::AlbumArtist, &t.album_artist),
        (ItemKey::RecordingDate, &t.date),
        (ItemKey::MusicBrainzTrackId, &t.musicbrainz_track_id),
        (ItemKey::MusicBrainzRecordingId, &t.musicbrainz_recording_id),
        (ItemKey::MusicBrainzReleaseId, &t.musicbrainz_release_id),
        (
            ItemKey::MusicBrainzReleaseGroupId,
            &t.musicbrainz_release_group_id,
        ),
        (ItemKey::MusicBrainzArtistId, &t.musicbrainz_artist_id),
    ];
    for (key, value) in text {
        if let Some(v) = value {
            // Formats without the key (e.g. RIFF INFO) silently skip it.
            tag.insert_text(key, v.clone());
        }
    }
    if t.date.is_none()
        && let Some(y) = t.year
    {
        tag.set_year(y);
    }
    if let Some(c) = cover {
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::from_str(&c.mime)),
            None,
            c.data.clone(),
        ));
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| lofty_err(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal FLAC: STREAMINFO only (44.1 kHz, stereo, 16 bit, `secs` long) + filler bytes.
    pub(crate) fn tiny_flac(path: &Path, secs: u64) {
        let mut b = b"fLaC".to_vec();
        b.push(0x00); // STREAMINFO, not last
        b.extend_from_slice(&[0, 0, 34]);
        b.extend_from_slice(&4096u16.to_be_bytes());
        b.extend_from_slice(&4096u16.to_be_bytes());
        b.extend_from_slice(&[0; 6]);
        let v: u64 = (44_100u64 << 44) | (1u64 << 41) | (15u64 << 36) | (secs * 44_100);
        b.extend_from_slice(&v.to_be_bytes());
        b.extend_from_slice(&[0; 16]);
        b.extend_from_slice(&[0x81, 0, 0, 16]); // last block: PADDING
        b.extend_from_slice(&[0; 16]);
        b.extend_from_slice(&[0xAB; 256]);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.flac");
        tiny_flac(&p, 200);
        let i = read(&p).unwrap();
        assert_eq!(i.duration_ms, Some(200_000));
        assert_eq!(i.bit_depth, Some(16));
        assert_eq!(i.quality(), reelix_parser::Quality::Flac);
        let t = FileTags {
            title: Some("Airbag".into()),
            track: Some(1),
            disc: Some(1),
            musicbrainz_release_id: Some("rel".into()),
            album_artist: Some("Radiohead".into()),
            ..Default::default()
        };
        let cover = Cover::from_bytes(b"\x89PNG\r\n\x1a\nrest".to_vec()).unwrap();
        write(&p, &t, Some(&cover)).unwrap();
        let i = read(&p).unwrap();
        assert_eq!(i.tags.title.as_deref(), Some("Airbag"), "{:?}", i.tags);
        assert_eq!(i.tags.musicbrainz_release_id.as_deref(), Some("rel"));
        assert_eq!(i.tags.album_artist.as_deref(), Some("Radiohead"));
        assert_eq!(i.duration_ms, Some(200_000));
    }
}
