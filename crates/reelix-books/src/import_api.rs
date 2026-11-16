//! Download import for the pipeline: one finished download → book files in the library.
//!
//! Every file of a book is staged in a hidden `.reelix-partial-…` folder inside the author's
//! folder (same filesystem as the destination), tagged there, and only then moved into place:
//! all files or none. When tags or covers will be written the files are copied instead of
//! hardlinked, so a file that is still seeding is never modified (D14).

use std::path::{Path, PathBuf};
use std::time::Duration;

use reelix_api_types::books::{AuthorDto, BookDto, BookSettingsDto};
use reelix_core::BookFormat;
use reelix_decision::{FormatInput, QualityProfile, Scored, is_upgrade, matching_formats, score_formats};
use reelix_import::content::ContentDownload;
use reelix_import::files::{PermissionSettings, TransferMode, TransferOptions, disk_space, transfer_with};
use reelix_parser::{ParsedRelease, QualityModel, parse_book};

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::naming::{BookNaming, destination};
use crate::rename::remove_empty_dirs;
use crate::repo::{editions, files};
use crate::scan::{Unit, ext, find_units, natural_cmp, quality_of};
use crate::service::{path_of, profile_of};
use crate::tags::{self, BookTags};

/// One imported book file.
#[derive(Debug, Clone)]
pub struct ImportedBookFile {
    pub book_id: i64,
    pub author_id: i64,
    pub format: BookFormat,
    pub file_id: i64,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub size: u64,
    pub quality: QualityModel,
    pub release_group: Option<String>,
    pub is_upgrade: bool,
    /// Files replaced by this import (moved to the recycle bin or deleted).
    pub replaced: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct BookImportOutcome {
    pub imported: Vec<ImportedBookFile>,
    pub rejections: Vec<String>,
}

/// Borrowed view of [`BooksService`] for the pipeline.
pub struct BookImportApi<'a> {
    pub(crate) svc: &'a BooksService,
}

/// Book units of a download: ebooks one per file; for audiobooks the whole download is one
/// book (parts in `CD1/`, `CD2/`… are merged), keeping the most common audio type.
pub(crate) fn download_units(source: &Path, format: BookFormat) -> Vec<Unit> {
    if source.is_file() {
        if crate::scan::format_of(source) != Some(format) {
            return Vec::new();
        }
        let size = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);
        return vec![Unit {
            format,
            path: source.to_path_buf(),
            files: vec![(source.to_path_buf(), size)],
        }];
    }
    let units = find_units(source, format);
    if format == BookFormat::Ebook || units.len() <= 1 {
        return units;
    }
    let mut all: Vec<(PathBuf, u64)> = units.into_iter().flat_map(|u| u.files).collect();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (p, _) in &all {
        *counts.entry(ext(p)).or_default() += 1;
    }
    let best = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(e, _)| e)
        .unwrap_or_default();
    all.retain(|(p, _)| ext(p) == best);
    all.sort_by(|a, b| natural_cmp(&a.0.to_string_lossy(), &b.0.to_string_lossy()));
    vec![Unit {
        format,
        path: source.to_path_buf(),
        files: all,
    }]
}

fn permissions(mm: &reelix_api_types::library::MediaManagementSettings) -> PermissionSettings {
    if !mm.set_permissions {
        return PermissionSettings::default();
    }
    let octal = |s: &Option<String>| s.as_deref().and_then(|m| u32::from_str_radix(m.trim(), 8).ok());
    PermissionSettings {
        file_mode: octal(&mm.file_mode),
        dir_mode: octal(&mm.dir_mode),
        uid: mm.chown_uid,
        gid: mm.chown_gid,
    }
}

fn has_writer(p: &Path) -> bool {
    matches!(
        ext(p).as_str(),
        "epub" | "m4b" | "m4a" | "mp3" | "flac" | "ogg" | "opus"
    )
}

