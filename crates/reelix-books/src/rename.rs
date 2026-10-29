//! Rename preview/execution of book files with the book naming templates.

use std::path::{Path, PathBuf};

use reelix_api_types::books::{AuthorDto, BookDto, BookNamingDto};
use reelix_api_types::library::RenamePreviewDto;
use reelix_core::MediaKind;
use reelix_db::NewJob;
use reelix_import::files::{TransferMode, transfer};
use reelix_import::naming::NamingConfig;

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::kinds;
use crate::naming::{BookNaming, destination};
use crate::repo::books::BookFilter;
use crate::repo::files::{self, FileRow};
use crate::service::path_of;

/// One planned rename of a book file record: `(old relative, new relative)` per file plus
/// the new record paths.
struct Plan {
    row: FileRow,
    moves: Vec<(String, String)>,
    new_relative: String,
    new_parts: Vec<String>,
}

fn plan(
    n: &BookNamingDto,
    a: &AuthorDto,
    b: &BookDto,
    row: &FileRow,
    base: &Path,
    config: &NamingConfig,
) -> Option<Plan> {
    let mut naming = BookNaming::new(a, b);
    naming.quality = Some(row.quality.quality);
    naming.narrator.clone_from(&row.narrator);
    let olds = row.file_paths();
    let sources: Vec<PathBuf> = olds.iter().map(|r| base.join(r)).collect();
    let refs: Vec<&Path> = sources.iter().map(PathBuf::as_path).collect();
    let (rel, parts) = destination(n, &naming, row.format, &refs, config);
    let news: Vec<String> = if parts.is_empty() {
        vec![rel.clone()]
    } else {
        parts.clone()
    };
    let moves: Vec<(String, String)> = olds.into_iter().zip(news).filter(|(o, n)| o != n).collect();
    (!moves.is_empty() || rel != row.relative_path).then(|| Plan {
        row: row.clone(),
        moves,
        new_relative: rel,
        new_parts: parts,
    })
}

impl BooksService {
    async fn rename_plans(&self, author_id: i64) -> Result<(AuthorDto, Vec<Plan>)> {
        let a = self.author_row(author_id).await?;
        let settings = self.settings().await?;
        let config = self.naming_config().await?;
        let books = self
            .load_books(BookFilter {
                ids: None,
                author_ids: Some(&[author_id]),
            })
            .await?;
        let ids: Vec<i64> = books.iter().map(|b| b.id).collect();
        let mut out = Vec::new();
        for row in files::list(&self.db, Some(&ids)).await? {
            let (Some(b), Some(base)) = (
                books.iter().find(|b| b.id == row.book_id),
                path_of(&a, row.format),
            ) else {
                continue;
            };
            if let Some(p) = plan(&settings.naming, &a, b, &row, Path::new(base), &config) {
                out.push(p);
            }
        }
        Ok((a, out))
    }

    /// Files of an author whose names differ from the templates (one entry per file;
    /// `kind` is always `movie`, `media_id` is the book id and the paths are relative to the
    /// author's folder of the file's format).
    pub async fn rename_preview(&self, author_id: i64) -> Result<Vec<RenamePreviewDto>> {
        let (_, plans) = self.rename_plans(author_id).await?;
        Ok(plans
            .iter()
            .flat_map(|p| {
                p.moves.iter().map(|(o, n)| RenamePreviewDto {
                    kind: MediaKind::Movie,
                    media_id: p.row.book_id,
                    file_id: p.row.id,
                    season_number: None,
                    episode_numbers: Vec::new(),
                    existing_path: o.clone(),
                    new_path: n.clone(),
                })
            })
            .collect())
    }

    pub async fn queue_rename(&self, author_id: i64) -> Result<Option<i64>> {
        self.author_row(author_id).await?;
        self.enqueue(
            NewJob::new(
                kinds::BOOK_RENAME,
                serde_json::json!({ "author_ids": [author_id] }),
            )
            .dedupe(format!("{}:{author_id}", kinds::BOOK_RENAME)),
        )
        .await
    }

    /// Renames every file of the author that differs from the templates; returns the number
    /// of files moved.
    pub async fn execute_rename(&self, author_id: i64) -> Result<usize> {
        let (a, plans) = self.rename_plans(author_id).await?;
        let guard = self.path_guard().await?;
        let mut moved = 0;
        for p in plans {
            let Some(base) = path_of(&a, p.row.format).map(PathBuf::from) else {
                continue;
            };
            let mut ok = true;
            for (old, new) in &p.moves {
                let (from, to) = (base.join(old), base.join(new));
                if !from.exists() {
                    ok = false;
                    continue;
                }
                let res = guard
                    .check_existing(&from)
                    .and_then(|_| guard.check(&to))
                    .map_err(BooksError::from)
                    .and_then(|_| {
                        if to.exists() {
                            Err(BooksError::Conflict(format!("{} already exists", to.display())))
                        } else {
                            transfer(&from, &to, TransferMode::Move).map_err(BooksError::from)
                        }
                    });
                match res {
                    Ok(_) => {
                        moved += 1;
                        remove_empty_dirs(from.parent(), &base);
                    }
                    Err(e) => {
                        ok = false;
                        tracing::warn!(from = %from.display(), to = %to.display(), error = %e, "rename failed");
                    }
                }
            }
            if ok {
                let mut row = p.row.clone();
                row.relative_path = p.new_relative.clone();
                for (part, new) in row.parts.iter_mut().zip(&p.new_parts) {
                    part.relative_path.clone_from(new);
                }
                files::upsert(&self.db, &row).await?;
                self.emit("book_file", "updated", row.id);
            }
        }
        if moved > 0 {
            self.emit("author", "updated", author_id);
        }
        Ok(moved)
    }
}

/// Removes empty folders from `dir` up to (not including) `stop`.
pub(crate) fn remove_empty_dirs(mut dir: Option<&Path>, stop: &Path) {
    while let Some(d) = dir {
        if d == stop || !d.starts_with(stop) {
            break;
        }
        if std::fs::read_dir(d).map_or(true, |mut e| e.next().is_some()) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_empty_dirs_up_to_base() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.path().join("a/keep.txt"), "x").unwrap();
        remove_empty_dirs(Some(&deep), dir.path());
        assert!(!dir.path().join("a/b").exists());
        assert!(dir.path().join("a").exists());
    }
}
