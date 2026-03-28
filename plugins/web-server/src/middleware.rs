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

/// Per-request CSP nonce, injected by [`SecurityHeadersLayer`] and extracted
/// by page handlers to stamp inline `<script>` / `<style>` tags.
#[derive(Clone, Debug)]
pub struct CspNonce(pub String);

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

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        let nonce = Uuid::new_v4().simple().to_string();
        req.extensions_mut().insert(CspNonce(nonce.clone()));

        let mut svc = self.inner.clone();
        Box::pin(async move {
            let mut response = svc.call(req).await?;
            let headers = response.headers_mut();

            let csp = format!(
                "default-src 'self'; \
                 script-src 'self' 'nonce-{nonce}'; \
                 style-src 'self' 'nonce-{nonce}'; \
                 img-src 'self' data:; \
                 connect-src 'self'; \
                 frame-ancestors 'none'; \
                 base-uri 'self'"
            );
            if let Ok(val) = HeaderValue::from_str(&csp) {
                headers.insert("Content-Security-Policy", val);
            }

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

    if !state.is_authorized(auth_header) {
        tracing::warn!(
            uri = %request.uri(),
            method = %request.method(),
            "authentication failed: invalid or missing bearer token"
        );
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    next.run(request).await
}
