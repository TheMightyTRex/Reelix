//! Disk scan of author folders: ebook files and audiobook units (single `.m4b` or a folder
//! of parts) matched to the author's books.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use reelix_api_types::books::{AuthorDto, BookDto, BookFilePartDto};
use reelix_core::BookFormat;
use reelix_decision::QualityProfile;
use reelix_import::files::{ScanOptions, scan};
use reelix_parser::{Quality, QualityModel, book_quality_from_extension, parse_book};

use crate::BooksService;
use crate::error::Result;
use crate::repo::books::BookFilter;
use crate::repo::{authors as author_repo, clean, editions, files};
use crate::service::{ScanSummary, path_of, profile_of};
use crate::tags;

pub(crate) const EBOOK_EXTS: &[&str] = &["epub", "azw3", "azw", "mobi", "pdf"];
pub(crate) const AUDIO_EXTS: &[&str] = &["m4b", "m4a", "mp3", "flac", "aac", "ogg", "opus"];

pub(crate) fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub(crate) fn format_of(p: &Path) -> Option<BookFormat> {
    let e = ext(p);
    if EBOOK_EXTS.contains(&e.as_str()) {
        Some(BookFormat::Ebook)
    } else if AUDIO_EXTS.contains(&e.as_str()) {
        Some(BookFormat::Audiobook)
    } else {
        None
    }
}

/// Quality from the file extension (`Unknown*` for audio types without their own quality).
pub(crate) fn quality_of(p: &Path, format: BookFormat) -> Quality {
    book_quality_from_extension(&ext(p)).unwrap_or(match format {
        BookFormat::Ebook => Quality::UnknownText,
        BookFormat::Audiobook => Quality::UnknownAudiobook,
    })
}

/// Natural order (`Part 2` before `Part 10`).
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.to_lowercase().chars() {
            let d = c.is_ascii_digit();
            match out.last_mut() {
                Some((ld, buf)) if *ld == d => buf.push(c),
                _ => out.push((d, c.to_string())),
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let o = match (x, y) {
            ((true, xs), (true, ys)) => {
                let xn = xs.trim_start_matches('0');
                let yn = ys.trim_start_matches('0');
                xn.len().cmp(&yn.len()).then_with(|| xn.cmp(yn))
            }
            ((_, xs), (_, ys)) => xs.cmp(ys),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    ca.len().cmp(&cb.len())
}

/// One book on disk: an ebook file, a single-file audiobook, or a folder of audiobook parts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Unit {
    pub format: BookFormat,
    /// The file, or the folder of a multi-file audiobook.
    pub path: PathBuf,
    /// Files in part order with sizes.
    pub files: Vec<(PathBuf, u64)>,
}

impl Unit {
    pub fn is_multi(&self) -> bool {
        self.files.len() > 1
    }

    pub fn quality(&self) -> Quality {
        self.files
            .first()
            .map_or(Quality::UnknownText, |(p, _)| quality_of(p, self.format))
    }

    pub fn size(&self) -> u64 {
        self.files.iter().map(|(_, s)| s).sum()
    }

    /// Names to match against book titles: file stem, then the containing folders.
    fn names(&self, base: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if !self.is_multi()
            && let Some(s) = self.path.file_stem()
        {
            out.push(s.to_string_lossy().into_owned());
        }
        let mut dir = if self.is_multi() {
            Some(self.path.as_path())
        } else {
            self.path.parent()
        };
        while let Some(d) = dir {
            if d == base || !d.starts_with(base) {
                break;
            }
            if let Some(n) = d.file_name() {
                out.push(n.to_string_lossy().into_owned());
            }
            dir = d.parent();
        }
        out
    }
}

/// Book units of `format` below `dir` (blocking). Audio files are grouped by folder; a
/// folder with several audio files is one multi-part unit.
pub(crate) fn find_units(dir: &Path, format: BookFormat) -> Vec<Unit> {
    let opts = ScanOptions {
        include_non_video: true,
        max_depth: 8,
    };
    let Ok(found) = scan(dir, &opts) else {
        return Vec::new();
    };
    let mine: Vec<(PathBuf, u64)> = found
        .into_iter()
        .filter(|f| format_of(&f.path) == Some(format))
        .map(|f| (f.path, f.size))
        .collect();
    match format {
        BookFormat::Ebook => mine
            .into_iter()
            .map(|(p, s)| Unit {
                format,
                path: p.clone(),
                files: vec![(p, s)],
            })
            .collect(),
        BookFormat::Audiobook => group_audio(mine, dir),
    }
}

/// Groups audio files into units: per folder, the files of the best-represented audio type.
pub(crate) fn group_audio(files: Vec<(PathBuf, u64)>, root: &Path) -> Vec<Unit> {
    let mut by_dir: HashMap<PathBuf, Vec<(PathBuf, u64)>> = HashMap::new();
    for (p, s) in files {
        let d = p.parent().map(Path::to_path_buf).unwrap_or_default();
        by_dir.entry(d).or_default().push((p, s));
    }
    let mut out = Vec::new();
    for (dir, mut fs) in by_dir {
        fs.sort_by(|a, b| natural_cmp(&a.0.to_string_lossy(), &b.0.to_string_lossy()));
        // Folders directly in the author folder with several single-file books (`*.m4b`) are
        // separate books; anything else is one book.
        let all_m4b = fs.iter().all(|(p, _)| ext(p) == "m4b");
        if fs.len() == 1 || (all_m4b && dir == root) {
            for (p, s) in fs {
                out.push(Unit {
                    format: BookFormat::Audiobook,
                    path: p.clone(),
                    files: vec![(p, s)],
                });
            }
            continue;
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (p, _) in &fs {
            *counts.entry(ext(p)).or_default() += 1;
        }
        let best = counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(e, _)| e)
            .unwrap_or_default();
        fs.retain(|(p, _)| ext(p) == best);
        out.push(Unit {
            format: BookFormat::Audiobook,
            path: dir,
            files: fs,
        });
    }
    out.sort_by(|a, b| natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy()));
    out
}

