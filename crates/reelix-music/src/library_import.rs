//! Import Library: existing artist folders under a music root → artists + their files.

use std::path::Path;

use reelix_api_types::library::{ImportErrorDto, LibraryImportResult};
use reelix_api_types::music::{AddArtistRequest, MusicImportCandidateDto, MusicLibraryImportRequest};
use reelix_library::content_roots;

use crate::error::{MusicError, Result};
use crate::{MusicService, repo};

/// Folders looked up on MusicBrainz per scan (1 req/s); the UI can search the rest by hand.
const MAX_LOOKUPS: usize = 25;

impl MusicService {
    pub async fn import_scan(&self, root_folder_id: i64) -> Result<Vec<MusicImportCandidateDto>> {
        let root = self.music_root(root_folder_id).await?;
        let dto = content_roots::dto(&root, &self.mapped_folders().await?).await;
        if !dto.accessible {
            return Err(MusicError::Validation(format!("{} is not accessible", root.path)));
        }
        let mut out = Vec::new();
        for (i, f) in dto.unmapped_folders.into_iter().enumerate() {
            let guess = f.name.trim().to_owned();
            let suggestions = if i < MAX_LOOKUPS && !guess.is_empty() {
                match self.lookup_artists(&guess).await {
                    Ok(mut s) => {
                        s.truncate(5);
                        s
                    }
                    Err(e) => {
                        tracing::debug!(folder = %f.path, error = %e, "artist lookup failed");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            out.push(MusicImportCandidateDto {
                folder: f.name,
                path: f.path,
                guessed_artist: Some(guess).filter(|g| !g.is_empty()),
                suggestions,
            });
        }
        Ok(out)
    }

    pub async fn library_import(&self, req: MusicLibraryImportRequest) -> Result<LibraryImportResult> {
        let root = self.music_root(req.root_folder_id).await?;
        let mut result = LibraryImportResult::default();
        for item in req.items {
            let path = content_roots::normalize(&item.path);
            let outcome = if !Path::new(&path).starts_with(&root.path) {
                Err(MusicError::Validation(format!(
                    "{path} is not inside {}",
                    root.path
                )))
            } else if repo::artists::load(&self.db, None)
                .await?
                .iter()
                .any(|a| content_roots::normalize(&a.path) == path)
            {
                Err(MusicError::Conflict(format!(
                    "{path} already belongs to an artist"
                )))
            } else {
                self.add_artist_at(
                    AddArtistRequest {
                        musicbrainz_id: item.musicbrainz_id.clone(),
                        quality_profile_id: item.quality_profile_id,
                        metadata_profile_id: item.metadata_profile_id,
                        root_folder_id: req.root_folder_id,
                        monitored: true,
                        monitor: item.monitor,
                        monitor_new_items: Default::default(),
                        selected_release_group_ids: Vec::new(),
                        tags: item.tags.clone(),
                        search_on_add: false,
                    },
                    Some(path.clone()),
                    None,
                )
                .await
            };
            match outcome {
                Ok(a) => result.added.push(a.id),
                Err(e) => result.errors.push(ImportErrorDto {
                    path,
                    message: e.to_string(),
                }),
            }
        }
        Ok(result)
    }
}
