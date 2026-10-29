//! Music root folders (kind `music` in the shared `root_folders` table).

use std::collections::HashSet;

use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_core::ContentKind;
use reelix_library::content_roots::{self, ContentRoot};

use crate::error::{MusicError, Result};
use crate::{MusicService, repo};

impl MusicService {
    /// Normalised folders of every artist (for the unmapped-folder lists).
    pub(crate) async fn mapped_folders(&self) -> Result<HashSet<String>> {
        Ok(repo::artists::load(&self.db, None)
            .await?
            .into_iter()
            .map(|a| content_roots::normalize(&a.path))
            .collect())
    }

    pub(crate) async fn music_root(&self, id: i64) -> Result<ContentRoot> {
        match content_roots::get(&self.db, id).await? {
            Some(r) if r.kind == ContentKind::Music => Ok(r),
            Some(_) => Err(MusicError::Validation(format!(
                "root folder {id} is not a music root folder"
            ))),
            None => Err(MusicError::Validation(format!("root folder {id} does not exist"))),
        }
    }

    pub async fn root_folders(&self) -> Result<Vec<ContentRootFolderDto>> {
        let mapped = self.mapped_folders().await?;
        let mut out = Vec::new();
        for r in content_roots::list(&self.db, &[ContentKind::Music]).await? {
            out.push(content_roots::dto(&r, &mapped).await);
        }
        Ok(out)
    }

    pub async fn add_root_folder(&self, req: CreateContentRootFolderRequest) -> Result<ContentRootFolderDto> {
        if req.kind != ContentKind::Music {
            return Err(MusicError::Validation(
                "music root folders must have kind `music`".into(),
            ));
        }
        let id = content_roots::add(&self.db, &req.path, ContentKind::Music).await?;
        self.emit("rootfolder", "created", id);
        let root = self.music_root(id).await?;
        Ok(content_roots::dto(&root, &self.mapped_folders().await?).await)
    }

    /// Refuses while artists use the folder.
    pub async fn delete_root_folder(&self, id: i64) -> Result<()> {
        self.music_root(id)
            .await
            .map_err(|_| MusicError::NotFound(format!("music root folder {id} not found")))?;
        let used = repo::artists::count_using(&self.db, "root_folder_id", id).await?;
        if used > 0 {
            return Err(MusicError::Conflict(format!(
                "root folder {id} is used by {used} artist(s)"
            )));
        }
        content_roots::delete(&self.db, id).await?;
        self.emit("rootfolder", "deleted", id);
        Ok(())
    }
}
