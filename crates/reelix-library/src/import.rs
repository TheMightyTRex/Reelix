//! Import an existing library: unmapped folders → TMDB suggestions → bulk add.

use std::collections::HashSet;
use std::path::Path;

use futures::stream::{self, StreamExt};

use reelix_api_types::library::{
    AddMovieRequest, AddSeriesRequest, ImportCandidateDto, ImportErrorDto, LibraryImportItem,
    LibraryImportRequest, LibraryImportResult, LookupResult, MovieMonitor, NewItemMonitor,
};
use reelix_core::MediaKind;
use reelix_import::files::{FileKind, ScanOptions, scan_async};
use reelix_metadata::MetaError;

use crate::error::{LibraryError, Result};
use crate::movies::hit_to_lookup;
use crate::repo::{misc, movies, series};
use crate::root_folders::unmapped_folders;
use crate::{LibraryService, content_roots};

/// Items added at once by [`LibraryService::library_import`] (metadata lookups dominate).
const IMPORT_CONCURRENCY: usize = 4;

/// Title + year from a folder name like `The Matrix (1999) {tmdb-603}`.
pub(crate) fn parse_folder_name(name: &str) -> (String, Option<i32>, Option<u32>) {
    let tmdb = name.find("tmdb-").or_else(|| name.find("tmdbid-")).and_then(|i| {
        let rest = &name[i..];
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    });
    // Drop bracketed id blocks before parsing.
    let mut cleaned = String::new();
    let mut depth = 0;
    for c in name.chars() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => cleaned.push(c),
            _ => {}
        }
    }
    let parsed = reelix_parser::parse(cleaned.trim());
    let (fallback_title, fallback_year) = crate::movies::split_year(cleaned.trim());
    let title = if parsed.title.trim().is_empty() {
        fallback_title
    } else {
        parsed.title.clone()
    };
    (title, parsed.year.map(i32::from).or(fallback_year), tmdb)
}

impl LibraryService {
    /// Unmapped folders of a root folder with up to 5 TMDB suggestions each.
    pub async fn import_scan(&self, root_folder_id: i64) -> Result<Vec<ImportCandidateDto>> {
        let root = misc::root_folder(&self.db, root_folder_id)
            .await?
            .ok_or_else(|| LibraryError::not_found("root folder", root_folder_id))?;
        let mapped = self.mapped_paths().await?;
        let folders = unmapped_folders(Path::new(&root.path), &mapped);
        let (movie_idx, (series_idx, _)) = (
            movies::tmdb_index(&self.db).await?,
            series::id_index(&self.db).await?,
        );
        let mut metadata_ok = true;
        let mut out = Vec::with_capacity(folders.len());
        for folder in folders {
            let files = scan_async(
                folder.path.clone().into(),
                ScanOptions {
                    include_non_video: false,
                    max_depth: 3,
                },
            )
            .await
            .unwrap_or_default();
            let videos: Vec<_> = files.iter().filter(|f| f.kind == FileKind::Video).collect();
            let (title, year, tmdb) = parse_folder_name(&folder.name);
            let mut suggestions: Vec<LookupResult> = Vec::new();
            if metadata_ok {
                let res = match (tmdb, root.media_kind) {
                    (Some(id), MediaKind::Movie) => self
                        .metadata
                        .movie(id)
                        .await
                        .map(|m| vec![crate::movies::movie_hit(&m)]),
                    _ => self.metadata.search(&title, root.media_kind, year).await,
                };
                match res {
                    Ok(hits) => {
                        suggestions = hits
                            .iter()
                            .take(5)
                            .map(|h| {
                                let idx = match root.media_kind {
                                    MediaKind::Movie => &movie_idx,
                                    MediaKind::Series => &series_idx,
                                };
                                hit_to_lookup(h, h.tmdb_id.and_then(|t| idx.get(&t).copied()))
                            })
                            .collect();
                    }
                    Err(MetaError::NotConfigured(_)) => metadata_ok = false,
                    Err(e) => tracing::debug!(folder = %folder.name, error = %e, "import lookup failed"),
                }
            }
            out.push(ImportCandidateDto {
                media_kind: root.media_kind,
                parsed_title: title,
                parsed_year: year,
                file_count: videos.len() as u32,
                size: videos.iter().map(|f| f.size).sum(),
                suggestions,
                folder,
            });
        }
        Ok(out)
    }

