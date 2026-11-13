//! Merging a multi-part audiobook into one chaptered M4B (RFX-87).
//!
//! An optional job the person starts (`book_merge`, `POST /bookfiles/{id}/merge`). It:
//! 1. reads each part with ffprobe (length, codec, title, track and disc tags);
//! 2. orders the parts by disc and track tags when every part has a track number, otherwise
//!    keeps the file-name order the library already uses;
//! 3. joins them with ffmpeg into `<book folder>/<audiobook file name>.m4b`, one chapter per part
//!    (titles from the part's title tag, then its file name), copying AAC audio as it is and
//!    encoding anything else to AAC;
//! 4. checks the result (chapter count and length) before anything else changes.
//!
//! The parts stay untouched until the new file is checked and recorded; they are then removed
//! the way Reelix removes replaced files (into the recycle bin when one is set). Any failure
//! removes the unfinished file and leaves the parts and the record as they were.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use reelix_core::BookFormat;
use reelix_db::NewJob;
use serde::Deserialize;

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::kinds;
use crate::naming::BookNaming;
use crate::repo::files::{self, FileRow};
use crate::service::path_of;

/// Most parts (chapters) one merge takes.
pub const MAX_PARTS: usize = 2000;
/// Longest a single ffmpeg run may take.
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(6 * 3600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// AAC bitrate used when encoding, unless the parts' own is lower or higher (kbps).
const DEFAULT_KBPS: u32 = 64;
const MIN_KBPS: u32 = 32;
const MAX_KBPS: u32 = 128;

/// ffmpeg and ffprobe programs (tests point these at stand-ins).
static TOOLS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

fn tools() -> (PathBuf, PathBuf) {
    TOOLS.get().cloned().unwrap_or_else(|| {
        let env = |k: &str, d: &str| std::env::var_os(k).map_or_else(|| PathBuf::from(d), PathBuf::from);
        (env("REELIX_FFMPEG", "ffmpeg"), env("REELIX_FFPROBE", "ffprobe"))
    })
}

/// What one part holds, from ffprobe.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PartProbe {
    pub duration_ms: u64,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bit_rate: Option<u32>,
    pub title: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
}

/// Outcome of a merge.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeOutcome {
    pub file_id: i64,
    /// Relative to the author's audiobook folder.
    pub relative_path: String,
    pub chapters: usize,
    /// True when the audio was copied, false when it was encoded to AAC.
    pub copied: bool,
    pub removed_parts: usize,
}

