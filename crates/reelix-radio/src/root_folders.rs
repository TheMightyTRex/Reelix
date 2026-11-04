//! Radio root folders (kind `radio` in the shared `root_folders` table): where filed episodes
//! go, i.e. the podcast library folder as Reelix sees it.

use std::collections::HashSet;

use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_core::ContentKind;
use reelix_library::content_roots::{self, ContentRoot};

use crate::error::{RadioError, Result};
use crate::{RadioService, repo};

impl RadioService {
    pub(crate) async fn radio_root(&self, id: i64) -> Result<ContentRoot> {
        match content_roots::get(&self.db, id).await? {
            Some(r) if r.kind == ContentKind::Radio => Ok(r),
            Some(_) => Err(RadioError::Validation(format!(
                "root folder {id} is not a radio root folder"
            ))),
            None => Err(RadioError::Validation(format!("root folder {id} does not exist"))),
        }
    }

    /// Radio root folders with free space. Audiobookshelf owns the folders inside, so none are
    /// listed as unmapped.
    pub async fn root_folders(&self) -> Result<Vec<ContentRootFolderDto>> {
        let mut out = Vec::new();
        for r in content_roots::list(&self.db, &[ContentKind::Radio]).await? {
            let mut dto = content_roots::dto(&r, &HashSet::new()).await;
            dto.unmapped_folders.clear();
            out.push(dto);
        }
        Ok(out)
    }

    pub async fn add_root_folder(&self, req: CreateContentRootFolderRequest) -> Result<ContentRootFolderDto> {
        if req.kind != ContentKind::Radio {
            return Err(RadioError::Validation(
                "radio root folders must have kind `radio`".into(),
            ));
        }
        let id = content_roots::add(&self.db, &req.path, ContentKind::Radio).await?;
        self.emit("rootfolder", "created", id);
        let root = self.radio_root(id).await?;
        let mut dto = content_roots::dto(&root, &HashSet::new()).await;
        dto.unmapped_folders.clear();
        Ok(dto)
    }

    /// Refuses while shows use the folder.
    pub async fn delete_root_folder(&self, id: i64) -> Result<()> {
        self.radio_root(id)
            .await
            .map_err(|_| RadioError::NotFound(format!("radio root folder {id} not found")))?;
        let used = repo::shows::count_using(&self.db, "root_folder_id", id).await?;
        if used > 0 {
            return Err(RadioError::Conflict(format!(
                "root folder {id} is used by {used} show(s)"
            )));
        }
        content_roots::delete(&self.db, id).await?;
        self.emit("rootfolder", "deleted", id);
        Ok(())
    }
}