/// The best book for a name: exact clean-title match, else the longest title the name
/// contains (titles of 4+ characters).
pub(crate) fn match_name<'b>(name: &str, author: &str, books: &'b [BookDto]) -> Option<&'b BookDto> {
    let parsed = parse_book(name, None);
    let mut keys = vec![clean(&parsed.title), clean(name)];
    let a = clean(author);
    if !a.is_empty() {
        let k = clean(name);
        if let Some(rest) = k.strip_prefix(&a) {
            keys.push(rest.to_string());
        }
    }
    keys.retain(|k| !k.is_empty());
    let mut best: Option<(usize, &BookDto)> = None;
    for b in books {
        let t = clean(&b.title);
        if t.is_empty() {
            continue;
        }
        let full = b.subtitle.as_deref().map(|s| clean(&format!("{} {s}", b.title)));
        if keys
            .iter()
            .any(|k| *k == t || full.as_deref() == Some(k.as_str()))
        {
            return Some(b);
        }
        if t.len() >= 4 && keys.iter().any(|k| k.contains(&t)) && best.is_none_or(|(l, _)| t.len() > l) {
            best = Some((t.len(), b));
        }
    }
    best.map(|(_, b)| b)
}

/// Book of a unit: embedded EPUB ISBNs first, then the file/folder names.
pub(crate) async fn match_unit<'b>(
    svc: &BooksService,
    u: &Unit,
    base: &Path,
    author: &AuthorDto,
    books: &'b [BookDto],
) -> Option<&'b BookDto> {
    if u.format == BookFormat::Ebook && ext(&u.path) == "epub" {
        let p = u.path.clone();
        let info = tokio::task::spawn_blocking(move || tags::epub::read_info(&p).ok())
            .await
            .ok()
            .flatten();
        if let Some(info) = info {
            for isbn in &info.isbns {
                let ids = editions::books_by_identifier(&svc.db, Some(isbn), None)
                    .await
                    .unwrap_or_default();
                if let Some(b) = books.iter().find(|b| ids.contains(&b.id)) {
                    return Some(b);
                }
            }
            if let Some(b) = info
                .title
                .as_deref()
                .and_then(|t| match_name(t, &author.name, books))
            {
                return Some(b);
            }
        }
    }
    u.names(base)
        .iter()
        .find_map(|n| match_name(n, &author.name, books))
}

fn rank(profile: Option<&QualityProfile>, q: Quality) -> (bool, i64) {
    match profile {
        Some(p) => (p.is_allowed(q), p.rank(q).map_or(-1, |r| r as i64)),
        None => (true, i64::from(q.id())),
    }
}

