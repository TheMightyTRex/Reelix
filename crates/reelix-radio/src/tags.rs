//! Tags written into filed episodes (lofty), only ever into Reelix's own copy in the podcast
//! library, never into a downloaded file that may still be seeding.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};

/// What a filed episode is tagged with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpisodeTags {
    /// The show.
    pub album: String,
    /// The episode title.
    pub title: String,
    /// Filing series number (specials = 0).
    pub disc: u32,
    pub track: Option<u32>,
    /// First broadcast, `YYYY-MM-DD`.
    pub date: Option<String>,
    /// Always `Radio` when written.
    pub genre: String,
    /// The station.
    pub artist: Option<String>,
    /// Source id of the episode (`provider:external id`).
    pub comment: Option<String>,
}

/// Writes `t` into the file at `path` (which must be Reelix's own copy).
pub fn write(path: &Path, t: &EpisodeTags) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let mut tagged = Probe::open(path)
        .map_err(|e| err(&e))?
        .guess_file_type()
        .map_err(|e| err(&e))?
        .read()
        .map_err(|e| err(&e))?;
    if tagged.primary_tag().is_none() {
        let tt = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tt));
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return Err(err(&"file type does not support tags"));
    };
    tag.set_album(t.album.clone());
    if !t.title.trim().is_empty() {
        tag.set_title(t.title.clone());
    }
    tag.set_disk(t.disc);
    if let Some(n) = t.track {
        tag.set_track(n);
    }
    tag.set_genre(t.genre.clone());
    if let Some(a) = &t.artist {
        tag.set_artist(a.clone());
    }
    if let Some(c) = &t.comment {
        tag.set_comment(c.clone());
    }
    if let Some(d) = &t.date {
        // Formats without the key (e.g. RIFF INFO) silently skip it.
        tag.insert_text(ItemKey::RecordingDate, d.clone());
        if let Some(y) = d.get(..4).and_then(|y| y.parse().ok()) {
            tag.set_year(y);
        }
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| err(&e))
}

/// Reads the tags back (tests, diagnostics); `None` when the file has none.
pub fn read(path: &Path) -> Option<EpisodeTags> {
    let tagged = Probe::open(path).ok()?.guess_file_type().ok()?.read().ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let s = |v: Option<std::borrow::Cow<'_, str>>| v.map(|c| c.into_owned());
    Some(EpisodeTags {
        album: s(tag.album()).unwrap_or_default(),
        title: s(tag.title()).unwrap_or_default(),
        disc: tag.disk().unwrap_or(0),
        track: tag.track(),
        date: tag.get_string(&ItemKey::RecordingDate).map(str::to_owned),
        genre: s(tag.genre()).unwrap_or_default(),
        artist: s(tag.artist()),
        comment: s(tag.comment()),
    })
}
