use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use reelix_api_types::ApiErrorBody;
use reelix_db::DbError;

/// Every error an API handler can return; rendered as [`ApiErrorBody`] JSON.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("authentication required")]
    Unauthorized,
    #[error("invalid username or password")]
    InvalidCredentials,
    #[error("you do not have permission to do that")]
    Forbidden,
    #[error("missing {} header", reelix_api_types::CSRF_HEADER)]
    Csrf,
    #[error("{0}")]
    NotFound(String),
    #[error("{message}")]
    Validation { status: StatusCode, message: String },
    #[error("{0}")]
    Conflict(String),
    #[error("too many failed attempts, try again in {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("internal error: {0}")]
    Internal(String),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal(cause.to_string())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized | Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::Csrf => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Validation { status, .. } => *status,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized | Self::InvalidCredentials => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Csrf => "csrf",
            Self::NotFound(_) => "not_found",
            Self::Validation { .. } => "validation",
            Self::Conflict(_) => "conflict",
            Self::RateLimited { .. } => "rate_limited",
            Self::Internal(_) => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match &self {
            Self::Internal(cause) => {
                tracing::error!(%cause, "internal API error");
                "An internal error occurred; see the server logs.".to_owned()
            }
            other => other.to_string(),
        };
        let body = ApiErrorBody {
            error: self.code().to_owned(),
            message,
        };
        let mut res = (self.status(), Json(body)).into_response();
        if let Self::RateLimited { retry_after_secs } = self {
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
        }
        res
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::Conflict(msg) => Self::Conflict(msg),
            other => Self::internal(other),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        Self::Validation {
            status: r.status(),
            message: r.body_text(),
        }
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        Self::Validation {
            status: StatusCode::BAD_REQUEST,
            message: r.body_text(),
        }
    }
}

impl From<PathRejection> for ApiError {
    fn from(r: PathRejection) -> Self {
        Self::Validation {
            status: StatusCode::BAD_REQUEST,
            message: r.body_text(),
        }
    }
}

/// `axum::Json` whose rejection is rendered as [`ApiErrorBody`].
#[derive(Debug, Clone, Copy, Default, axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

impl<T: serde::Serialize> IntoResponse for ApiJson<T> {
    fn into_response(self) -> Response {
        Json(self.0).into_response()
    }
}

/// `axum::extract::Query` whose rejection is rendered as [`ApiErrorBody`].
#[derive(Debug, Clone, Copy, Default, axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct ApiQuery<T>(pub T);

/// `axum::extract::Path` whose rejection is rendered as [`ApiErrorBody`].
#[derive(Debug, Clone, Copy, Default, axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct ApiPath<T>(pub T);
