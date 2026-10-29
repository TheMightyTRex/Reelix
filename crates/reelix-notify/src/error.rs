use thiserror::Error;

/// Errors returned by notifiers.
#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("unknown notifier implementation: {0}")]
    UnknownImplementation(String),
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The service answered with a non-success status.
    #[error("service returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("authentication failed: {0}")]
    Unauthorized(String),
    /// Rate limited; retry after the given number of seconds when known.
    #[error("rate limited (retry after {retry_after_secs:?}s)")]
    RateLimited { retry_after_secs: Option<u64> },
    /// The service accepted the request but reported an application-level error.
    #[error("service error: {0}")]
    Api(String),
    #[error("email error: {0}")]
    Email(String),
    #[error("script error: {0}")]
    Script(String),
}

impl NotifyError {
    pub(crate) fn settings(msg: impl Into<String>) -> Self {
        Self::InvalidSettings(msg.into())
    }
}
