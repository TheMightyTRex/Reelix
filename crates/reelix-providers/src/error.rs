use reelix_db::DbError;

/// Errors from the provider service.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    NotFound(String),
    /// Bad input (unknown implementation, schema validation, blocked address…).
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    /// A feature is not configured (e.g. no TMDB key).
    #[error("{0}")]
    NotConfigured(String),
    /// An upstream service failed (test connection, Plex PIN…).
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for ProviderError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(e))
    }
}

impl From<serde_json::Error> for ProviderError {
    fn from(e: serde_json::Error) -> Self {
        Self::Db(DbError::Json(e))
    }
}

pub type Result<T> = std::result::Result<T, ProviderError>;
