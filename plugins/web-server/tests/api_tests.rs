//! Integration tests for the web server REST API.

use std::sync::Arc;

use axum_test::TestServer;
use serde_json::json;
use uuid::Uuid;

use voom_domain::job::Job;
use voom_domain::media::{Container, MediaFile};

// Inline test store since test_helpers is pub(crate)
mod test_store {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;
    use uuid::Uuid;
    use voom_domain::errors::{Result, VoomError};
    use voom_domain::job::{Job, JobStatus, JobUpdate};
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;
    use voom_domain::stats::ProcessingStats;
    use voom_domain::storage::{FileFilters, StorageTrait, StoredPlan};

    pub struct InMemoryStore {
        files: Mutex<HashMap<Uuid, MediaFile>>,
        jobs: Mutex<HashMap<Uuid, Job>>,
    }

    impl InMemoryStore {
        pub fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
            }
        }

        pub fn with_file(self, file: MediaFile) -> Self {
            self.files.lock().unwrap().insert(file.id, file);
            self
        }

        pub fn with_job(self, job: Job) -> Self {
            self.jobs.lock().unwrap().insert(job.id, job);
            self
        }
    }

    impl StorageTrait for InMemoryStore {
        fn upsert_file(&self, file: &MediaFile) -> Result<()> {
            self.files.lock().unwrap().insert(file.id, file.clone());
            Ok(())
        }
        fn get_file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
            Ok(self.files.lock().unwrap().get(id).cloned())
        }
        fn get_file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .find(|f| f.path == path)
                .cloned())
        }
        fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
            let files = self.files.lock().unwrap();
            let mut result: Vec<MediaFile> = files
                .values()
                .filter(|f| {
                    if let Some(ref prefix) = filters.path_prefix {
                        if !f.path.to_string_lossy().starts_with(prefix.as_str()) {
                            return false;
                        }
                    }
                    true
                })
                .cloned()
                .collect();
            result.sort_by(|a, b| a.path.cmp(&b.path));
            if let Some(offset) = filters.offset {
                result = result.into_iter().skip(offset as usize).collect();
            }
            if let Some(limit) = filters.limit {
                result.truncate(limit as usize);
            }
            Ok(result)
        }
        fn delete_file(&self, id: &Uuid) -> Result<()> {
            self.files.lock().unwrap().remove(id);
            Ok(())
        }
        fn create_job(&self, job: &Job) -> Result<Uuid> {
            self.jobs.lock().unwrap().insert(job.id, job.clone());
            Ok(job.id)
        }
        fn get_job(&self, id: &Uuid) -> Result<Option<Job>> {
            Ok(self.jobs.lock().unwrap().get(id).cloned())
        }
        fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()> {
            let mut jobs = self.jobs.lock().unwrap();
            let job = jobs
                .get_mut(id)
                .ok_or_else(|| VoomError::Storage(format!("job {id} not found")))?;
            if let Some(status) = update.status {
                job.status = status;
            }
            Ok(())
        }
        fn claim_next_job(&self, _worker_id: &str) -> Result<Option<Job>> {
            Ok(None)
        }
        fn list_jobs(&self, status: Option<JobStatus>, limit: Option<u32>) -> Result<Vec<Job>> {
            let jobs = self.jobs.lock().unwrap();
            let mut result: Vec<Job> = jobs
                .values()
                .filter(|j| status.map_or(true, |s| j.status == s))
                .cloned()
                .collect();
            result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            if let Some(limit) = limit {
                result.truncate(limit as usize);
            }
            Ok(result)
        }
        fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>> {
            let jobs = self.jobs.lock().unwrap();
            let mut counts: HashMap<JobStatus, u64> = HashMap::new();
            for job in jobs.values() {
                *counts.entry(job.status).or_insert(0) += 1;
            }
            Ok(counts.into_iter().collect())
        }
        fn save_plan(&self, _plan: &Plan) -> Result<Uuid> {
            Ok(Uuid::new_v4())
        }
        fn get_plans_for_file(&self, _file_id: &Uuid) -> Result<Vec<StoredPlan>> {
            Ok(Vec::new())
        }
        fn update_plan_status(&self, _plan_id: &Uuid, _status: &str) -> Result<()> {
            Ok(())
        }
        fn get_file_history(
            &self,
            _path: &Path,
        ) -> Result<Vec<voom_domain::storage::FileHistoryEntry>> {
            Ok(vec![])
        }
        fn record_stats(&self, _stats: &ProcessingStats) -> Result<()> {
            Ok(())
        }
        fn get_plugin_data(&self, _plugin: &str, _key: &str) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn set_plugin_data(&self, _plugin: &str, _key: &str, _value: &[u8]) -> Result<()> {
            Ok(())
        }
        fn vacuum(&self) -> Result<()> {
            Ok(())
        }
        fn prune_missing_files(&self) -> Result<u64> {
            Ok(0)
        }
    }
}

use test_store::InMemoryStore;

fn make_test_file(name: &str) -> MediaFile {
    let mut file = MediaFile::new(format!("/media/{name}").into());
    file.container = Container::Mkv;
    file.size = 1_000_000;
    file.content_hash = "abc123".into();
    file.duration = 3600.0;
    file
}

fn make_server(store: InMemoryStore) -> TestServer {
    make_server_with_auth(store, None)
}

fn make_server_with_auth(store: InMemoryStore, auth_token: Option<String>) -> TestServer {
    let store = Arc::new(store);
    let templates = voom_web_server::server::embedded_templates_for_test();
    let state = voom_web_server::state::AppState::new(store, templates, auth_token);
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

    let resp = server.delete(&format!("/api/files/{id}")).await;
    resp.assert_status_ok();

    // Verify it's gone
    let resp = server.get(&format!("/api/files/{id}")).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
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
    let job = Job::new("transcode".into());
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
    let job = Job::new("scan".into());
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
    let job1 = Job::new("scan".into());
    let mut job2 = Job::new("transcode".into());
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
async fn test_list_plugins() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/api/plugins").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let plugins = body["plugins"].as_array().unwrap();
    assert!(plugins.len() >= 10);
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
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

// === Page Tests (HTML) ===

#[tokio::test(flavor = "multi_thread")]
async fn test_dashboard_page() {
    let server = make_server(InMemoryStore::new());
    let resp = server.get("/").await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(body.contains("VOOM"));
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
    // unsafe-inline should only be in style-src, not in script-src
    assert!(csp.contains("style-src 'self' 'unsafe-inline'"));
    assert!(csp.contains("script-src 'self' https://unpkg.com/htmx.org@"));
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
    let server = make_server_with_auth(InMemoryStore::new(), Some("secret-token".into()));
    // Page routes should be public even when auth is configured
    let resp = server.get("/").await;
    resp.assert_status_ok();
    let resp = server.get("/library").await;
    resp.assert_status_ok();
    let resp = server.get("/jobs").await;
    resp.assert_status_ok();
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
    let templates = voom_web_server::server::embedded_templates_for_test();
    let state = voom_web_server::state::AppState::new(store, templates, None);
    // Simulate 64 clients already connected
    state.sse_client_count.store(64, Ordering::Relaxed);
    let router = voom_web_server::router::build_router(state);
    let server = TestServer::new(router).unwrap();

    let resp = server.get("/events").await;
    resp.assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
}