/// Everything the blocking stage step needs.
struct StagePlan {
    stage: PathBuf,
    /// `(source, relative destination)` per file in part order.
    files: Vec<(PathBuf, String)>,
    mode: TransferMode,
    options: TransferOptions,
    tags: Option<BookTags>,
    /// Saved as `cover.jpg` next to multi-file audiobooks.
    cover_file: Option<(String, Vec<u8>)>,
}

/// Transfers every file into the stage folder and writes tags there. On error the stage is
/// removed and the sources are untouched.
fn stage(plan: &StagePlan) -> Result<()> {
    let run = || -> Result<()> {
        std::fs::create_dir_all(&plan.stage)?;
        let count = plan.files.len() as u32;
        for (i, (src, rel)) in plan.files.iter().enumerate() {
            let dst = plan.stage.join(rel);
            transfer_with(src, &dst, plan.mode, &plan.options)?;
            if let Some(t) = &plan.tags
                && has_writer(&dst)
            {
                let mut t = t.clone();
                if count > 1 {
                    t.part = Some((i as u32 + 1, count));
                }
                if let Err(e) = tags::write(&dst, &t) {
                    tracing::warn!(file = %dst.display(), error = %e, "writing book metadata failed");
                }
            }
        }
        if let Some((rel, data)) = &plan.cover_file {
            let p = plan.stage.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(p, data)?;
        }
        Ok(())
    };
    let res = run();
    if res.is_err() {
        let _ = std::fs::remove_dir_all(&plan.stage);
    }
    res
}

