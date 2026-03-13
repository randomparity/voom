//! Tower middleware for the web server.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::extract::State;
use axum::http::{HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use uuid::Uuid;

use crate::state::AppState;

/// Layer that adds security headers (CSP, X-Frame-Options, etc.) to all responses.
#[derive(Clone)]
pub struct SecurityHeadersLayer;

impl<S> Layer<S> for SecurityHeadersLayer {
    type Service = SecurityHeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityHeadersService { inner }
    }
}

#[derive(Clone)]
pub struct SecurityHeadersService<S> {
    inner: S,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for SecurityHeadersService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let mut svc = self.inner.clone();
        Box::pin(async move {
            let mut response = svc.call(req).await?;
            let headers = response.headers_mut();

            // TODO: Replace 'unsafe-inline' with nonce-based CSP once templates support it
            headers.insert(
                "Content-Security-Policy",
                HeaderValue::from_static(
                    "default-src 'self'; script-src 'self' https://unpkg.com/htmx.org@2.0.4/dist/htmx.min.js https://unpkg.com/alpinejs@3.14.8/dist/cdn.min.js; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'",
                ),
            );
            headers.insert(
                "X-Content-Type-Options",
                HeaderValue::from_static("nosniff"),
            );
            headers.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
            headers.insert(
                "Referrer-Policy",
                HeaderValue::from_static("strict-origin-when-cross-origin"),
            );

            Ok(response)
        })
    }
}

/// Layer that generates a UUID v4 request ID and adds it as an `X-Request-Id` response header.
#[derive(Clone)]
pub struct RequestIdLayer;

impl<S> Layer<S> for RequestIdLayer {
    type Service = RequestIdService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestIdService { inner }
    }
}

#[derive(Clone)]
pub struct RequestIdService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for RequestIdService<S>
where
    S: Service<Request<B>, Response = Response<axum::body::Body>> + Clone + Send + 'static,
    S::Future: Send,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let mut response = inner.call(req).await?;
            let request_id = Uuid::new_v4().to_string();
            response.headers_mut().insert(
                axum::http::HeaderName::from_static("x-request-id"),
                HeaderValue::from_str(&request_id).expect("UUID is valid header value"),
            );
            Ok(response)
        })
    }
}

/// Configuration for optional token-based auth.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub token: Option<String>,
}

impl AuthConfig {
    #[must_use]
    pub fn new(token: Option<String>) -> Self {
        Self { token }
    }

    /// Check if the given token is valid. Returns true if no auth is configured.
    /// Uses constant-time comparison to prevent timing side-channel attacks.
    #[must_use]
    pub fn validate(&self, provided: Option<&str>) -> bool {
        use subtle::ConstantTimeEq;
        match &self.token {
            None => true, // No auth configured
            Some(expected) => {
                let provided_str = provided.unwrap_or("");
                provided.is_some()
                    && provided_str.len() == expected.len()
                    && bool::from(provided_str.as_bytes().ct_eq(expected.as_bytes()))
            }
        }
    }
}

/// Axum middleware that enforces Bearer token authentication on API routes.
/// If `AppState` has no `auth_token` configured, all requests pass through.
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());

    if !state.validate_auth(auth_header) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_config_no_token() {
        let config = AuthConfig::new(None);
        assert!(config.validate(None));
        assert!(config.validate(Some("anything")));
    }

    #[test]
    fn test_auth_config_with_token() {
        let config = AuthConfig::new(Some("secret-token".into()));
        assert!(!config.validate(None));
        assert!(!config.validate(Some("wrong")));
        assert!(config.validate(Some("secret-token")));
    }
}
