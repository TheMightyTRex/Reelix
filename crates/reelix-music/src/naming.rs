//! Music naming tokens rendered with [`reelix_import::naming::render_custom`].
//!
//! Tokens: `{Artist Name}`, `{Artist SortName}`, `{Artist CleanName}`, `{Album Title}`,
//! `{Album CleanTitle}`, `{Album Type}`, `{Release Year}`, `{Album Disambiguation}`,
//! `{medium:00}`, `{Medium Name}`, `{track:00}`, `{Track Title}`, `{Track ArtistName}`,
//! `{Quality Title}`, `{MediaInfo AudioCodec}`, `{MediaInfo AudioBitRate}`.

use std::path::PathBuf;

use reelix_api_types::music::{AlbumType, MusicNamingDto};
use reelix_import::naming::{NamingConfig, clean_title, pad_number, render_custom, unknown_custom_tokens};
use reelix_parser::Quality;

use crate::convert::album_type_name;
use crate::error::{MusicError, Result};

const KNOWN: &[&str] = &[
    "artist name",
    "artist sortname",
    "artist cleanname",
    "album title",
    "album cleantitle",
    "album type",
    "release year",
    "album disambiguation",
    "medium",
    "medium name",
    "track",
    "track title",
    "track artistname",
    "quality title",
    "mediainfo audiocodec",
    "mediainfo audiobitrate",
];

/// Facts about one track (and its album/artist) for rendering.
#[derive(Debug, Clone, Default)]
pub struct TrackFacts {
    pub artist_name: String,
    pub artist_sort_name: String,
    pub album_title: String,
    pub album_type: AlbumType,
    pub album_disambiguation: Option<String>,
    pub release_year: Option<i32>,
    pub medium: u32,
    pub medium_name: Option<String>,
    pub track: u32,
    pub track_title: String,
    pub track_artist: Option<String>,
    pub quality: Option<Quality>,
    pub audio_codec: Option<String>,
    pub audio_bitrate_kbps: Option<u32>,
}

fn resolve(f: &TrackFacts, key: &str, spec: Option<&str>) -> Option<String> {
    Some(match key {
        "artist name" => f.artist_name.clone(),
        "artist sortname" => f.artist_sort_name.clone(),
        "artist cleanname" => clean_title(&f.artist_name),
        "album title" => f.album_title.clone(),
        "album cleantitle" => clean_title(&f.album_title),
        "album type" => album_type_name(f.album_type).to_owned(),
        "release year" => f.release_year.map(|y| y.to_string()).unwrap_or_default(),
        "album disambiguation" => f.album_disambiguation.clone().unwrap_or_default(),
        "medium" => pad_number(f.medium.max(1), spec),
        "medium name" => f.medium_name.clone().unwrap_or_default(),
        "track" => pad_number(f.track, spec),
        "track title" => f.track_title.clone(),
        "track artistname" => f.track_artist.clone().unwrap_or_else(|| f.artist_name.clone()),
        "quality title" => f.quality.map(|q| q.name().to_owned()).unwrap_or_default(),
        "mediainfo audiocodec" => f.audio_codec.clone().unwrap_or_default(),
        "mediainfo audiobitrate" => f
            .audio_bitrate_kbps
            .map(|b| format!("{b} kbps"))
            .unwrap_or_default(),
        _ => return None,
    })
}

pub fn render(template: &str, f: &TrackFacts, cfg: &NamingConfig) -> String {
    render_custom(template, &|k, s| resolve(f, k, s), cfg)
}

/// Codec display name of an audio extension.
pub fn codec_name(ext: &str, quality: Quality) -> Option<String> {
    Some(
        match ext {
            "flac" => "FLAC",
            "mp3" => "MP3",
            "m4a" | "m4b" | "mp4" if matches!(quality, Quality::Alac | Quality::Alac24) => "ALAC",
            "m4a" | "m4b" | "mp4" | "aac" => "AAC",
            "alac" => "ALAC",
            "ogg" | "oga" => "Vorbis",
            "opus" => "Opus",
            "wma" => "WMA",
            "wav" => "PCM",
            "aiff" | "aif" => "AIFF",
            "ape" => "APE",
            "wv" => "WavPack",
            _ => return None,
        }
        .to_owned(),
    )
}

