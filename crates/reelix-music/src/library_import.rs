//! Import Library: existing artist folders under a music root → artists + their files.

use std::path::Path;

use reelix_api_types::library::{ImportErrorDto, LibraryImportResult, UnmappedFolderDto};
use reelix_api_types::music::{AddArtistRequest, MusicImportCandidateDto, MusicLibraryImportRequest};
use reelix_library::content_roots;
use reelix_metadata::MetaError;

use crate::error::{MusicError, Result};
use crate::{MusicService, repo};

/// Folders looked up on MusicBrainz per `lookup = true` scan (1 req/s); the UI matches the rest
/// in batches with [`MusicService::import_match`].
const MAX_LOOKUPS: usize = 25;
/// Folders per [`MusicService::import_match`] request (MusicBrainz allows one lookup a second).
pub const MAX_MATCH_BATCH: usize = 25;

impl MusicService {
    /// Unmapped folders of a music root folder with up to 5 MusicBrainz suggestions each (only
    /// the first [`MAX_LOOKUPS`] folders). With `lookup = false` only the folders are listed
    /// (fast, no MusicBrainz calls), and suggestions are fetched afterwards in small batches with
    /// [`Self::import_match`], so every folder gets matched without one long request.
    pub async fn import_scan(
        &self,
        root_folder_id: i64,
        lookup: bool,
    ) -> Result<Vec<MusicImportCandidateDto>> {
        let root = self.music_root(root_folder_id).await?;
        let dto = content_roots::dto(&root, &self.mapped_folders().await?).await;
        if !dto.accessible {
            return Err(MusicError::Validation(format!("{} is not accessible", root.path)));
        }
        let max_lookups = if lookup { MAX_LOOKUPS } else { 0 };
        Ok(self.import_candidates(dto.unmapped_folders, max_lookups).await)
    }

    /// MusicBrainz suggestions for some folders of a music root folder (at most
    /// [`MAX_MATCH_BATCH`]).
    pub async fn import_match(
        &self,
        root_folder_id: i64,
        paths: Vec<String>,
    ) -> Result<Vec<MusicImportCandidateDto>> {
        if paths.len() > MAX_MATCH_BATCH {
            return Err(MusicError::Validation(format!(
                "match at most {MAX_MATCH_BATCH} folders per request"
            )));
        }
        let root = self.music_root(root_folder_id).await?;
        let mut folders = Vec::with_capacity(paths.len());
        for path in paths {
            content_roots::ensure_inside(&path, [root.path.as_str()])?;
            let name = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            folders.push(UnmappedFolderDto { name, path });
        }
        Ok(self.import_candidates(folders, usize::MAX).await)
    }

    /// Candidates for `folders`, looking up the first `max_lookups` one after another: MusicBrainz
    /// allows one request a second, so concurrent lookups would only queue on its rate limiter.
    async fn import_candidates(
        &self,
        folders: Vec<UnmappedFolderDto>,
        max_lookups: usize,
    ) -> Vec<MusicImportCandidateDto> {
        let mut out = Vec::with_capacity(folders.len());
        // Set once the provider is known to be unusable; later folders report it without a call.
        let mut not_configured = None::<String>;
        for (i, f) in folders.into_iter().enumerate() {
            let guess = f.name.trim().to_owned();
            let mut suggestions = Vec::new();
            let mut lookup_error = not_configured.clone();
            if i < max_lookups && !guess.is_empty() && not_configured.is_none() {
                match self.lookup_artists(&guess).await {
                    Ok(mut s) => {
                        s.truncate(5);
                        suggestions = s;
                    }
                    Err(MusicError::Metadata(MetaError::NotConfigured(what))) => {
                        let msg = format!("{what} isn't set up, so folders can't be matched");
                        tracing::warn!("music library import: {msg}");
                        not_configured = Some(msg.clone());
                        lookup_error = Some(msg);
                    }
                    Err(e) => {
                        tracing::warn!(folder = %f.path, error = %e, "artist lookup failed");
                        lookup_error = Some(format!("MusicBrainz lookup failed: {e}"));
                    }
                }
            }
            out.push(MusicImportCandidateDto {
                folder: f.name,
                path: f.path,
                guessed_artist: Some(guess).filter(|g| !g.is_empty()),
                suggestions,
                lookup_error,
            });
        }
        out
    }

    pub async fn library_import(&self, req: MusicLibraryImportRequest) -> Result<LibraryImportResult> {
        let root = self.music_root(req.root_folder_id).await?;
        let mut result = LibraryImportResult::default();
        for item in req.items {
            let path = content_roots::normalize(&item.path);
            let outcome = if let Err(e) = content_roots::ensure_inside(&path, [root.path.as_str()]) {
                Err(e.into())
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
