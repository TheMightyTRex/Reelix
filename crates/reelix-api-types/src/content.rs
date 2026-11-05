//! Types shared by books, music and radio: content kinds, their root folders and search kinds.

use serde::{Deserialize, Serialize};

use crate::library::UnmappedFolderDto;

pub use reelix_core::{BookFormat, ContentKind};

/// A book, music or radio root folder with free space and unmapped sub-folders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentRootFolderDto {
    pub id: i64,
    pub path: String,
    /// `ebook`, `audiobook`, `music` or `radio`.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: ContentKind,
    pub accessible: bool,
    pub free_space: Option<u64>,
    pub total_space: Option<u64>,
    pub unmapped_folders: Vec<UnmappedFolderDto>,
}

/// `POST /bookrootfolders` / `POST /musicrootfolders`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateContentRootFolderRequest {
    pub path: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: ContentKind,
}

/// Shared wanted/search kinds for books and music.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ContentSearchKind {
    /// Book / album ids.
    Item,
    /// Author / artist ids.
    Parent,
    Missing,
    Cutoff,
}
