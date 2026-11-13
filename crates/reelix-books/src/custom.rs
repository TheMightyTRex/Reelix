//! Authors added with the owner's own details (RFX-90): no Open Library match, a typed name,
//! overview and picture, and a folder that already exists in an ebook or audiobook root
//! folder. The author's `openlibrary_id` is `custom:{id}` and `custom = 1` (migration 0020):
//! refreshes leave it alone, and each scan makes a book for every unit on disk no book matches
//! ([`BooksService::custom_books_from_files`]), named after its sub-folder or file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use reelix_api_types::books::{AuthorDto, AuthorStatisticsDto, NewBookMonitor};
use reelix_api_types::custom::{CustomDetails, CustomDetailsChange};
use reelix_api_types::library::ImagesDto;
use reelix_core::{BookFormat, ContentKind};
use reelix_library::content_roots;
use reelix_library::custom::{custom_folder, folder_owner, folder_taken};
use reelix_parser::{QualityFamily, parse_book};

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::metadata::{blank_book, sort_name};
use crate::repo::books::BookFilter;
use crate::repo::{authors as repo, books as book_repo, clean};
use crate::scan::{Unit, find_units, match_name};
use crate::service::path_of;

/// The book title a unit gives: the first sub-folder of the author folder it lies in, else the
/// file name, cleaned as a release name (`Title (2004) [epub]` → `Title`).
pub(crate) fn unit_title(u: &Unit, base: &Path) -> String {
    let rel = u.path.strip_prefix(base).unwrap_or(&u.path);
    let mut parts = rel.components();
    let first = parts.next().map(|c| c.as_os_str().to_string_lossy().into_owned());
    let name = match (first, parts.next()) {
        // Inside a sub-folder (or a multi-part folder directly in the author folder).
        (Some(dir), Some(_)) => dir,
        (Some(dir), None) if u.is_multi() => dir,
        _ => u
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let parsed = parse_book(&name, None).title;
    let t = if parsed.trim().is_empty() { name } else { parsed };
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl BooksService {
    /// Adds an author with typed details from an existing folder of an ebook or audiobook root
    /// folder (the root's kind is the format); queues a disk scan, which makes the books.
    pub async fn add_custom_author(
        &self,
        d: &CustomDetails,
        root_folder_id: i64,
        path: &str,
        quality_profile_id: Option<i64>,
        monitored: bool,
    ) -> Result<AuthorDto> {
        let root = content_roots::get(&self.db, root_folder_id)
            .await?
            .ok_or_else(|| BooksError::Validation(format!("root folder {root_folder_id} does not exist")))?;
        let format = match root.kind {
            ContentKind::Ebook => BookFormat::Ebook,
            ContentKind::Audiobook => BookFormat::Audiobook,
            _ => {
                return Err(BooksError::Validation(format!(
                    "root folder {} is not a book root folder",
                    root.path
                )));
            }
        };
        let folder = custom_folder(&root.path, path)?;
        let authors = repo::load(&self.db, None).await?;
        let taken = authors.iter().flat_map(|a| {
            [a.path.as_deref(), a.audio_path.as_deref()]
                .into_iter()
                .flatten()
                .map(|p| (a.id, a.name.as_str(), p))
        });
        if let Some((id, name)) = folder_owner(&folder, taken) {
            return Err(folder_taken(&folder, "author", id, &name).into());
        }
        let profile = match quality_profile_id {
            Some(id) => {
                self.check_profile(id, format).await?;
                id
            }
            None => self
                .db
                .quality_profiles()
                .list_family(QualityFamily::of_book(format))
                .await?
                .first()
                .map(|p| p.id)
                .ok_or_else(|| {
                    BooksError::Validation(format!("there is no {} quality profile", format.as_str()))
                })?,
        };
        let mut a = AuthorDto {
            id: 0,
            // Unique until the row has an id; then `custom:{id}`.
            openlibrary_id: format!(
                "custom:new:{}",
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            name: d.title.clone(),
            sort_name: sort_name(&d.title),
            overview: d.overview.clone(),
            birth_date: None,
            death_date: None,
            images: ImagesDto::default(),
            links: Vec::new(),
            monitored,
            monitor_new_items: NewBookMonitor::None,
            quality_profile_id: None,
            root_folder_id: None,
            path: None,
            audio_quality_profile_id: None,
            audio_root_folder_id: None,
            audio_path: None,
            tags: Vec::new(),
            statistics: AuthorStatisticsDto::default(),
            added: Utc::now(),
            last_refreshed: Some(Utc::now()),
            custom: true,
        };
        match format {
            BookFormat::Ebook => {
                a.quality_profile_id = Some(profile);
                a.root_folder_id = Some(root.id);
                a.path = Some(folder);
            }
            BookFormat::Audiobook => {
                a.audio_quality_profile_id = Some(profile);
                a.audio_root_folder_id = Some(root.id);
                a.audio_path = Some(folder);
            }
        }
        let id = repo::insert(&self.db, &a, None).await?;
        sqlx::query("UPDATE authors SET openlibrary_id = ? WHERE id = ?")
            .bind(format!("custom:{id}"))
            .bind(id)
            .execute(self.db.pool())
            .await?;
        self.emit("author", "created", id);
        tracing::info!(id, name = %a.name, "custom author added");
        self.queue_scan(&[id], None).await?;
        self.author_row(id).await
    }

    async fn custom_author(&self, id: i64) -> Result<AuthorDto> {
        let a = self.author_row(id).await?;
        if !a.custom {
            return Err(BooksError::Validation(format!(
                "author {id} has an Open Library match; only authors added with their own details can be changed here"
            )));
        }
        Ok(a)
    }

    /// Changes the typed name and overview of a custom author (authors have no year).
    pub async fn change_custom_author(&self, id: i64, c: &CustomDetailsChange) -> Result<AuthorDto> {
        let mut a = self.custom_author(id).await?;
        if let Some(t) = &c.title {
            a.name = t.clone();
            a.sort_name = sort_name(t);
        }
        if let Some(o) = &c.overview {
            a.overview = o.clone();
        }
        repo::save(&self.db, &a).await?;
        self.emit("author", "updated", id);
        self.author_row(id).await
    }

    /// Sets (or removes) the picture of a custom author.
    pub async fn set_custom_author_picture(&self, id: i64, poster: Option<String>) -> Result<()> {
        let mut a = self.custom_author(id).await?;
        a.images.poster = poster;
        repo::save(&self.db, &a).await?;
        self.emit("author", "updated", id);
        Ok(())
    }

    /// Books of a custom author from the units on disk: every unit no book matches becomes a
    /// book (`custom:{author}:{title}`), named by [`unit_title`]. Not monitored.
    pub(crate) async fn custom_books_from_files(&self, a: &AuthorDto) -> Result<u32> {
        let mut books = self
            .load_books(BookFilter {
                ids: None,
                author_ids: Some(&[a.id]),
            })
            .await?;
        let mut added = 0;
        for format in BookFormat::ALL {
            let Some(base) = path_of(a, format).map(PathBuf::from) else {
                continue;
            };
            if !base.is_dir() {
                continue;
            }
            let dir = base.clone();
            let units = tokio::task::spawn_blocking(move || find_units(&dir, format))
                .await
                .unwrap_or_default();
            let mut seen: HashSet<String> = books.iter().map(|b| clean(&b.title)).collect();
            for u in units {
                let title = unit_title(&u, &base);
                if title.is_empty() || match_name(&title, &a.name, &books).is_some() {
                    continue;
                }
                let key = clean(&title);
                if key.is_empty() || !seen.insert(key.clone()) {
                    continue;
                }
                let mut b = blank_book(a);
                b.openlibrary_work_id = format!("custom:{}:{key}", a.id);
                b.title = title;
                b.sort_title = reelix_metadata::sort_title(&b.title);
                if book_repo::id_by_work(&self.db, &b.openlibrary_work_id)
                    .await?
                    .is_some()
                {
                    continue;
                }
                b.id = book_repo::insert(&self.db, &b).await?;
                self.emit("book", "created", b.id);
                books.push(b);
                added += 1;
            }
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(path: &str, files: &[&str]) -> Unit {
        Unit {
            format: BookFormat::Audiobook,
            path: PathBuf::from(path),
            files: files.iter().map(|f| (PathBuf::from(f), 1)).collect(),
        }
    }

    #[test]
    fn titles_from_folders_and_files() {
        let base = Path::new("/books/Jo Bloggs");
        assert_eq!(
            unit_title(
                &unit(
                    "/books/Jo Bloggs/Garden Diary.m4b",
                    &["/books/Jo Bloggs/Garden Diary.m4b"]
                ),
                base
            ),
            "Garden Diary"
        );
        assert_eq!(
            unit_title(
                &unit(
                    "/books/Jo Bloggs/Lectures",
                    &[
                        "/books/Jo Bloggs/Lectures/01.mp3",
                        "/books/Jo Bloggs/Lectures/02.mp3"
                    ]
                ),
                base
            ),
            "Lectures"
        );
        assert_eq!(
            unit_title(
                &unit(
                    "/books/Jo Bloggs/Notes/notes.epub",
                    &["/books/Jo Bloggs/Notes/notes.epub"]
                ),
                base
            ),
            "Notes"
        );
    }
}
