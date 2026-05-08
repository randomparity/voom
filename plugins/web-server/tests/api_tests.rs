//! Integration tests for the web server REST API.

use std::sync::Arc;

use axum_test::TestServer;
use serde_json::json;
use uuid::Uuid;

use voom_domain::job::{Job, JobType};
use voom_domain::media::{Container, MediaFile};
use voom_domain::test_support::InMemoryStore;
use voom_domain::verification::{VerificationMode, VerificationOutcome, VerificationRecord};

fn make_test_file(name: &str) -> MediaFile {
    let mut file = MediaFile::new(format!("/media/{name}").into());
    file.container = Container::Mkv;
    file.size = 1_000_000;
    file.content_hash = Some("abc123".into());
    file.duration = 3600.0;
    file
}

fn make_verification(file_id: Uuid, outcome: VerificationOutcome) -> VerificationRecord {
    VerificationRecord::new(
        Uuid::new_v4(),
        file_id.to_string(),
        chrono::Utc::now(),
        VerificationMode::Hash,
        outcome,
        u32::from(outcome == VerificationOutcome::Error),
        0,
        Some("abc123".into()),
        Some("verification details".into()),
    )
}

fn make_verification_at(
    file_id: Uuid,
    outcome: VerificationOutcome,
    verified_at: chrono::DateTime<chrono::Utc>,
) -> VerificationRecord {
    VerificationRecord::new(
        Uuid::new_v4(),
        file_id.to_string(),
        verified_at,
        VerificationMode::Hash,
        outcome,
        u32::from(outcome == VerificationOutcome::Error),
        0,
        Some("abc123".into()),
        Some("verification details".into()),
    )
}

fn make_server(store: InMemoryStore) -> TestServer {
    make_server_with_auth(store, None)
}

fn make_server_with_auth(store: InMemoryStore, auth_token: Option<String>) -> TestServer {
    let store = Arc::new(store);
    let templates = voom_web_server::server::embedded_templates().unwrap();
    let state =
        voom_web_server::state::AppState::new_with_default_sse(store, templates, auth_token, None);
    let router = voom_web_server::router::build_router(state);
    TestServer::new(router).unwrap()
}

const VALID_POLICY: &str = r#"policy "test" {
  phase clean {
    keep audio where codec in [aac, opus]
  }
}"#;