/// Artist folder name (`{Artist Name}`).
pub fn artist_folder(n: &MusicNamingDto, artist_name: &str, sort_name: &str, cfg: &NamingConfig) -> String {
    let f = TrackFacts {
        artist_name: artist_name.into(),
        artist_sort_name: sort_name.into(),
        ..Default::default()
    };
    let s = render(&n.artist_folder, &f, cfg);
    if s.trim().is_empty() {
        reelix_import::naming::sanitize(artist_name, cfg)
    } else {
        s
    }
}

/// Path of a track file relative to the artist folder: `album/[disc/]file.ext`.
/// `original_stem` is used when renaming is off.
pub fn track_path(
    n: &MusicNamingDto,
    f: &TrackFacts,
    disc_count: u32,
    ext: &str,
    original_stem: Option<&str>,
    cfg: &NamingConfig,
) -> PathBuf {
    let album = render(&n.album_folder, f, cfg);
    let album = if album.trim().is_empty() {
        reelix_import::naming::sanitize(&f.album_title, cfg)
    } else {
        album
    };
    let mut p = PathBuf::from(album);
    let multi = disc_count > 1;
    let disc_folder = n.multi_disc_folder.as_deref().filter(|s| !s.trim().is_empty());
    let file_template = match (multi, disc_folder) {
        (true, Some(folder)) => {
            p.push(render(folder, f, cfg));
            &n.track_file
        }
        (true, None) => &n.multi_disc_track_file,
        (false, _) => &n.track_file,
    };
    let stem = match original_stem.filter(|_| !n.rename) {
        Some(s) => reelix_import::naming::sanitize(s, cfg),
        None => render(file_template, f, cfg),
    };
    let stem = if stem.trim().is_empty() {
        format!("{:02}", f.track)
    } else {
        stem
    };
    p.push(if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    });
    p
}

/// Rejects empty required templates and unknown tokens.
pub fn validate(n: &MusicNamingDto) -> Result<()> {
    let known = |k: &str| KNOWN.contains(&k);
    let fields = [
        ("artist_folder", Some(&n.artist_folder), true),
        ("album_folder", Some(&n.album_folder), true),
        ("track_file", Some(&n.track_file), true),
        ("multi_disc_track_file", Some(&n.multi_disc_track_file), true),
        ("multi_disc_folder", n.multi_disc_folder.as_ref(), false),
    ];
    for (name, tpl, required) in fields {
        let Some(tpl) = tpl else { continue };
        if tpl.trim().is_empty() {
            if required {
                return Err(MusicError::Validation(format!("{name}: must not be empty")));
            }
            continue;
        }
        let unknown = unknown_custom_tokens(tpl, &known);
        if !unknown.is_empty() {
            return Err(MusicError::Validation(format!(
                "{name}: unknown token(s) {}",
                unknown.join(", ")
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> TrackFacts {
        TrackFacts {
            artist_name: "Radiohead".into(),
            artist_sort_name: "Radiohead".into(),
            album_title: "OK Computer".into(),
            release_year: Some(1997),
            medium: 2,
            track: 3,
            track_title: "Subterranean Homesick Alien".into(),
            quality: Some(Quality::Flac),
            ..Default::default()
        }
    }

    #[test]
    fn renders_paths() {
        let cfg = NamingConfig::default();
        let mut n = MusicNamingDto::default();
        assert_eq!(
            track_path(&n, &facts(), 1, "flac", None, &cfg),
            PathBuf::from("OK Computer (1997)/03 - Subterranean Homesick Alien.flac")
        );
        assert_eq!(
            track_path(&n, &facts(), 2, "flac", None, &cfg),
            PathBuf::from("OK Computer (1997)/02-03 - Subterranean Homesick Alien.flac")
        );
        n.multi_disc_folder = Some("CD {medium}".into());
        assert_eq!(
            track_path(&n, &facts(), 2, "flac", None, &cfg),
            PathBuf::from("OK Computer (1997)/CD 2/03 - Subterranean Homesick Alien.flac")
        );
        n.rename = false;
        assert_eq!(
            track_path(&n, &facts(), 1, "mp3", Some("orig name"), &cfg),
            PathBuf::from("OK Computer (1997)/orig name.mp3")
        );
        assert!(validate(&MusicNamingDto::default()).is_ok());
        let bad = MusicNamingDto {
            track_file: "{Movie Title}".into(),
            ..Default::default()
        };
        assert!(validate(&bad).is_err());
    }
}