#[derive(Debug, Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: ProbeFormat,
    #[serde(default)]
    chapters: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct ProbeStream {
    #[serde(default)]
    codec_name: String,
    sample_rate: Option<String>,
    channels: Option<u32>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

/// Parses `ffprobe -print_format json -show_format -show_streams [-show_chapters]` output.
/// Returns the part and the number of chapters it already has.
pub fn parse_probe(json: &[u8]) -> Option<(PartProbe, usize)> {
    let p: ProbeJson = serde_json::from_slice(json).ok()?;
    let s = p.streams.first()?;
    let secs: f64 = p.format.duration.as_deref()?.trim().parse().ok()?;
    if !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    let tag = |k: &str| {
        p.format
            .tags
            .iter()
            .chain(s.tags.iter())
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    // `3/12` → 3.
    let number = |v: Option<String>| v.and_then(|v| v.split('/').next()?.trim().parse::<u32>().ok());
    let int = |v: Option<&String>| v.and_then(|v| v.trim().parse::<u32>().ok());
    Some((
        PartProbe {
            duration_ms: (secs * 1000.0).round() as u64,
            codec: s.codec_name.to_ascii_lowercase(),
            sample_rate: int(s.sample_rate.as_ref()),
            channels: s.channels,
            bit_rate: int(s.bit_rate.as_ref()).or_else(|| int(p.format.bit_rate.as_ref())),
            title: tag("title"),
            track: number(tag("track")),
            disc: number(tag("disc")),
        },
        p.chapters.len(),
    ))
}

/// Disc and track tags first, when every part has a distinct track number; otherwise the
/// given (file-name) order.
pub fn order_parts<T>(mut parts: Vec<(T, PartProbe)>) -> Vec<(T, PartProbe)> {
    let keys: Vec<Option<(u32, u32)>> = parts
        .iter()
        .map(|(_, p)| p.track.map(|t| (p.disc.unwrap_or(1), t)))
        .collect();
    let all = keys.iter().all(Option::is_some);
    let mut unique: Vec<(u32, u32)> = keys.iter().flatten().copied().collect();
    unique.sort_unstable();
    unique.dedup();
    if all && unique.len() == parts.len() {
        parts.sort_by_key(|(_, p)| (p.disc.unwrap_or(1), p.track));
    }
    parts
}

/// A title that says nothing beyond a number (`Track 03`, `Part 2`, `CD1 - 04`, `07`).
fn generic(title: &str) -> bool {
    let words: Vec<String> = title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    words.iter().all(|w| {
        w.chars().all(|c| c.is_ascii_digit())
            || matches!(
                w.trim_end_matches(|c: char| c.is_ascii_digit()),
                "track" | "part" | "pt" | "chapter" | "ch" | "disc" | "disk" | "cd" | "of"
            )
    })
}

/// `01 - The Boy Who Lived` → `The Boy Who Lived`.
fn without_number(stem: &str) -> &str {
    stem.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace() || "-_.:".contains(c))
}

/// The chapter title of part `index` (0-based).
pub fn chapter_title(path: &Path, tag_title: Option<&str>, book_title: &str, index: usize) -> String {
    let fallback = format!("Chapter {}", index + 1);
    if let Some(t) = tag_title.map(str::trim).filter(|t| !t.is_empty())
        && !t.eq_ignore_ascii_case(book_title.trim())
        && !generic(t)
    {
        return t.chars().take(200).collect();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = without_number(&stem).trim();
    if stem.is_empty() || generic(stem) || stem.eq_ignore_ascii_case(book_title.trim()) {
        return fallback;
    }
    // `Book Title - Part 03 - Name`: only the part after the book title says anything.
    let rest = stem
        .strip_prefix(book_title.trim())
        .map(|r| without_number(r.trim_start_matches([' ', '-', '_'])).trim())
        .unwrap_or(stem);
    if rest.is_empty() || generic(rest) {
        fallback
    } else {
        rest.chars().take(200).collect()
    }
}

/// Escapes a value for an ffmetadata file.
fn meta_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '=' | ';' | '#' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// An ffmetadata file with the book's title and artist and one chapter per part.
pub fn ffmetadata(title: &str, artist: &str, chapters: &[(String, u64)]) -> String {
    let mut out = format!(
        ";FFMETADATA1\ntitle={}\nartist={}\n",
        meta_escape(title),
        meta_escape(artist)
    );
    let mut start = 0u64;
    for (name, len) in chapters {
        let end = start + len;
        out.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={start}\nEND={end}\ntitle={}\n",
            meta_escape(name)
        ));
        start = end;
    }
    out
}

/// One line of an ffmpeg concat list (single quotes escaped as `'\''`).
pub fn concat_line(path: &Path) -> String {
    format!("file '{}'\n", path.to_string_lossy().replace('\'', "'\\''"))
}

/// `None` copies the audio (every part AAC with the same sample rate and channels); otherwise
/// the AAC bitrate to encode at.
pub fn encode_kbps(parts: &[PartProbe]) -> Option<u32> {
    let first = parts.first()?;
    let copy = parts
        .iter()
        .all(|p| p.codec == "aac" && p.sample_rate == first.sample_rate && p.channels == first.channels);
    if copy {
        return None;
    }
    let kbps = parts
        .iter()
        .filter_map(|p| p.bit_rate)
        .max()
        .map_or(DEFAULT_KBPS, |b| (b / 1000).clamp(MIN_KBPS, MAX_KBPS));
    Some(kbps)
}

/// Checks a merged file: as many chapters as parts, and a length within 1 % (at least two
/// seconds) of the parts together.
pub fn check_output(probe: &PartProbe, chapters: usize, want_chapters: usize, want_ms: u64) -> Result<()> {
    if chapters != want_chapters {
        return Err(BooksError::Validation(format!(
            "the merged file has {chapters} chapters, not {want_chapters}"
        )));
    }
    let slack = (want_ms / 100).max(2000);
    if probe.duration_ms.abs_diff(want_ms) > slack {
        return Err(BooksError::Validation(format!(
            "the merged file is {} seconds long, not {}",
            probe.duration_ms / 1000,
            want_ms / 1000
        )));
    }
    Ok(())
}

async fn run(mut cmd: tokio::process::Command, timeout: Duration, what: &str) -> Result<Vec<u8>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| {
        BooksError::Validation(format!(
            "{what} could not be started ({e}); merging needs ffmpeg and ffprobe on the PATH"
        ))
    })?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            BooksError::Internal(format!(
                "{what} took longer than {} minutes",
                timeout.as_secs() / 60
            ))
        })??;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no message");
        return Err(BooksError::Internal(format!(
            "{what} failed: {}",
            last.chars().take(300).collect::<String>()
        )));
    }
    Ok(out.stdout)
}

