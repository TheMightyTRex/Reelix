use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Settings(String),
    #[error("{0}")]
    NotFound(String),
    #[error("subtitles are off; activate them in the subtitle settings")]
    Off,
    #[error("{0}")]
    Source(String),
    #[error("{0} asked Reelix to slow down")]
    RateLimited(String),
    #[error("the OpenSubtitles download allowance is used up for today")]
    QuotaExhausted,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Db(#[from] reelix_db::DbError),
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
