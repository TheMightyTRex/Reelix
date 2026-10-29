//! Audio tags (M4B/M4A/MP3/FLAC/OGG/Opus) via lofty.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag};

use super::{BookTags, image_mime};

/// Duration (seconds) and audio bitrate (kbps) of an audio file.
pub fn probe(path: &Path) -> Option<(u32, Option<u32>)> {
    let f = lofty::read_from_path(path).ok()?;
    let p = f.properties();
    Some((
        p.duration().as_secs() as u32,
        p.audio_bitrate().filter(|b| *b > 0),
    ))
}

/// Narrator (composer / `©wrt`) stored in an audio file, if any.
pub fn narrator(path: &Path) -> Option<String> {
    let f = lofty::read_from_path(path).ok()?;
    let tag = f.primary_tag().or_else(|| f.first_tag())?;
    tag.get_string(&ItemKey::Composer)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Writes title, author (artist + album artist), album (book title), narrator (composer,
/// `©wrt` in MP4), genre, year, part number and front cover into the primary tag.
pub fn write(path: &Path, t: &BookTags) -> Result<(), String> {
    let mut file = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    if file.primary_tag().is_none() {
        let tt = file.primary_tag_type();
        file.insert_tag(Tag::new(tt));
    }
    let tag = file
        .primary_tag_mut()
        .ok_or_else(|| "no writable tag".to_string())?;
    let title = match t.part {
        Some((n, c)) if c > 1 => format!("{} - Part {n:02}", t.title),
        _ => t.title.clone(),
    };
    tag.set_title(title);
    tag.set_album(t.title.clone());
    if !t.authors.is_empty() {
        let authors = t.authors.join(", ");
        tag.set_artist(authors.clone());
        tag.insert_text(ItemKey::AlbumArtist, authors);
    }
    if let Some(n) = t.narrator.as_deref().filter(|n| !n.trim().is_empty()) {
        tag.insert_text(ItemKey::Composer, n.trim().to_string());
    }
    tag.set_genre("Audiobook".to_string());
    if let Some(y) = t.year.and_then(|y| u32::try_from(y).ok()) {
        tag.set_year(y);
    }
    if let Some((n, c)) = t.part {
        tag.set_track(n);
        tag.set_track_total(c);
    }
    if let Some(d) = t.description.as_deref().filter(|d| !d.trim().is_empty()) {
        tag.set_comment(d.trim().to_string());
    }
    if let Some(cover) = &t.cover {
        let mime = match image_mime(cover) {
            Some("image/png") => Some(MimeType::Png),
            Some(_) => Some(MimeType::Jpeg),
            None => None,
        };
        if mime.is_some() {
            tag.remove_picture_type(PictureType::CoverFront);
            tag.push_picture(Picture::new_unchecked(
                PictureType::CoverFront,
                mime,
                None,
                cover.clone(),
            ));
        }
    }
    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A few silent MPEG-1 Layer III frames (128 kbps, 44.1 kHz).
    pub(crate) fn mp3_bytes(frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..frames {
            out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
            out.extend(std::iter::repeat_n(0u8, 417 - 4));
        }
        out
    }

    #[test]
    fn writes_and_reads_mp3_tags() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("part.mp3");
        std::fs::write(&p, mp3_bytes(40)).unwrap();
        let tags = BookTags {
            title: "The Final Empire".into(),
            authors: vec!["Brandon Sanderson".into()],
            narrator: Some("Michael Kramer".into()),
            part: Some((2, 3)),
            cover: Some(vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0]),
            ..Default::default()
        };
        write(&p, &tags).unwrap();
        let f = lofty::read_from_path(&p).unwrap();
        let tag = f.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("The Final Empire - Part 02"));
        assert_eq!(tag.album().as_deref(), Some("The Final Empire"));
        assert_eq!(tag.artist().as_deref(), Some("Brandon Sanderson"));
        assert_eq!(tag.track(), Some(2));
        assert_eq!(tag.pictures().len(), 1);
        assert_eq!(narrator(&p).as_deref(), Some("Michael Kramer"));
        assert!(probe(&p).is_some());
    }
}
