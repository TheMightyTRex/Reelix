use reelix_db::DbError;
use reelix_metadata::MetaError;

/// Errors from library operations.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    #[error("metadata: {0}")]
    Metadata(#[from] MetaError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("filesystem: {0}")]
    Files(#[from] reelix_import::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T, E = LibraryError> = std::result::Result<T, E>;

impl From<sqlx::Error> for LibraryError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(e))
    }
}

impl From<serde_json::Error> for LibraryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Db(DbError::Json(e))
    }
}

impl LibraryError {
    pub fn not_found(what: &str, id: impl std::fmt::Display) -> Self {
        Self::NotFound(format!("{what} {id} not found"))
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
}