    /// Adds each folder as a movie/series using its existing path. Adds run a few at a time;
    /// one disk scan covers every added item and, with `search_on_add`, searches for what is
    /// still missing once the scan has linked the files already on disk.
    pub async fn library_import(&self, req: LibraryImportRequest) -> Result<LibraryImportResult> {
        let root = misc::root_folder(&self.db, req.root_folder_id)
            .await?
            .ok_or_else(|| LibraryError::not_found("root folder", req.root_folder_id))?;
        let mut seen = HashSet::new();
        let outcomes: Vec<(String, Result<i64>)> = stream::iter(req.items)
            .map(|item| {
                let first = seen.insert(item.tmdb_id);
                let root = &root;
                async move {
                    let res = if !first {
                        Err(LibraryError::Conflict(format!(
                            "TMDB id {} is selected for more than one folder",
                            item.tmdb_id
                        )))
                    } else if let Err(e) = content_roots::ensure_inside(&item.path, [root.path.as_str()]) {
                        Err(e)
                    } else {
                        self.import_one(root.id, root.media_kind, &item).await
                    };
                    (item.path, res)
                }
            })
            .buffered(IMPORT_CONCURRENCY)
            .collect()
            .await;

        let mut result = LibraryImportResult::default();
        for (path, res) in outcomes {
            match res {
                Ok(id) => result.added.push(id),
                Err(e) => result.errors.push(ImportErrorDto {
                    path,
                    message: e.to_string(),
                }),
            }
        }
        if !result.added.is_empty() {
            let (m, s): (&[i64], &[i64]) = match root.media_kind {
                MediaKind::Movie => (&result.added, &[]),
                MediaKind::Series => (&[], &result.added),
            };
            self.queue_import_scan(m, s, req.search_on_add).await?;
        }
        Ok(result)
    }

    async fn import_one(&self, root_id: i64, kind: MediaKind, item: &LibraryImportItem) -> Result<i64> {
        match kind {
            MediaKind::Movie => self
                .add_movie_inner(
                    AddMovieRequest {
                        tmdb_id: item.tmdb_id,
                        quality_profile_id: item.quality_profile_id,
                        root_folder_id: root_id,
                        monitor: if item.monitored {
                            MovieMonitor::MovieOnly
                        } else {
                            MovieMonitor::None
                        },
                        minimum_availability: item.minimum_availability,
                        tags: item.tags.clone(),
                        // Searched after the scan instead (see `library_import`).
                        search_on_add: false,
                        path: Some(item.path.clone()),
                        added_by_request_id: None,
                    },
                    false,
                )
                .await
                .map(|m| m.id),
            MediaKind::Series => self
                .add_series_inner(
                    AddSeriesRequest {
                        tmdb_id: Some(item.tmdb_id),
                        tvdb_id: None,
                        quality_profile_id: item.quality_profile_id,
                        root_folder_id: root_id,
                        monitor: if item.monitored {
                            item.monitor
                        } else {
                            reelix_api_types::library::SeriesMonitor::None
                        },
                        seasons: None,
                        series_type: item.series_type,
                        season_folder: item.season_folder,
                        monitor_new_items: NewItemMonitor::All,
                        tags: item.tags.clone(),
                        search_on_add: false,
                        search_cutoff_unmet: false,
                        path: Some(item.path.clone()),
                        added_by_request_id: None,
                    },
                    false,
                )
                .await
                .map(|s| s.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names() {
        assert_eq!(
            parse_folder_name("The Matrix (1999) {tmdb-603}"),
            ("The Matrix".into(), Some(1999), Some(603))
        );
        let (t, y, id) = parse_folder_name("Breaking Bad");
        assert_eq!((t.as_str(), y, id), ("Breaking Bad", None, None));
    }
}