/// Moves the staged files into `base` (folders are renamed as a whole when the destination
/// does not exist yet), then removes the stage.
fn commit(stage: &Path, base: &Path, rels: &[String], extra: Option<&str>) -> Result<()> {
    let tops: Vec<String> = {
        let mut t: Vec<String> = rels
            .iter()
            .chain(extra.map(str::to_string).iter())
            .filter_map(|r| r.split('/').next().map(str::to_string))
            .collect();
        t.sort();
        t.dedup();
        t
    };
    let res = (|| -> Result<()> {
        for top in &tops {
            let from = stage.join(top);
            let to = base.join(top);
            if from.is_dir() && !to.exists() {
                std::fs::rename(&from, &to)?;
                continue;
            }
            // Destination folder exists (or a single file): move file by file.
            for rel in rels
                .iter()
                .map(String::as_str)
                .chain(extra)
                .filter(|r| r.split('/').next() == Some(top))
            {
                let (f, t) = (stage.join(rel), base.join(rel));
                if let Some(parent) = t.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&f, &t)?;
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(stage);
    res
}

impl BooksService {
    async fn fetch_cover(&self, url: Option<&str>) -> Option<Vec<u8>> {
        let url = url.filter(|u| u.starts_with("http://") || u.starts_with("https://"))?;
        let res = self
            .http
            .get(url)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            tracing::debug!(url, status = %res.status(), "cover download failed");
            return None;
        }
        let bytes = res.bytes().await.ok()?;
        tags::image_mime(&bytes).map(|_| bytes.to_vec())
    }

    async fn book_tags(&self, a: &AuthorDto, b: &BookDto, narrator: Option<String>) -> Result<BookTags> {
        let preferred = editions::list(&self.db, b.id)
            .await?
            .into_iter()
            .find(|e| e.monitored);
        let mut authors: Vec<String> = b
            .credits
            .iter()
            .filter(|c| c.role.as_deref().is_none_or(|r| r == "author"))
            .map(|c| c.name.clone())
            .collect();
        if authors.is_empty() {
            authors.push(a.name.clone());
        }
        let series = b.series.first();
        Ok(BookTags {
            title: b.title.clone(),
            authors,
            narrator,
            series: series.map(|s| s.title.clone()),
            series_position: series.and_then(|s| s.position.clone()),
            isbn: preferred
                .as_ref()
                .and_then(|e| e.isbn13.clone())
                .or_else(|| b.isbn13.clone()),
            year: b.year,
            language: preferred.and_then(|e| e.language).or_else(|| b.language.clone()),
            description: b.overview.clone(),
            cover: None,
            part: None,
        })
    }
}

impl BookImportApi<'_> {
    /// Imports one finished download for `book_id`/`format` (multi-file audiobooks
    /// atomically), applying the profile (upgrade check), naming, metadata/cover writing and
    /// Calibre.
    pub async fn import_download(
        &self,
        d: &ContentDownload,
        book_id: i64,
        format: BookFormat,
    ) -> Result<BookImportOutcome> {
        let svc = self.svc;
        let b = svc.book_row(book_id).await?;
        let a = svc.author_row(b.author_id).await?;
        let mut out = BookImportOutcome::default();
        let profile_id = profile_of(&a, format).ok_or_else(|| {
            BooksError::Validation(format!("{} has no {} quality profile", a.name, format.as_str()))
        })?;
        let profiles = svc.profiles().await?;
        let profile: QualityProfile = profiles
            .profile(Some(profile_id))
            .cloned()
            .ok_or_else(|| BooksError::Validation(format!("quality profile {profile_id} does not exist")))?;
        let base = PathBuf::from(path_of(&a, format).ok_or_else(|| {
            BooksError::Validation(format!("{} has no {} folder", a.name, format.as_str()))
        })?);

        // 1. What is in the download.
        let src = d.source.clone();
        let units = tokio::task::spawn_blocking(move || download_units(&src, format))
            .await
            .map_err(|e| BooksError::Internal(e.to_string()))?;
        if units.is_empty() {
            out.rejections.push(format!(
                "no {} files found in {}",
                format.as_str(),
                d.source.display()
            ));
            return Ok(out);
        }
        let rank = |u: &Unit| profile.rank(u.quality()).map_or(-1, |r| r as i64);
        let Some(unit) = units
            .iter()
            .filter(|u| profile.is_allowed(u.quality()))
            .max_by_key(|u| (rank(u), u.size()))
            .cloned()
        else {
            let found: Vec<&str> = units.iter().map(|u| u.quality().name()).collect();
            out.rejections.push(format!(
                "{} is not wanted by the quality profile {}",
                found.join(", "),
                profile.name
            ));
            return Ok(out);
        };

        // 2. Quality, custom formats and the upgrade check.
        let release_title = d.release_title.clone().unwrap_or_else(|| {
            d.source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let parsed = parse_book(&release_title, Some(format));
        let quality = QualityModel {
            quality: quality_of(&unit.files[0].0, format),
            revision: parsed.quality.revision,
        };
        let mut release: ParsedRelease = parsed.to_release();
        release.quality = quality;
        let size = unit.size();
        let input = FormatInput {
            size: Some(size),
            ..FormatInput::from_parsed(&release)
        };
        let matched = matching_formats(&profiles.formats, &input);
        let score = score_formats(&profile, &matched);
        let format_ids: Vec<i64> = matched.iter().map(|f| f.id).collect();
        let existing = files::for_book(&svc.db, book_id, format).await?;
        if let Some(e) = &existing {
            let present = e.file_paths().iter().all(|p| base.join(p).exists());
            let old = Scored {
                quality: e.quality,
                format_score: e.custom_format_score,
            };
            let new = Scored {
                quality,
                format_score: score,
            };
            if present && !is_upgrade(&old, &new, &profile) {
                out.rejections.push(format!(
                    "not an upgrade for the existing {} file ({})",
                    format.as_str(),
                    e.quality.quality.name()
                ));
                return Ok(out);
            }
        }

        // 3. Free space.
        let mm = svc.library.media_management().await?;
        if !mm.skip_free_space_check {
            let probe = base
                .ancestors()
                .find(|p| p.is_dir())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| base.clone());
            if let Ok(space) = disk_space(&probe) {
                let need = size + mm.minimum_free_space_mb * 1024 * 1024;
                if space.available < need {
                    out.rejections.push(format!(
                        "not enough free space in {} ({} MB free)",
                        probe.display(),
                        space.available / (1024 * 1024)
                    ));
                    return Ok(out);
                }
            }
        }

        // 4. Destination names.
        let settings: BookSettingsDto = svc.settings().await?;
        let config = svc.naming_config().await?;
        let mut naming = BookNaming::new(&a, &b);
        naming.quality = Some(quality.quality);
        // The release name's narrator first, then the recording's (RFX-86).
        let narrator = match parsed.narrator.clone() {
            Some(n) => Some(n),
            None if format == BookFormat::Audiobook => svc.stored_narrators(b.id).await,
            None => None,
        };
        naming.narrator.clone_from(&narrator);
        let sources: Vec<&Path> = unit.files.iter().map(|(p, _)| p.as_path()).collect();
        let (relative, parts) = destination(&settings.naming, &naming, format, &sources, &config);
        let rels: Vec<String> = if parts.is_empty() {
            vec![relative.clone()]
        } else {
            parts.clone()
        };
        let guard = {
            let mut g = svc.path_guard().await?;
            if !g.contains(&base)
                && let Some(root) = match format {
                    BookFormat::Ebook => a.root_folder_id,
                    BookFormat::Audiobook => a.audio_root_folder_id,
                }
                && let Some(r) = reelix_library::content_roots::get(&svc.db, root).await?
            {
                std::fs::create_dir_all(&r.path)?;
                g.add_root(Path::new(&r.path))?;
            }
            g
        };
        for r in &rels {
            guard.check(&base.join(r))?;
        }

        // 5. Stage (copy when tags will be written), write tags, commit.
        let writes = settings.write_metadata && unit.files.iter().any(|(p, _)| has_writer(p));
        let cover = if settings.write_covers && (writes || format == BookFormat::Audiobook) {
            svc.fetch_cover(b.images.poster.as_deref()).await
        } else {
            None
        };
        let mut tags = if writes {
            Some(svc.book_tags(&a, &b, narrator.clone()).await?)
        } else {
            None
        };
        if let Some(t) = &mut tags {
            t.cover = cover.clone();
        }
        let stage_mode = if writes {
            TransferMode::Copy
        } else {
            match d.mode {
                TransferMode::Copy => TransferMode::Copy,
                TransferMode::Hardlink => TransferMode::Hardlink,
                TransferMode::HardlinkOrCopy | TransferMode::Move => TransferMode::HardlinkOrCopy,
            }
        };
        let cover_file = match (&cover, format) {
            (Some(c), BookFormat::Audiobook) => {
                let folder = relative.split('/').next().unwrap_or_default().to_string();
                let name = if tags::image_mime(c) == Some("image/png") {
                    "cover.png"
                } else {
                    "cover.jpg"
                };
                Some((format!("{folder}/{name}"), c.clone()))
            }
            _ => None,
        };
        let plan = StagePlan {
            stage: base.join(format!(".reelix-partial-{book_id}-{}", format.as_str())),
            files: unit
                .files
                .iter()
                .map(|(p, _)| p.clone())
                .zip(rels.iter().cloned())
                .collect(),
            mode: stage_mode,
            options: TransferOptions {
                overwrite: true,
                permissions: permissions(&mm),
                preserve_mtime: true,
            },
            tags,
            cover_file,
        };
        std::fs::create_dir_all(&base)?;
        let plan = tokio::task::spawn_blocking(move || stage(&plan).map(|()| plan))
            .await
            .map_err(|e| BooksError::Internal(e.to_string()))??;

        // Replace the old file(s), then move the staged ones into place.
        let mut replaced = Vec::new();
        if let Some(e) = &existing {
            replaced = svc.remove_book_file(e).await?;
        }
        for r in &rels {
            let dst = base.join(r);
            if dst.exists() {
                svc.remove_path(&dst).await?;
            }
        }
        let extra = plan.cover_file.as_ref().map(|(r, _)| r.clone());
        let (stage_dir, base2, rels2) = (plan.stage.clone(), base.clone(), rels.clone());
        tokio::task::spawn_blocking(move || commit(&stage_dir, &base2, &rels2, extra.as_deref()))
            .await
            .map_err(|e| BooksError::Internal(e.to_string()))??;

        // Move mode: the sources go once the library copy is complete.
        if d.mode == TransferMode::Move {
            for (src, _) in &unit.files {
                if let Err(e) = std::fs::remove_file(src) {
                    tracing::warn!(file = %src.display(), error = %e, "removing source failed");
                }
            }
            if d.source.is_dir() {
                remove_empty_dirs(
                    unit.files.first().and_then(|(p, _)| p.parent()),
                    d.source.parent().unwrap_or(&d.source),
                );
            }
        }

        // 6. Record.
        let dest_unit = Unit {
            format,
            path: base.join(&relative),
            files: rels
                .iter()
                .zip(&unit.files)
                .map(|(r, (_, s))| (base.join(r), *s))
                .collect(),
        };
        let bu = base.clone();
        let mut row = tokio::task::spawn_blocking(move || crate::scan::unit_record(&dest_unit, &bu, book_id))
            .await
            .map_err(|e| BooksError::Internal(e.to_string()))?;
        row.quality = quality;
        row.narrator = parsed
            .narrator
            .clone()
            .or(row.narrator)
            .or_else(|| narrator.clone());
        row.bitrate_kbps = parsed.bitrate_kbps.or(row.bitrate_kbps);
        row.release_group.clone_from(&parsed.release_group);
        row.scene_name = d.release_title.clone();
        row.original_path = Some(d.source.to_string_lossy().into_owned());
        row.custom_format_ids = format_ids;
        row.custom_format_score = score;
        row.edition_id = editions::list(&svc.db, book_id)
            .await?
            .into_iter()
            .find(|e| e.monitored)
            .map(|e| e.id);
        let file_id = files::upsert(&svc.db, &row).await?;

        // 7. Calibre (ebooks only; failures only warn).
        if format == BookFormat::Ebook && settings.calibre.enabled {
            let dst = base.join(&relative);
            match crate::calibre::add(&settings.calibre, &dst).await {
                Ok(id) => tracing::info!(file = %dst.display(), calibre_id = ?id, "added to calibre"),
                Err(e) => tracing::warn!(file = %dst.display(), error = %e, "calibredb add failed"),
            }
        }

        svc.emit("book_file", "created", file_id);
        svc.emit("book", "updated", book_id);
        svc.emit("author", "updated", a.id);
        tracing::info!(
            book = book_id,
            format = format.as_str(),
            files = rels.len(),
            quality = quality.quality.name(),
            "book imported"
        );
        for (i, ((src, s), r)) in unit.files.iter().zip(&rels).enumerate() {
            out.imported.push(ImportedBookFile {
                book_id,
                author_id: a.id,
                format,
                file_id,
                source: src.clone(),
                destination: base.join(r),
                size: *s,
                quality,
                release_group: parsed.release_group.clone(),
                is_upgrade: existing.is_some(),
                replaced: if i == 0 {
                    std::mem::take(&mut replaced)
                } else {
                    Vec::new()
                },
            });
        }
        Ok(out)
    }

    /// Folder of the author for a format (for media-server refresh paths).
    pub async fn destination_root(&self, book_id: i64, format: BookFormat) -> Result<PathBuf> {
        let b = self.svc.book_row(book_id).await?;
        let a = self.svc.author_row(b.author_id).await?;
        path_of(&a, format)
            .map(PathBuf::from)
            .ok_or_else(|| BooksError::Validation(format!("{} has no {} folder", a.name, format.as_str())))
    }
}
