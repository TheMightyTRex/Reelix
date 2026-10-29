use thiserror::Error;

/// Errors returned by media server adapters.
#[derive(Debug, Error)]
pub enum MsError {
    #[error("unknown media server implementation: {0}")]
    UnknownImplementation(String),
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The API key/token was rejected.
    #[error("unauthorized: check the API key or token")]
    Unauthorized,
    /// A user login failed (wrong username or password).
    #[error("invalid username or password")]
    InvalidCredentials,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("server returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unexpected response: {0}")]
    Parse(String),
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}

/// Maps non-success responses to [`MsError`].
pub(crate) async fn check(resp: reqwest::Response) -> Result<reqwest::Response, MsError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    match status.as_u16() {
        401 | 403 => Err(MsError::Unauthorized),
        404 => Err(MsError::NotFound(resp.url().path().to_string())),
        s => {
            let mut body = resp.text().await.unwrap_or_default();
            if body.len() > 500 {
                let mut cut = 500;
                while !body.is_char_boundary(cut) {
                    cut -= 1;
                }
                body.truncate(cut);
            }
            Err(MsError::Status { status: s, body })
        }
    }
}

/// Deserializes a JSON body, reporting shape mismatches as [`MsError::Parse`].
pub(crate) async fn json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, MsError> {
    let bytes = check(resp).await?.bytes().await?;
    serde_json::from_slice(&bytes).map_err(|e| MsError::Parse(e.to_string()))
}
