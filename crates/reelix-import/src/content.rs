//! Finished downloads handed from the pipeline to the books and music importers.

use std::path::PathBuf;

use crate::files::TransferMode;

/// One completed download for a book or album import.
#[derive(Debug, Clone)]
pub struct ContentDownload {
    /// Downloaded file or folder (already mapped to a local path).
    pub source: PathBuf,
    pub release_title: Option<String>,
    /// How to bring files into the library (decided by the pipeline).
    pub mode: TransferMode,
    pub download_client: Option<String>,
    pub download_id: Option<String>,
}
