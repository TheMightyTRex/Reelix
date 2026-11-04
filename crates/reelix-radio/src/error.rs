use reelix_db::DbError;
use reelix_library::LibraryError;

use crate::provider::ProviderError;

/// Errors from the radio service.
#[derive(Debug, thiserror::Error)]
pub enum RadioError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T, E = RadioError> = std::result::Result<T, E>;

impl From<sqlx::Error> for RadioError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(e))
    }
}

impl From<LibraryError> for RadioError {
    fn from(e: LibraryError) -> Self {
        match e {
            LibraryError::NotFound(m) => Self::NotFound(m),
            LibraryError::Validation(m) => Self::Validation(m),
            LibraryError::Conflict(m) => Self::Conflict(m),
            LibraryError::Db(d) => Self::Db(d),
            other => Self::Internal(other.to_string()),
        }
    }
}
