use reelix_db::DbError;
use reelix_library::LibraryError;
use reelix_metadata::MetaError;

/// Errors from the books service.
#[derive(Debug, thiserror::Error)]
pub enum BooksError {
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
    #[error("library: {0}")]
    Library(String),
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T, E = BooksError> = std::result::Result<T, E>;

impl From<sqlx::Error> for BooksError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(e))
    }
}

impl From<LibraryError> for BooksError {
    fn from(e: LibraryError) -> Self {
        match e {
            LibraryError::NotFound(m) => Self::NotFound(m),
            LibraryError::Validation(m) => Self::Validation(m),
            LibraryError::Conflict(m) => Self::Conflict(m),
            LibraryError::Metadata(m) => Self::Metadata(m),
            LibraryError::Db(d) => Self::Db(d),
            LibraryError::Files(f) => Self::Files(f),
            LibraryError::Io(io) => Self::Io(io),
            LibraryError::Internal(m) => Self::Library(m),
        }
    }
}