async fn probe(ffprobe: &Path, file: &Path) -> Result<(PartProbe, usize)> {
    let mut cmd = tokio::process::Command::new(ffprobe);
    cmd.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        "-show_chapters",
        "-select_streams",
        "a:0",
    ])
    .arg(file);
    let out = run(cmd, PROBE_TIMEOUT, "ffprobe").await?;
    parse_probe(&out).ok_or_else(|| {
        BooksError::Validation(format!(
            "{} has no readable audio",
            file.file_name().unwrap_or_default().to_string_lossy()
        ))
    })
}

/// Removes the unfinished output and the helper files whatever happens.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl BooksService {
    /// Points the merge at other ffmpeg and ffprobe programs (tests). Later calls are ignored.
    #[doc(hidden)]
    pub fn set_merge_tools(ffmpeg: PathBuf, ffprobe: PathBuf) {
        let _ = TOOLS.set((ffmpeg, ffprobe));
    }

    /// The audiobook record `file_id`, checked to be one a merge can take.
    async fn mergeable(&self, file_id: i64) -> Result<FileRow> {
        let f = files::get(&self.db, file_id)
            .await?
            .ok_or_else(|| BooksError::NotFound(format!("book file {file_id} not found")))?;
        if f.format != BookFormat::Audiobook {
            return Err(BooksError::Validation("only audiobooks can be merged".into()));
        }
        if f.parts.len() < 2 {
            return Err(BooksError::Validation(
                "this audiobook is already one file".into(),
            ));
        }
        if f.parts.len() > MAX_PARTS {
            return Err(BooksError::Validation(format!(
                "this audiobook has {} parts; at most {MAX_PARTS} can be merged",
                f.parts.len()
            )));
        }
        Ok(f)
    }

    /// Queues the merge of a multi-part audiobook (job `book_merge`).
    pub async fn queue_merge(&self, file_id: i64) -> Result<Option<i64>> {
        self.mergeable(file_id).await?;
        let mut job = NewJob::new(kinds::BOOK_MERGE, serde_json::json!({ "file_id": file_id }))
            .dedupe(format!("{}:{file_id}", kinds::BOOK_MERGE));
        // A failed merge changed nothing; running it again would only fail the same way.
        job.max_attempts = 1;
        self.enqueue(job).await
    }

    /// Merges the parts of audiobook record `file_id` into one chaptered M4B.
    pub async fn merge_book_file(&self, file_id: i64) -> Result<MergeOutcome> {
        let f = self.mergeable(file_id).await?;
        let b = self.book_row(f.book_id).await?;
        let a = self.author_row(b.author_id).await?;
        let base = PathBuf::from(
            path_of(&a, BookFormat::Audiobook)
                .ok_or_else(|| BooksError::Validation("the author has no audiobook folder".into()))?,
        );
        let guard = self.path_guard().await?;
        let (ffmpeg, ffprobe) = tools();

        // 1. Read every part.
        let mut probed = Vec::with_capacity(f.parts.len());
        for part in &f.parts {
            let p = base.join(&part.relative_path);
            guard.check(&p)?;
            if !p.is_file() {
                return Err(BooksError::Validation(format!(
                    "part {} is missing; rescan the author first",
                    part.relative_path
                )));
            }
            let (info, _) = probe(&ffprobe, &p).await?;
            probed.push((p, info));
        }
        let ordered = order_parts(probed);
        let infos: Vec<PartProbe> = ordered.iter().map(|(_, i)| i.clone()).collect();
        let kbps = encode_kbps(&infos);
        let total_ms: u64 = infos.iter().map(|i| i.duration_ms).sum();
        let chapters: Vec<(String, u64)> = ordered
            .iter()
            .enumerate()
            .map(|(i, (p, info))| {
                (
                    chapter_title(p, info.title.as_deref(), &b.title, i),
                    info.duration_ms,
                )
            })
            .collect();

        // 2. Where the merged file goes: the parts' folder, named like a one-file audiobook.
        let folder_rel = Path::new(&f.relative_path);
        let folder = base.join(folder_rel);
        guard.check(&folder)?;
        let settings = self.settings().await?;
        let config = self.naming_config().await?;
        let mut naming = BookNaming::new(&a, &b);
        naming.narrator.clone_from(&f.narrator);
        let stem = {
            let s = naming.render(&settings.naming.audiobook_file, &config);
            if s.trim().is_empty() {
                reelix_import::naming::sanitize(&b.title, &config)
            } else {
                s
            }
        };
        let stem = if stem.trim().is_empty() {
            "Audiobook".to_string()
        } else {
            stem
        };
        let parts_set: Vec<&PathBuf> = ordered.iter().map(|(p, _)| p).collect();
        let mut name = format!("{stem}.m4b");
        if folder.join(&name).exists() && !parts_set.contains(&&folder.join(&name)) {
            name = format!("{stem} (merged).m4b");
        }
        let target = folder.join(&name);
        guard.check(&target)?;
        if target.exists() {
            return Err(BooksError::Conflict(format!("{name} already exists")));
        }
        let partial = folder.join(format!(".{stem}.reelix-partial.m4b"));
        let list = folder.join(".reelix-merge-list.txt");
        let meta = folder.join(".reelix-merge-chapters.txt");
        let mut cleanup = Cleanup(vec![partial.clone(), list.clone(), meta.clone()]);
        let artist = b
            .credits
            .iter()
            .filter(|c| c.role.as_deref().is_none_or(|r| r == "author"))
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let artist = if artist.is_empty() { a.name.clone() } else { artist };
        std::fs::write(
            &list,
            ordered.iter().map(|(p, _)| concat_line(p)).collect::<String>(),
        )?;
        std::fs::write(&meta, ffmetadata(&b.title, &artist, &chapters))?;

        // 3. Join.
        let mut cmd = tokio::process::Command::new(&ffmpeg);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg(&list)
        .args(["-f", "ffmetadata", "-i"])
        .arg(&meta)
        .args(["-map", "0:a:0", "-map_metadata", "1", "-map_chapters", "1"]);
        match kbps {
            None => cmd.args(["-c:a", "copy"]),
            Some(k) => cmd.args(["-c:a", "aac", "-b:a"]).arg(format!("{k}k")),
        };
        cmd.args(["-threads", "1", "-movflags", "+faststart", "-f", "ipod"])
            .arg(&partial);
        tracing::info!(
            file_id,
            parts = chapters.len(),
            copy = kbps.is_none(),
            "merging audiobook parts"
        );
        run(cmd, FFMPEG_TIMEOUT, "ffmpeg").await?;

        // 4. Check, then move into place.
        let (out, out_chapters) = probe(&ffprobe, &partial).await?;
        check_output(&out, out_chapters, chapters.len(), total_ms)?;
        if settings.write_metadata {
            let mut tags = self.book_tags(&a, &b, f.narrator.clone()).await?;
            if settings.write_covers {
                tags.cover = self.fetch_cover(b.images.poster.as_deref()).await;
            }
            let p = partial.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || crate::tags::m4b::write(&p, &tags))
                .await
                .map_err(|e| BooksError::Internal(e.to_string()))?
            {
                tracing::warn!(file_id, error = %e, "tags not written into the merged audiobook");
            }
        }
        std::fs::rename(&partial, &target)?;
        cleanup.0.retain(|p| p != &partial);
        drop(cleanup);

        // 5. Record the new file, then remove the parts.
        let mut row = f.clone();
        row.relative_path = folder_rel.join(&name).to_string_lossy().replace('\\', "/");
        row.parts = Vec::new();
        row.size = std::fs::metadata(&target)?.len();
        row.duration_seconds = u32::try_from(out.duration_ms / 1000).ok().filter(|s| *s > 0);
        row.bitrate_kbps = out.bit_rate.map(|b| b / 1000).filter(|b| *b > 0);
        files::upsert(&self.db, &row).await?;
        let mut removed = 0;
        for (p, _) in &ordered {
            match self.remove_path(p).await {
                Ok(_) => removed += 1,
                Err(e) => tracing::warn!(path = %p.display(), error = %e, "merged part not removed"),
            }
        }
        self.emit("book_file", "updated", f.id);
        self.emit("book", "updated", f.book_id);
        tracing::info!(file_id, path = %target.display(), removed, "audiobook merged");
        Ok(MergeOutcome {
            file_id: f.id,
            relative_path: row.relative_path,
            chapters: chapters.len(),
            copied: kbps.is_none(),
            removed_parts: removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(track: Option<u32>, disc: Option<u32>) -> PartProbe {
        PartProbe {
            duration_ms: 1000,
            codec: "mp3".into(),
            track,
            disc,
            ..PartProbe::default()
        }
    }

    #[test]
    fn parses_ffprobe_output() {
        let json = br#"{"streams":[{"codec_name":"mp3","sample_rate":"44100","channels":1,"bit_rate":"64000","tags":{}}],
            "format":{"duration":"4.048980","bit_rate":"64565","tags":{"title":"Chapter 1","track":"3/12","disc":"2"}},
            "chapters":[{}]}"#;
        let (p, chapters) = parse_probe(json).unwrap();
        assert_eq!(p.duration_ms, 4049);
        assert_eq!(p.codec, "mp3");
        assert_eq!(p.sample_rate, Some(44100));
        assert_eq!(p.bit_rate, Some(64000));
        assert_eq!(p.title.as_deref(), Some("Chapter 1"));
        assert_eq!(p.track, Some(3));
        assert_eq!(p.disc, Some(2));
        assert_eq!(chapters, 1);
        assert!(parse_probe(br#"{"streams":[],"format":{"duration":"1"}}"#).is_none());
        assert!(parse_probe(b"nonsense").is_none());
    }

    #[test]
    fn orders_by_tags_only_when_every_part_has_one() {
        let parts = vec![
            ("a", part(Some(2), Some(1))),
            ("b", part(Some(1), Some(2))),
            ("c", part(Some(1), Some(1))),
        ];
        let names: Vec<&str> = order_parts(parts).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
        let parts = vec![("a", part(Some(2), None)), ("b", part(None, None))];
        let names: Vec<&str> = order_parts(parts).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a", "b"], "a missing tag keeps the name order");
        let parts = vec![("a", part(Some(1), None)), ("b", part(Some(1), None))];
        let names: Vec<&str> = order_parts(parts).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a", "b"], "repeated tags keep the name order");
    }

    #[test]
    fn chapter_titles() {
        let p = Path::new("/x/01 - The Boy Who Lived.mp3");
        assert_eq!(chapter_title(p, None, "Book", 0), "The Boy Who Lived");
        assert_eq!(chapter_title(p, Some("Track 01"), "Book", 0), "The Boy Who Lived");
        assert_eq!(chapter_title(p, Some("A Real Title"), "Book", 0), "A Real Title");
        assert_eq!(chapter_title(p, Some("Book"), "Book", 0), "The Boy Who Lived");
        let p = Path::new("/x/Dune - Part 03.mp3");
        assert_eq!(chapter_title(p, None, "Dune", 2), "Chapter 3");
        let p = Path::new("/x/Dune - Part 03 - Arrakis.mp3");
        assert_eq!(chapter_title(p, None, "Dune", 2), "Part 03 - Arrakis");
        assert_eq!(
            chapter_title(Path::new("/x/07.mp3"), None, "Dune", 6),
            "Chapter 7"
        );
        assert_eq!(
            chapter_title(Path::new("/x/CD1 - 04.mp3"), Some("  "), "Dune", 3),
            "Chapter 4"
        );
    }

    #[test]
    fn metadata_and_concat_files() {
        let m = ffmetadata(
            "Dune; Part=1",
            "Frank Herbert",
            &[("One".into(), 4000), ("Two#".into(), 5000)],
        );
        assert!(m.starts_with(";FFMETADATA1\ntitle=Dune\\; Part\\=1\nartist=Frank Herbert\n"));
        assert!(m.contains("START=0\nEND=4000\ntitle=One\n"));
        assert!(m.contains("START=4000\nEND=9000\ntitle=Two\\#\n"));
        assert_eq!(
            concat_line(Path::new("/a/book's dir/p.mp3")),
            "file '/a/book'\\''s dir/p.mp3'\n"
        );
    }

    #[test]
    fn copies_aac_and_encodes_the_rest() {
        let aac = PartProbe {
            codec: "aac".into(),
            sample_rate: Some(44100),
            channels: Some(2),
            bit_rate: Some(64000),
            ..PartProbe::default()
        };
        assert_eq!(encode_kbps(&[aac.clone(), aac.clone()]), None);
        let mut other_rate = aac.clone();
        other_rate.sample_rate = Some(22050);
        assert_eq!(encode_kbps(&[aac.clone(), other_rate]), Some(64));
        let mut mp3 = part(None, None);
        mp3.bit_rate = Some(320_000);
        assert_eq!(encode_kbps(&[mp3.clone()]), Some(MAX_KBPS));
        mp3.bit_rate = None;
        assert_eq!(encode_kbps(&[mp3]), Some(DEFAULT_KBPS));
    }

    #[test]
    fn checks_the_result() {
        let out = PartProbe {
            duration_ms: 600_000,
            ..PartProbe::default()
        };
        assert!(check_output(&out, 3, 3, 601_000).is_ok());
        assert!(check_output(&out, 2, 3, 600_000).is_err());
        assert!(check_output(&out, 3, 3, 700_000).is_err());
    }
}
