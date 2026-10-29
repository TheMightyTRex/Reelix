use reelix_db::DbError;
use reelix_library::LibraryError;

/// Errors from download monitoring, import and the activity APIs.
#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("filesystem: {0}")]
    Files(#[from] reelix_import::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl ActivityError {
    pub fn not_found(what: &str, id: impl std::fmt::Display) -> Self {
        Self::NotFound(format!("{what} {id} not found"))
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
}

impl From<sqlx::Error> for ActivityError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(e))
    }
}

impl From<serde_json::Error> for ActivityError {
    fn from(e: serde_json::Error) -> Self {
        Self::Db(DbError::Json(e))
    }
}

pub type Result<T, E = ActivityError> = std::result::Result<T, E>;