/// Builds the file record of a unit (relative to `base`), probing audio duration/bitrate.
pub(crate) fn unit_record(u: &Unit, base: &Path, book_id: i64) -> files::FileRow {
    let rel = |p: &Path| {
        p.strip_prefix(base)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut row = files::FileRow::new(book_id, u.format, rel(&u.path), QualityModel::new(u.quality()));
    row.size = u.size();
    if u.is_multi() {
        row.parts = u
            .files
            .iter()
            .map(|(p, s)| BookFilePartDto {
                relative_path: rel(p),
                size: *s,
                duration_ms: None,
            })
            .collect();
    }
    if u.format == BookFormat::Audiobook {
        let mut total = 0u32;
        let mut bitrate = None;
        for (i, (p, _)) in u.files.iter().enumerate() {
            if let Some((secs, kbps)) = tags::m4b::probe(p) {
                total += secs;
                bitrate = bitrate.or(kbps);
                if let Some(part) = row.parts.get_mut(i) {
                    part.duration_ms = Some(u64::from(secs) * 1000);
                }
            }
        }
        row.duration_seconds = (total > 0).then_some(total);
        row.bitrate_kbps = bitrate;
        row.narrator = u.files.first().and_then(|(p, _)| tags::m4b::narrator(p));
    }
    row
}

impl BooksService {
    /// Scans the folders of the given authors (all when empty) and syncs the file records.
    pub async fn scan_authors(&self, ids: &[i64]) -> Result<ScanSummary> {
        let authors = author_repo::load(&self.db, (!ids.is_empty()).then_some(ids)).await?;
        let mut total = ScanSummary::default();
        for a in authors {
            let s = self.scan_author(&a).await?;
            if s != ScanSummary::default() {
                self.emit("author", "updated", a.id);
            }
            total += s;
        }
        Ok(total)
    }

    async fn scan_author(&self, a: &AuthorDto) -> Result<ScanSummary> {
        if a.custom {
            self.custom_books_from_files(a).await?;
        }
        let books = self
            .load_books(BookFilter {
                ids: None,
                author_ids: Some(&[a.id]),
            })
            .await?;
        let profiles = self.profiles().await?;
        let mut summary = ScanSummary::default();
        for format in BookFormat::ALL {
            let Some(base) = path_of(a, format).map(PathBuf::from) else {
                continue;
            };
            if !base.is_dir() {
                continue;
            }
            let profile = profiles.profile(profile_of(a, format));
            let dir = base.clone();
            let units = tokio::task::spawn_blocking(move || find_units(&dir, format))
                .await
                .unwrap_or_default();
            // Best unit per book.
            let mut chosen: HashMap<i64, Unit> = HashMap::new();
            for u in units {
                let Some(b) = match_unit(self, &u, &base, a, &books).await else {
                    tracing::debug!(path = %u.path.display(), "no book matches");
                    continue;
                };
                let better = chosen
                    .get(&b.id)
                    .is_none_or(|c| rank(profile, u.quality()) > rank(profile, c.quality()));
                if better {
                    chosen.insert(b.id, u);
                }
            }
            let existing: HashMap<i64, files::FileRow> =
                files::list(&self.db, Some(&books.iter().map(|b| b.id).collect::<Vec<_>>()))
                    .await?
                    .into_iter()
                    .filter(|f| f.format == format)
                    .map(|f| (f.book_id, f))
                    .collect();
            for b in &books {
                let old = existing.get(&b.id);
                let old_present = old.is_some_and(|f| f.file_paths().iter().all(|p| base.join(p).exists()));
                match chosen.remove(&b.id) {
                    Some(u) => {
                        let (bs, us) = (base.clone(), u.clone());
                        let id = b.id;
                        let rec = tokio::task::spawn_blocking(move || unit_record(&us, &bs, id))
                            .await
                            .map_err(|e| crate::BooksError::Internal(e.to_string()))?;
                        match old {
                            Some(o) if o.relative_path == rec.relative_path => {
                                if o.size != rec.size || o.parts.len() != rec.parts.len() {
                                    let mut r = rec;
                                    r.id = o.id;
                                    r.quality = o.quality;
                                    r.release_group.clone_from(&o.release_group);
                                    r.scene_name.clone_from(&o.scene_name);
                                    r.original_path.clone_from(&o.original_path);
                                    r.edition_id = o.edition_id;
                                    r.narrator = r.narrator.or_else(|| o.narrator.clone());
                                    r.date_added = o.date_added;
                                    files::upsert(&self.db, &r).await?;
                                    summary.updated += 1;
                                }
                            }
                            // Keep a still-present imported file over another copy on disk.
                            Some(_) if old_present => {}
                            Some(_) => {
                                files::upsert(&self.db, &rec).await?;
                                summary.updated += 1;
                            }
                            None => {
                                files::upsert(&self.db, &rec).await?;
                                summary.added += 1;
                            }
                        }
                    }
                    None => {
                        if let Some(o) = old
                            && !old_present
                        {
                            files::delete(&self.db, o.id).await?;
                            summary.removed += 1;
                        }
                    }
                }
            }
        }
        if summary != ScanSummary::default() {
            tracing::info!(
                author = %a.name,
                added = summary.added,
                updated = summary.updated,
                removed = summary.removed,
                "book scan"
            );
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["Part 10.mp3", "Part 2.mp3", "Part 1.mp3"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Part 1.mp3", "Part 2.mp3", "Part 10.mp3"]);
        assert_eq!(natural_cmp("01", "1"), Ordering::Equal);
    }

    #[test]
    fn groups_audio_units() {
        let root = Path::new("/a");
        let files = vec![
            (PathBuf::from("/a/Book One.m4b"), 10),
            (PathBuf::from("/a/Book Two.m4b"), 10),
            (PathBuf::from("/a/Three/02.mp3"), 5),
            (PathBuf::from("/a/Three/01.mp3"), 5),
            (PathBuf::from("/a/Three/cover.flac"), 1),
            (PathBuf::from("/a/Four/Four.m4b"), 7),
        ];
        let units = group_audio(files, root);
        assert_eq!(units.len(), 4);
        let three = units.iter().find(|u| u.path == Path::new("/a/Three")).unwrap();
        assert!(three.is_multi());
        assert_eq!(three.files[0].0, Path::new("/a/Three/01.mp3"));
        assert_eq!(three.files.len(), 2);
        assert_eq!(three.size(), 10);
    }
}