// === File API Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_list_files_empty() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/files").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["files"], json!([]));
    assert_eq!(body["total"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_files_with_data() {
    let file = make_test_file("movie.mkv");
    let store = InMemoryStore::new().with_file(file);
    let server = make_server(store);

    let resp = server.get("/api/files").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["total"], 1);
    assert_eq!(body["files"][0]["container"], "Mkv");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_file_by_id() {
    let file = make_test_file("movie.mkv");
    let id = file.id;
    let store = InMemoryStore::new().with_file(file);
    let server = make_server(store);

    let resp = server.get(&format!("/api/files/{id}")).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"], id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_file_not_found() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get(&format!("/api/files/{}", Uuid::new_v4())).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_delete_file() {
    let file = make_test_file("movie.mkv");
    let id = file.id;
    let store = InMemoryStore::new().with_file(file);
    let server = make_server(store);

    // DELETE marks the file as missing (soft-delete)
    let resp = server.delete(&format!("/api/files/{id}")).await;
    resp.assert_status_ok();

    // File record still exists (soft-deleted, status = missing)
    let resp = server.get(&format!("/api/files/{id}")).await;
    resp.assert_status_ok();
}

// === Job API Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_list_jobs_empty() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/jobs").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["jobs"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_jobs_with_data() {
    let job = Job::new(JobType::Transcode);
    let store = InMemoryStore::new().with_job(job);
    let server = make_server(store);

    let resp = server.get("/api/jobs").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(body["jobs"][0]["job_type"], "transcode");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_job_by_id() {
    let job = Job::new(JobType::Scan);
    let id = job.id;
    let store = InMemoryStore::new().with_job(job);
    let server = make_server(store);

    let resp = server.get(&format!("/api/jobs/{id}")).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"], id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_job_not_found() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get(&format!("/api/jobs/{}", Uuid::new_v4())).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_job_stats() {
    let job1 = Job::new(JobType::Scan);
    let mut job2 = Job::new(JobType::Transcode);
    job2.status = voom_domain::job::JobStatus::Completed;
    let store = InMemoryStore::new().with_job(job1).with_job(job2);
    let server = make_server(store);

    let resp = server.get("/api/jobs/stats").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert!(!body["counts"].as_array().unwrap().is_empty());
}

// === Plugin API Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_list_plugins_empty_by_default() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/plugins").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let plugins = body["plugins"].as_array().unwrap();
    // No plugins registered in test state (populated from kernel at startup)
    assert_eq!(plugins.len(), 0);
    assert_eq!(body["total"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_plugins_with_data() {
    let store = Arc::new(InMemoryStore::new());
    let templates = voom_web_server::server::embedded_templates().unwrap();
    let plugin_info = vec![voom_web_server::api::plugins::PluginInfoResponse::new(
        "test-plugin".into(),
        "0.1.0".into(),
        "A test plugin".into(),
        String::new(),
        String::new(),
        String::new(),
        vec!["test".into()],
    )];
    let state =
        voom_web_server::state::AppState::new_with_default_sse(store, templates, None, None)
            .with_plugin_info(plugin_info);
    let router = voom_web_server::router::build_router(state);
    let server = TestServer::new(router).unwrap();

    let resp = server.get("/api/plugins").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let plugins = body["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["name"], "test-plugin");
    assert_eq!(body["total"], 1);
}

// === Stats API Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_get_stats() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/stats").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["total_files"], 0);
}

// === Policy API Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_validate_valid_policy() {
    let server = make_server(InMemoryStore::new());
    let resp = server
        .post("/api/policy/validate")
        .json(&json!({ "source": VALID_POLICY }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["valid"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_validate_invalid_policy() {
    let server = make_server(InMemoryStore::new());
    let resp = server
        .post("/api/policy/validate")
        .json(&json!({ "source": "this is not valid DSL" }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["valid"], false);
    assert!(!body["errors"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_format_policy() {
    let server = make_server(InMemoryStore::new());
    let resp = server
        .post("/api/policy/format")
        .json(&json!({ "source": VALID_POLICY }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert!(body["formatted"].as_str().unwrap().contains("policy"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_format_invalid_policy() {
    let server = make_server(InMemoryStore::new());
    let resp = server
        .post("/api/policy/format")
        .json(&json!({ "source": "not valid" }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert!(body.get("formatted").is_none());
    assert!(!body["errors"].as_array().unwrap().is_empty());
}

// === Page Tests (HTML) ===

#[tokio::test(flavor = "multi_thread")]
async fn test_dashboard_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/").await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(body.contains("VOOM"));
    assert!(body.contains("Library Integrity"));
    assert!(body.contains("/integrity"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_library_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/library").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_jobs_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/jobs").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_policies_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/policies").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_plugins_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/plugins").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_settings_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/settings").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_file_detail_page_shows_verification_history() {
    let file = make_test_file("movie.mkv");
    let id = file.id;
    let record = make_verification(id, VerificationOutcome::Ok);
    let store = InMemoryStore::new()
        .with_file(file)
        .with_verification(record);
    let server = make_server(store);

    let resp = server.get(&format!("/files/{id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(body.contains("Verification History"));
    assert!(body.contains("verification details"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_integrity_page_lists_latest_error_files() {
    let healthy = make_test_file("healthy.mkv");
    let failing = make_test_file("failing.mkv");
    let failing_id = failing.id;
    let store = InMemoryStore::new()
        .with_file(healthy.clone())
        .with_file(failing.clone())
        .with_verification(make_verification(healthy.id, VerificationOutcome::Ok))
        .with_verification(make_verification(failing_id, VerificationOutcome::Error));
    let server = make_server(store);

    let resp = server.get("/integrity").await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(body.contains("Integrity"));
    assert!(body.contains("failing.mkv"));
    assert!(!body.contains("healthy.mkv"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_integrity_page_requires_auth_when_configured() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    let resp = server.get("/integrity").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// === Security Header Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_security_headers() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/files").await;
    let headers = resp.headers();
    assert!(headers.get("content-security-policy").is_some());
    let csp = headers
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("base-uri 'self'"));
    assert!(!csp.contains("unsafe-eval"));
    assert!(
        !csp.contains("unsafe-inline"),
        "CSP should use nonces, not unsafe-inline"
    );
    // Nonce-based CSP: each response gets a unique nonce for inline scripts/styles
    assert!(csp.contains("style-src 'self' 'nonce-"));
    assert!(csp.contains("script-src 'self' 'nonce-"));
    // No external CDN references — JS is bundled locally
    assert!(
        !csp.contains("unpkg.com"),
        "CSP should not reference external CDN"
    );
    assert!(headers.get("x-content-type-options").is_some());
    assert!(headers.get("x-frame-options").is_some());
    assert!(headers.get("referrer-policy").is_some());
}

// === Auth Middleware Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_returns_401_with_wrong_token() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    let resp = server
        .get("/api/files")
        .add_header(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer wrong-token"),
        )
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_returns_401_without_token() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    let resp = server.get("/api/files").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_returns_200_with_correct_token() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    let resp = server
        .get("/api/files")
        .add_header(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer secret-token"),
        )
        .await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_passthrough_when_no_token_configured() {
    let server = make_server_with_auth(InMemoryStore::new(), None);
    let resp = server.get("/api/files").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_page_routes_accessible_without_auth() {
    // Without auth configured, pages are public
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/").await;
    resp.assert_status_ok();
    let resp = server.get("/library").await;
    resp.assert_status_ok();
    let resp = server.get("/jobs").await;
    resp.assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_page_routes_require_auth_when_configured() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    // Page routes are protected when auth is configured
    let resp = server.get("/").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let resp = server.get("/library").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let resp = server.get("/jobs").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// === Fallback 404 Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_unknown_route_returns_json_404() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/nonexistent").await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "Not found");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_unknown_route_requires_auth_when_configured() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    // Unknown paths must hit auth before the fallback so unauthenticated
    // probes get 401 instead of leaking the 404 distinction.
    let resp = server.get("/api/nonexistent").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let resp = server.get("/totally/unknown/path").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// === File Filter Validation Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_list_files_excessive_limit_is_clamped() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/files?limit=999999").await;
    resp.assert_status_ok();
    // Should succeed — limit is clamped, not rejected
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_files_excessive_offset_is_clamped() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/files?offset=99999999").await;
    resp.assert_status_ok();
}

// === Policy Size Limit Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_validate_oversized_policy_returns_error() {
    let server = make_server(InMemoryStore::new());
    let oversized = "x".repeat(1_024 * 1_024 + 1);
    let resp = server
        .post("/api/policy/validate")
        .json(&json!({ "source": oversized }))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_format_oversized_policy_returns_error() {
    let server = make_server(InMemoryStore::new());
    let oversized = "x".repeat(1_024 * 1_024 + 1);
    let resp = server
        .post("/api/policy/format")
        .json(&json!({ "source": oversized }))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

// === SSE Client Limit Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_sse_client_limit_enforced() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(InMemoryStore::new());
    let templates = voom_web_server::server::embedded_templates().unwrap();
    let state =
        voom_web_server::state::AppState::new_with_default_sse(store, templates, None, None);
    // Simulate 64 clients already connected
    state.sse_client_count.store(64, Ordering::Relaxed);
    let router = voom_web_server::router::build_router(state);
    let server = TestServer::new(router).unwrap();

    let resp = server.get("/events").await;
    resp.assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_id_header_present() {
    let store = Arc::new(InMemoryStore::new());
    let templates = voom_web_server::server::embedded_templates().unwrap();
    let state =
        voom_web_server::state::AppState::new_with_default_sse(store, templates, None, None);
    let router = voom_web_server::router::build_router(state);
    let server = TestServer::new(router).unwrap();

    let resp = server.get("/api/stats").await;
    resp.assert_status_ok();
    let request_id = resp
        .headers()
        .get("x-request-id")
        .expect("x-request-id header should be present");
    let id_str = request_id.to_str().unwrap();
    assert!(
        Uuid::parse_str(id_str).is_ok(),
        "x-request-id should be a valid UUID"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_id_unique_per_request() {
    let store = Arc::new(InMemoryStore::new());
    let templates = voom_web_server::server::embedded_templates().unwrap();
    let state =
        voom_web_server::state::AppState::new_with_default_sse(store, templates, None, None);
    let router = voom_web_server::router::build_router(state);
    let server = TestServer::new(router).unwrap();

    let resp1 = server.get("/api/stats").await;
    let resp2 = server.get("/api/stats").await;

    let id1 = resp1
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let id2 = resp2
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(id1, id2, "each request should get a unique request ID");
}

// === Static Asset Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_static_htmx_returns_js() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/static/htmx-2.0.4.min.js").await;
    resp.assert_status_ok();
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(ct, "application/javascript");
    let cache = resp
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cache.contains("immutable"));
    let body = resp.text();
    assert!(body.contains("htmx"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_static_alpine_returns_js() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/static/alpine-3.14.8.min.js").await;
    resp.assert_status_ok();
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(ct, "application/javascript");
    let body = resp.text();
    assert!(!body.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_static_assets_require_auth_when_configured() {
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret".into()));
    let resp = server.get("/static/htmx-2.0.4.min.js").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// === Rate Limit Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_rate_limit_cpu_intensive_returns_429() {
    let server = make_server(InMemoryStore::new());
    let policy_body = json!({ "source": VALID_POLICY });

    // Send 10 requests (within limit)
    for _ in 0..10 {
        let resp = server.post("/api/policy/validate").json(&policy_body).await;
        resp.assert_status_ok();
    }

    // 11th request should be rate-limited
    let resp = server.post("/api/policy/validate").json(&policy_body).await;
    resp.assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_rate_limit_general_api_is_generous() {
    let server = make_server(InMemoryStore::new());

    // 20 requests should all succeed (limit is 120/min)
    for _ in 0..20 {
        let resp = server.get("/api/files").await;
        resp.assert_status_ok();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_rate_limit_429_response_format() {
    let server = make_server(InMemoryStore::new());
    let policy_body = json!({ "source": VALID_POLICY });

    // Exhaust the CPU-intensive limit
    for _ in 0..10 {
        server.post("/api/policy/validate").json(&policy_body).await;
    }

    let resp = server.post("/api/policy/validate").json(&policy_body).await;
    resp.assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);

    // Verify JSON body format
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "Too many requests");
    assert!(body["details"].as_str().unwrap().contains("Retry after"));

    // Verify Retry-After header is present
    let retry_after = resp
        .headers()
        .get("Retry-After")
        .expect("Retry-After header should be present");
    let secs: u64 = retry_after.to_str().unwrap().parse().unwrap();
    assert!(secs > 0, "Retry-After should be positive");
}

// === Transition API Tests ===

use voom_domain::transition::{FileTransition, TransitionSource};

#[tokio::test(flavor = "multi_thread")]
async fn test_list_transitions_empty() {
    let file = make_test_file("movie.mkv");
    let id = file.id;
    let store = InMemoryStore::new().with_file(file);
    let server = make_server(store);

    let resp = server.get(&format!("/api/files/{id}/transitions")).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["transitions"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_transitions_with_data() {
    let file = make_test_file("movie.mkv");
    let id = file.id;
    let t = FileTransition::new(
        id,
        "/media/movie.mkv".into(),
        "hash1".into(),
        1_000_000,
        TransitionSource::Discovery,
    );
    let store = InMemoryStore::new().with_file(file).with_transition(t);
    let server = make_server(store);

    let resp = server.get(&format!("/api/files/{id}/transitions")).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["transitions"].as_array().unwrap().len(), 1);
    assert_eq!(body["transitions"][0]["source"], "discovery");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_transitions_filters_by_file_id() {
    let file_a = make_test_file("a.mkv");
    let file_b = make_test_file("b.mkv");
    let id_a = file_a.id;
    let id_b = file_b.id;
    let t_a = FileTransition::new(
        id_a,
        "/media/a.mkv".into(),
        "ha".into(),
        100,
        TransitionSource::Discovery,
    );
    let t_b = FileTransition::new(
        id_b,
        "/media/b.mkv".into(),
        "hb".into(),
        200,
        TransitionSource::External,
    );
    let store = InMemoryStore::new()
        .with_file(file_a)
        .with_file(file_b)
        .with_transition(t_a)
        .with_transition(t_b);
    let server = make_server(store);

    let resp = server.get(&format!("/api/files/{id_a}/transitions")).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let ts = body["transitions"].as_array().unwrap();
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0]["source"], "discovery");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_transitions_file_not_found() {
    let server = make_server(InMemoryStore::new());
    let id = Uuid::new_v4();
    let resp = server.get(&format!("/api/files/{id}/transitions")).await;
    resp.assert_status_not_found();
}

// === Verify API Tests ===

#[tokio::test(flavor = "multi_thread")]
async fn test_list_verifications_empty() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/verify").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_verifications_filter_validation() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/verify?mode=bogus").await;
    resp.assert_status_bad_request();

    let resp = server.get("/api/verify?outcome=bogus").await;
    resp.assert_status_bad_request();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_verifications_accepts_valid_filters() {
    let server = make_server(InMemoryStore::new());
    let resp = server
        .get("/api/verify?mode=hash&outcome=ok&limit=25")
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_verifications_filters_by_relative_since() {
    let recent_id = Uuid::new_v4();
    let old_id = Uuid::new_v4();
    let store = InMemoryStore::new()
        .with_verification(make_verification_at(
            recent_id,
            VerificationOutcome::Ok,
            chrono::Utc::now() - chrono::Duration::days(2),
        ))
        .with_verification(make_verification_at(
            old_id,
            VerificationOutcome::Ok,
            chrono::Utc::now() - chrono::Duration::days(10),
        ));
    let server = make_server(store);

    let resp = server.get("/api/verify?since=7d").await;

    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let records = body.as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["file_id"], recent_id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_verifications_rejects_invalid_since() {
    let server = make_server(InMemoryStore::new());

    let resp = server.get("/api/verify?since=garbage").await;

    resp.assert_status_bad_request();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_file_verifications_empty() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get(&format!("/api/verify/{}", Uuid::new_v4())).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_integrity_summary_returns_json() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/integrity-summary").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    // InMemoryStore returns a default IntegritySummary -- all zeros.
    assert_eq!(body["total_files"], 0);
    assert_eq!(body["never_verified"], 0);
    assert_eq!(body["stale"], 0);
    assert_eq!(body["with_errors"], 0);
    assert_eq!(body["with_warnings"], 0);
    assert_eq!(body["hash_mismatches"], 0);
}
