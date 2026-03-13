//! Error types for the web server.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// API error response.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Web server error type.
#[derive(Debug)]
pub enum WebError {
    NotFound(String),
    BadRequest(String),
    Internal(String),
    Storage(String),
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebError::NotFound(msg) => write!(f, "not found: {msg}"),
            WebError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            WebError::Internal(msg) => write!(f, "internal error: {msg}"),
            WebError::Storage(msg) => write!(f, "storage error: {msg}"),
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, error_msg) = match &self {
            WebError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            WebError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            WebError::Internal(msg) => {
                tracing::error!(error = %msg, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
            WebError::Storage(msg) => {
                tracing::error!(error = %msg, "storage error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database operation failed".to_string(),
                )
            }
        };

        let body = axum::Json(ApiError {
            error: error_msg,
            details: None,
        });

        (status, body).into_response()
    }
}

impl From<voom_domain::errors::VoomError> for WebError {
    fn from(err: voom_domain::errors::VoomError) -> Self {
        match &err {
            voom_domain::errors::VoomError::ToolNotFound { .. } => {
                WebError::NotFound(err.to_string())
            }
            voom_domain::errors::VoomError::Validation(_) => WebError::BadRequest(err.to_string()),
            voom_domain::errors::VoomError::Storage(_) => WebError::Storage(err.to_string()),
            _ => WebError::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::errors::VoomError;

    #[test]
    fn test_voom_error_to_web_error_mapping() {
        let err = VoomError::ToolNotFound {
            tool: "ffprobe".into(),
        };
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::NotFound(_)));

        let err = VoomError::Validation("bad input".into());
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::BadRequest(_)));

        let err = VoomError::Storage("db error".into());
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::Storage(_)));

        let err = VoomError::Plugin {
            plugin: "x".into(),
            message: "y".into(),
        };
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::Internal(_)));
    }
}
