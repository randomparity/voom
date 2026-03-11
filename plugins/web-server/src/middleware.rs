//! Tower middleware for the web server.

use axum::http::{HeaderValue, Request, Response};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{Layer, Service};

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

            headers.insert(
                "Content-Security-Policy",
                HeaderValue::from_static(
                    "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' https://unpkg.com; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'",
                ),
            );
            headers.insert(
                "X-Content-Type-Options",
                HeaderValue::from_static("nosniff"),
            );
            headers.insert(
                "X-Frame-Options",
                HeaderValue::from_static("DENY"),
            );
            headers.insert(
                "Referrer-Policy",
                HeaderValue::from_static("strict-origin-when-cross-origin"),
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
    pub fn new(token: Option<String>) -> Self {
        Self { token }
    }

    /// Check if the given token is valid. Returns true if no auth is configured.
    pub fn validate(&self, provided: Option<&str>) -> bool {
        match &self.token {
            None => true, // No auth configured
            Some(expected) => provided == Some(expected.as_str()),
        }
    }
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
