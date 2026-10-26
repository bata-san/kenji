use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Unified API error type. Always serializes to {"error":{"code","message"}}.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.clone()),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid token".into(),
            ),
            ApiError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "insufficient role".into(),
            ),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found", "not found".into()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.clone()),
            ApiError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error".into(),
                )
            }
        };
        (
            status,
            Json(json!({"error": {"code": code, "message": message}})),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Internal(anyhow::Error::new(e))
    }
}

impl From<crate::auth::UserError> for ApiError {
    fn from(e: crate::auth::UserError) -> Self {
        use crate::auth::UserError;
        match e {
            UserError::Invalid(m) => ApiError::BadRequest(m),
            UserError::Conflict(m) => ApiError::Conflict(m),
            UserError::NotFound => ApiError::NotFound,
            UserError::LastAdmin => {
                ApiError::Conflict("最後の管理者は削除または降格できません".into())
            }
            UserError::Other(e) => ApiError::Internal(e),
        }
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        ApiError::Internal(anyhow::Error::new(e))
    }
}

impl From<crate::pty::TerminalError> for ApiError {
    fn from(e: crate::pty::TerminalError) -> Self {
        match e {
            crate::pty::TerminalError::Limit(message) => ApiError::Conflict(message),
            crate::pty::TerminalError::Open(error) => ApiError::Internal(error),
        }
    }
}
