//! Error types for the web server.
//!
//! This module contains two distinct error categories:
//!
//! - [`ServerError`] — typed errors returned by [`crate::server::start_server`] and
//!   related startup functions. These are library-level errors suitable for callers
//!   who want to handle individual failure modes programmatically.
//! - [`WebError`] / [`ApiError`] — HTTP-layer errors used inside axum handlers.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Errors that can occur while starting or running the web server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The bind address string is not a valid [`std::net::SocketAddr`].
    #[error("invalid bind address '{address}': {source}")]
    InvalidBindAddress {
        address: String,
        #[source]
        source: std::net::AddrParseError,
    },

    /// The TCP listener could not bind to the requested address/port.
    #[error("failed to bind to {address}: {source}")]
    BindFailed {
        address: String,
        #[source]
        source: std::io::Error,
    },

    /// The HTTP server returned an error while serving requests.
    #[error("server error: {source}")]
    Serve {
        #[source]
        source: std::io::Error,
    },

    /// Template loading or compilation failed.
    #[error("template error: {0}")]
    Template(String),
}

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
    /// A constraint was violated (e.g. duplicate unique key). Maps to HTTP 409.
    Conflict(String),
    Internal(String),
    Storage(String),
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebError::NotFound(msg) => write!(f, "not found: {msg}"),
            WebError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            WebError::Conflict(msg) => write!(f, "conflict: {msg}"),
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
            WebError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
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

/// Run a blocking storage operation on a background thread.
///
/// Wraps `tokio::task::spawn_blocking` with the standard double-map_err pattern
/// used across all web handlers: `JoinError` → Internal, `StorageError` → Storage.
pub async fn spawn_store_op<F, T>(f: F) -> Result<T, WebError>
where
    F: FnOnce() -> Result<T, voom_domain::errors::VoomError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(WebError::from)
}

impl From<voom_domain::errors::VoomError> for WebError {
    fn from(err: voom_domain::errors::VoomError) -> Self {
        use voom_domain::errors::StorageErrorKind;
        match &err {
            voom_domain::errors::VoomError::ToolNotFound { .. } => {
                WebError::NotFound(err.to_string())
            }
            voom_domain::errors::VoomError::Validation(_) => WebError::BadRequest(err.to_string()),
            voom_domain::errors::VoomError::Storage { kind, .. } => match kind {
                StorageErrorKind::ConstraintViolation => WebError::Conflict(err.to_string()),
                StorageErrorKind::NotFound => WebError::NotFound(err.to_string()),
                _ => WebError::Storage(err.to_string()),
            },
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

        let err = VoomError::Storage {
            kind: voom_domain::errors::StorageErrorKind::Other,
            message: "db error".into(),
        };
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::Storage(_)));

        let err = VoomError::Storage {
            kind: voom_domain::errors::StorageErrorKind::ConstraintViolation,
            message: "unique constraint failed".into(),
        };
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::Conflict(_)));

        let err = VoomError::Plugin {
            plugin: "x".into(),
            message: "y".into(),
        };
        let web_err: WebError = err.into();
        assert!(matches!(web_err, WebError::Internal(_)));
    }
}
