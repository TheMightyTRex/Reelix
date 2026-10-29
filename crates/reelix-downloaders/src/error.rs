//! Error type shared by every download client adapter.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    /// Could not reach the client (DNS, refused, TLS, timeout…).
    #[error("connection failed: {0}")]
    Connection(String),
    /// Credentials or API key rejected.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The client answered with an HTTP status we did not expect.
    #[error("unexpected HTTP status {status}: {body}")]
    Http { status: u16, body: String },
    /// The client returned an application-level error.
    #[error("client error: {0}")]
    Api(String),
    /// The client refused the download (duplicate, invalid NZB, …).
    #[error("rejected: {0}")]
    Rejected(String),
    /// The response could not be decoded.
    #[error("invalid response: {0}")]
    Parse(String),
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    /// The release payload is unusable (empty NZB, bad torrent, wrong protocol…).
    #[error("invalid release: {0}")]
    InvalidPayload(String),
    #[error("unsupported version {found} (minimum {minimum})")]
    UnsupportedVersion { found: String, minimum: String },
    #[error("download {0} not found")]
    NotFound(String),
    /// Operation not supported by this client.
    #[error("not supported by this client: {0}")]
    Unsupported(&'static str),
    #[error("unknown implementation `{0}`")]
    UnknownImplementation(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_decode() {
            return Self::Parse(e.to_string());
        }
        if let Some(status) = e.status() {
            return Self::Http {
                status: status.as_u16(),
                body: e.to_string(),
            };
        }
        Self::Connection(e.to_string())
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e.to_string())
    }
}
