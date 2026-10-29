//! Book root folders (kinds `ebook` and `audiobook`) via [`reelix_library::content_roots`].

use std::collections::HashSet;

use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_core::ContentKind;
use reelix_library::content_roots;

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::repo::authors;

const KINDS: [ContentKind; 2] = [ContentKind::Ebook, ContentKind::Audiobook];

impl BooksService {
    /// Normalised author folders of both formats (to compute unmapped folders).
    pub(crate) async fn mapped_folders(&self) -> Result<HashSet<String>> {
        Ok(authors::load(&self.db, None)
            .await?
            .into_iter()
            .flat_map(|a| [a.path, a.audio_path])
            .flatten()
            .map(|p| content_roots::normalize(&p))
            .collect())
    }

    pub async fn root_folders(&self) -> Result<Vec<ContentRootFolderDto>> {
        let mapped = self.mapped_folders().await?;
        let mut out = Vec::new();
        for r in content_roots::list(&self.db, &KINDS).await? {
            out.push(content_roots::dto(&r, &mapped).await);
        }
        Ok(out)
    }

    pub async fn add_root_folder(&self, req: CreateContentRootFolderRequest) -> Result<ContentRootFolderDto> {
        if !KINDS.contains(&req.kind) {
            return Err(BooksError::Validation(
                "book root folders are of kind ebook or audiobook".into(),
            ));
        }
        let id = content_roots::add(&self.db, &req.path, req.kind).await?;
        let root = content_roots::get(&self.db, id)
            .await?
            .ok_or_else(|| BooksError::Internal("root folder vanished".into()))?;
        self.emit("book_root_folder", "created", id);
        Ok(content_roots::dto(&root, &self.mapped_folders().await?).await)
    }

    /// Refuses while authors use the folder.
    pub async fn delete_root_folder(&self, id: i64) -> Result<()> {
        match content_roots::get(&self.db, id).await? {
            Some(r) if KINDS.contains(&r.kind) => {}
            _ => return Err(BooksError::NotFound(format!("book root folder {id} not found"))),
        }
        let used = authors::count_using_root(&self.db, id).await?;
        if used > 0 {
            return Err(BooksError::Conflict(format!(
                "root folder {id} is used by {used} author(s)"
            )));
        }
        content_roots::delete(&self.db, id).await?;
        self.emit("book_root_folder", "deleted", id);
        Ok(())
    }
}
