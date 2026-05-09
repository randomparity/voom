//! Host function implementations for WASM plugins.
//!
//! Contains the implementations of functions that WASM plugins can call:
//! logging, plugin data resolution, tool execution, and HTTP requests.

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use super::{HostState, HttpResponse, ToolOutput, MAX_PLUGIN_DATA_VALUE_SIZE};

/// Extract the host (domain) from a URL string.
/// Supports `scheme://[user@]host[:port]/...` forms.
fn extract_url_host(url: &str) -> Result<String, String> {
    let after_scheme = url
        .find("://")
        .map(|i| &url[i + 3..])
        .ok_or_else(|| format!("invalid URL '{url}': missing scheme"))?;
    let after_userinfo = after_scheme
        .find('@')
        .map_or(after_scheme, |i| &after_scheme[i + 1..]);
    let authority = after_userinfo
        .find(['/', '?', '#'])
        .map_or(after_userinfo, |i| &after_userinfo[..i]);
    let host = if authority.starts_with('[') {
        authority.find(']').map_or(authority, |i| &authority[..=i])
    } else {
        authority.rfind(':').map_or(authority, |i| &authority[..i])
    };
    if host.is_empty() {
        return Err(format!("URL '{url}' has no host"));
    }
    Ok(host.to_string())
}

/// Check whether a string looks like a filesystem path.
fn looks_like_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with('~')
}

fn host_tool_error(tool: &str, timeout_ms: u64, error: &voom_domain::errors::VoomError) -> String {
    if let voom_domain::errors::VoomError::ToolExecution { message, .. } = error {
        if message.contains("timed out") {
            return format!("tool '{tool}' timed out after {timeout_ms}ms");
        }
    }
    error.to_string()
}

impl HostState {
    /// Log a message at the given level.
    pub fn log(&self, level: &str, message: &str) {
        match level {
            "trace" => tracing::trace!(plugin = %self.plugin_name, "{}", message),
            "debug" => tracing::debug!(plugin = %self.plugin_name, "{}", message),
            "info" => tracing::info!(plugin = %self.plugin_name, "{}", message),
            "warn" => tracing::warn!(plugin = %self.plugin_name, "{}", message),
            "error" => tracing::error!(plugin = %self.plugin_name, "{}", message),
            _ => tracing::info!(plugin = %self.plugin_name, level = %level, "{}", message),
        }
    }

    /// Resolve plugin-specific persisted data by key, using a fallback chain.
    ///
    /// Lookup order:
    /// 1. Persistent storage backend (if attached via `with_storage`)
    /// 2. In-memory `plugin_data` map (seeded by `with_initial_config`)
    ///
    /// This fallback chain means config seeded via `with_initial_config` acts as a
    /// default that the plugin can override by calling `set_plugin_data` (which
    /// writes to persistent storage). Once overridden, the storage value takes
    /// precedence on all subsequent reads.
    pub fn get_plugin_data(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        if let Some(storage) = &self.storage {
            match storage.get(&self.plugin_name, key) {
                Ok(Some(data)) => Ok(Some(data)),
                Ok(None) => Ok(self.plugin_data.get(key).cloned()),
                Err(e) => Err(format!(
                    "failed to read plugin data for plugin '{}' key '{}': {e}",
                    self.plugin_name, key
                )),
            }
        } else {
            Ok(self.plugin_data.get(key).cloned())
        }
    }

    /// Set plugin-specific persisted data.
    pub fn set_plugin_data(&mut self, key: &str, value: &[u8]) -> Result<(), String> {
        if value.len() > MAX_PLUGIN_DATA_VALUE_SIZE {
            return Err(format!(
                "plugin data value exceeds maximum size ({} bytes, max {})",
                value.len(),
                MAX_PLUGIN_DATA_VALUE_SIZE
            ));
        }
        if let Some(storage) = &self.storage {
            storage.set(&self.plugin_name, key, value)
        } else {
            self.plugin_data.insert(key.to_string(), value.to_vec());
            Ok(())
        }
    }

    /// Validate that a path string is within the allowed directories.
    fn check_path_allowed(&self, path_str: &str) -> Result<(), String> {
        let path = Path::new(path_str);
        let canonical = std::fs::canonicalize(path).map_err(|e| {
            format!(
                "cannot resolve path '{}' for plugin '{}': {e}",
                path_str, self.plugin_name
            )
        })?;
        let allowed = self
            .allowed_paths
            .iter()
            .any(|allowed_dir| canonical.starts_with(allowed_dir));
        if !allowed {
            return Err(format!(
                "path '{}' is not within allowed directories for plugin '{}'",
                path_str, self.plugin_name
            ));
        }
        Ok(())
    }

    /// Write content to a file (sandboxed to allowed paths).
    ///
    /// Canonicalizes the parent directory (since the file may not exist
    /// yet) and verifies it is within the allowed paths.
    pub fn write_file(&self, path: &str, content: &[u8]) -> Result<(), String> {
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "path '{}' is not within allowed directories for plugin '{}' \
                 (no paths configured)",
                path, self.plugin_name
            ));
        }

        let file_path = std::path::Path::new(path);

        let parent = file_path.parent().ok_or_else(|| {
            format!(
                "path '{}' has no parent directory for plugin '{}'",
                path, self.plugin_name
            )
        })?;

        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
            format!(
                "cannot resolve parent of '{}' for plugin '{}': {e}",
                path, self.plugin_name
            )
        })?;

        let file_name = file_path
            .file_name()
            .ok_or_else(|| format!("path '{}' has no filename", path))?;
        let canonical_target = canonical_parent.join(file_name);

        let allowed = self
            .allowed_paths
            .iter()
            .any(|allowed_dir| canonical_target.starts_with(allowed_dir));
        if !allowed {
            return Err(format!(
                "path '{}' is not within allowed directories for plugin '{}'",
                path, self.plugin_name
            ));
        }

        std::fs::write(file_path, content).map_err(|e| format!("failed to write '{}': {e}", path))
    }

    /// Run an external tool with the given arguments.
    pub fn run_tool(
        &self,
        tool: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> Result<ToolOutput, String> {
        // Security check: verify tool is allowed (empty allowlist = deny all).
        if self.allowed_tools.is_empty() || !self.allowed_tools.iter().any(|t| t == tool) {
            return Err(format!(
                "tool '{}' is not in the allowed list for plugin '{}'",
                tool, self.plugin_name
            ));
        }

        // Security check: verify path arguments are within allowed directories.
        // Canonicalize paths to prevent traversal attacks (e.g., /allowed/../etc/passwd).
        // Also checks `--flag=/path` patterns by splitting on `=`.
        // When allowed_paths is empty, no paths are permitted (deny all).
        for arg in args {
            if looks_like_path(arg) {
                self.check_path_allowed(arg)?;
            }
            if let Some(eq_pos) = arg.find('=') {
                let value = &arg[eq_pos + 1..];
                if looks_like_path(value) {
                    self.check_path_allowed(value)?;
                }
            }
        }

        // Security check: verify plugin has execute capability (empty = deny all,
        // consistent with allowed_tools).
        if self.allowed_capabilities.is_empty()
            || !self
                .allowed_capabilities
                .iter()
                .any(|c| c.starts_with("execute"))
        {
            return Err(format!(
                "plugin '{}' lacks 'execute' capability required for tool execution",
                self.plugin_name
            ));
        }

        let timeout = Duration::from_millis(timeout_ms);
        let output = voom_process::run_with_timeout_config(
            tool,
            args,
            timeout,
            voom_process::CaptureConfig::default(),
        )
        .map_err(|e| host_tool_error(tool, timeout_ms, &e))?;

        Ok(ToolOutput::new(
            output.status.code().unwrap_or(-1),
            output.stdout,
            output.stderr,
        ))
    }

    /// Query transitions for a file by its UUID.
    /// Returns MessagePack-serialized `Vec<FileTransition>`.
    pub fn get_file_transitions(&self, file_id: &uuid::Uuid) -> Result<Vec<u8>, String> {
        let store = self.transition_store.as_ref().ok_or_else(|| {
            "file transition history not available \
                 (no transition store configured)"
                .to_string()
        })?;
        let transitions = store.transitions_for_file(file_id)?;
        rmp_serde::to_vec(&transitions).map_err(|e| format!("failed to serialize transitions: {e}"))
    }

    /// Query transitions for a file by its filesystem path.
    /// Returns MessagePack-serialized `Vec<FileTransition>`.
    ///
    /// Enforces the same `allowed_paths` sandbox as other filesystem-aware
    /// host functions: requires a non-empty path allowlist and verifies the
    /// query path falls within it.
    pub fn get_path_transitions(&self, path: &str) -> Result<Vec<u8>, String> {
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "path '{path}' is not within allowed directories for plugin '{}' \
                 (no paths configured)",
                self.plugin_name
            ));
        }
        let canonical =
            std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
        let allowed = self.allowed_paths.iter().any(|p| canonical.starts_with(p));
        if !allowed {
            return Err(format!(
                "path '{path}' is not within allowed directories for plugin '{}'",
                self.plugin_name
            ));
        }
        let store = self.transition_store.as_ref().ok_or_else(|| {
            "file transition history not available \
                 (no transition store configured)"
                .to_string()
        })?;
        let path = std::path::Path::new(path);
        let transitions = store.transitions_for_path(path)?;
        rmp_serde::to_vec(&transitions).map_err(|e| format!("failed to serialize transitions: {e}"))
    }

    /// Check that the URL's domain is in the allowed HTTP domains list.
    /// Empty allowlist = deny all (matches `run_tool` semantics).
    fn check_http_domain(&self, url: &str) -> Result<(), String> {
        if self.allowed_http_domains.is_empty() {
            return Err(format!(
                "HTTP access not enabled for plugin '{}' (no allowed domains)",
                self.plugin_name
            ));
        }
        let domain = extract_url_host(url)?;
        let allowed = self.allowed_http_domains.iter().any(|d| d == &domain);
        if !allowed {
            return Err(format!(
                "domain '{domain}' is not in the allowed list for plugin '{}'",
                self.plugin_name
            ));
        }
        Ok(())
    }

    /// Perform an HTTP GET request.
    pub fn http_get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, String> {
        self.check_http_domain(url)?;

        let mut request = ureq::get(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request
            .call()
            .map_err(|e| format!("HTTP GET failed: {e}"))?;

        parse_response(response)
    }

    /// Perform an HTTP POST request.
    pub fn http_post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, String> {
        self.check_http_domain(url)?;

        let mut request = ureq::post(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request
            .send_bytes(body)
            .map_err(|e| format!("HTTP POST failed: {e}"))?;

        parse_response(response)
    }
}

/// Extract status, headers, and body from a ureq response.
fn parse_response(response: ureq::Response) -> Result<HttpResponse, String> {
    let status = response.status();
    let header_names = response.headers_names();
    let headers: Vec<(String, String)> = header_names
        .iter()
        .filter_map(|name| {
            response
                .header(name)
                .map(|val| (name.clone(), val.to_string()))
        })
        .collect();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(10 * 1024 * 1024) // 10 MiB limit
        .read_to_end(&mut body)
        .map_err(|e| format!("failed to read response body: {e}"))?;

    Ok(HttpResponse::with_headers(status, headers, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostState;
    use std::sync::Arc;

    #[test]
    fn test_write_file_allowed_path() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = HostState::new("test".into()).with_paths(vec![canonical_dir.clone()]);
        let file_path = canonical_dir.join("output.srt");
        let result = state.write_file(
            &file_path.to_string_lossy(),
            b"1\n00:00:00,000 --> 00:00:02,500\nHello\n",
        );
        assert!(result.is_ok());
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "1\n00:00:00,000 --> 00:00:02,500\nHello\n"
        );
    }

    #[test]
    fn test_write_file_blocked_path() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = HostState::new("test".into()).with_paths(vec![canonical_dir]);
        let result = state.write_file("/etc/evil.txt", b"bad");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not within allowed"));
    }

    #[test]
    fn test_write_file_no_paths_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let state = HostState::new("test".into());
        let file_path = dir.path().join("output.txt");
        let result = state.write_file(&file_path.to_string_lossy(), b"hello");
        // Empty allowed_paths = deny all (matches allowed_tools semantics)
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not within allowed"));
    }

    #[test]
    fn test_extract_url_host_basic() {
        assert_eq!(
            extract_url_host("http://example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("https://api.example.com:443/v1").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            extract_url_host("http://192.0.2.1:8080").unwrap(),
            "192.0.2.1"
        );
    }

    #[test]
    fn test_extract_url_host_no_scheme() {
        assert!(extract_url_host("example.com/path").is_err());
    }

    #[test]
    fn test_looks_like_path() {
        assert!(looks_like_path("/etc/passwd"));
        assert!(looks_like_path("./relative"));
        assert!(looks_like_path("../parent"));
        assert!(looks_like_path("~/home"));
        assert!(!looks_like_path("just-a-flag"));
        assert!(!looks_like_path("--verbose"));
    }

    #[test]
    fn test_get_file_transitions_no_store() {
        let state = HostState::new("test".into());
        let result = state.get_file_transitions(&uuid::Uuid::new_v4());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[test]
    fn test_get_file_transitions_with_store() {
        use crate::host::InMemoryTransitionStore;
        use std::path::PathBuf;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = Arc::new(InMemoryTransitionStore::new());
        let file_id = uuid::Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "hash123".into(),
            2000,
            TransitionSource::Discovery,
        );
        store.record_transition(&t).unwrap();

        let state = HostState::new("test".into()).with_transition_store(store);

        let bytes = state.get_file_transitions(&file_id).unwrap();
        let transitions: Vec<FileTransition> = rmp_serde::from_slice(&bytes).expect("deserialize");
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to_hash, "hash123");
    }

    #[test]
    fn test_get_path_transitions_no_paths_configured() {
        let state = HostState::new("test".into());
        let result = state.get_path_transitions("/movies/test.mkv");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no paths configured"));
    }

    #[test]
    fn test_get_path_transitions_no_store() {
        use std::path::PathBuf;
        let state = HostState::new("test".into()).with_paths(vec![PathBuf::from("/movies")]);
        let result = state.get_path_transitions("/movies/test.mkv");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[test]
    fn test_get_path_transitions_with_store() {
        use crate::host::InMemoryTransitionStore;
        use std::path::PathBuf;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = Arc::new(InMemoryTransitionStore::new());
        let path = PathBuf::from("/movies/test.mkv");
        let t = FileTransition::new(
            uuid::Uuid::new_v4(),
            path.clone(),
            "hash456".into(),
            3000,
            TransitionSource::Voom,
        );
        store.record_transition(&t).unwrap();

        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![PathBuf::from("/movies")]);

        let bytes = state.get_path_transitions(&path.to_string_lossy()).unwrap();
        let transitions: Vec<FileTransition> = rmp_serde::from_slice(&bytes).expect("deserialize");
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to_hash, "hash456");
    }

    #[test]
    fn test_get_file_transitions_preserves_metadata_snapshot() {
        use crate::host::InMemoryTransitionStore;
        use std::path::PathBuf;
        use voom_domain::media::{Container, MediaFile, Track, TrackType};
        use voom_domain::snapshot::MetadataSnapshot;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = Arc::new(InMemoryTransitionStore::new());
        let file_id = uuid::Uuid::new_v4();

        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(7200.0)
            .with_tracks(vec![
                Track::new(0, TrackType::Video, "hevc".into()),
                Track::new(1, TrackType::AudioMain, "aac".into()),
            ]);
        let snap = MetadataSnapshot::from_media_file(&file);

        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "hash789".into(),
            2_000_000,
            TransitionSource::Voom,
        )
        .with_metadata_snapshot(snap.clone());
        store.record_transition(&t).unwrap();

        let state = HostState::new("test".into()).with_transition_store(store);
        let bytes = state.get_file_transitions(&file_id).unwrap();
        let transitions: Vec<FileTransition> = rmp_serde::from_slice(&bytes).expect("deserialize");
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].metadata_snapshot, Some(snap));
    }

    #[test]
    fn test_get_path_transitions_blocked_by_allowed_paths() {
        use crate::host::InMemoryTransitionStore;

        let store = Arc::new(InMemoryTransitionStore::new());
        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![std::path::PathBuf::from("/movies")]);

        let result = state.get_path_transitions("/etc/passwd");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not within allowed directories"));
    }

    #[test]
    fn test_get_path_transitions_denied_by_empty_paths() {
        use crate::host::InMemoryTransitionStore;

        let store = Arc::new(InMemoryTransitionStore::new());
        // Empty allowed_paths = deny all, matching write_file and run_tool.
        let state = HostState::new("test".into()).with_transition_store(store);

        let result = state.get_path_transitions("/movies/test.mkv");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no paths configured"));
    }

    // --- URL host extraction: security-relevant edge cases ---

    #[test]
    fn test_extract_url_host_strips_userinfo() {
        // user:pass@host must not be treated as the domain.
        assert_eq!(
            extract_url_host("http://user:pass@example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("https://alice@api.example.com/v1").unwrap(),
            "api.example.com"
        );
        // Userinfo with port must still resolve to bare host.
        assert_eq!(
            extract_url_host("http://user:pass@example.com:8080/x").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn test_extract_url_host_bracketed_ipv6() {
        // IPv6 literal in brackets — brackets must be preserved so the
        // port separator inside ::1 isn't mistaken for a port delimiter.
        assert_eq!(extract_url_host("http://[::1]/path").unwrap(), "[::1]");
        assert_eq!(
            extract_url_host("http://[2001:db8::1]:8080/x").unwrap(),
            "[2001:db8::1]"
        );
    }

    #[test]
    fn test_extract_url_host_ignores_query_and_fragment() {
        assert_eq!(
            extract_url_host("http://example.com/p?foo=bar").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("http://example.com/p#frag").unwrap(),
            "example.com"
        );
        // Query appearing directly after authority (no path) also handled.
        assert_eq!(
            extract_url_host("http://example.com?x=1").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("http://example.com#frag").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn test_extract_url_host_no_path() {
        // Whole authority returned when there's no path/query/fragment.
        assert_eq!(
            extract_url_host("http://example.com").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("https://example.com:443").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn test_extract_url_host_empty_host_rejected() {
        // No authority at all.
        let err = extract_url_host("http:///path").unwrap_err();
        assert!(
            err.contains("no host"),
            "expected empty-host rejection: {err}"
        );
        // Only userinfo, no host.
        let err = extract_url_host("http://user@/path").unwrap_err();
        assert!(
            err.contains("no host"),
            "expected empty-host rejection: {err}"
        );
    }

    // --- write_file: structural path rejections ---

    #[test]
    fn test_write_file_no_filename_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = HostState::new("test".into()).with_paths(vec![canonical_dir.clone()]);
        // A path ending in `..` resolves to a parent that exists and canonicalizes
        // successfully, but `file_name()` returns None — this is the structural
        // check inside write_file that we want to exercise.
        let path_str = format!("{}/..", canonical_dir.to_string_lossy());
        let result = state.write_file(&path_str, b"data");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("no filename"),
            "expected filename error, got: {err}"
        );
    }

    #[test]
    fn test_write_file_unresolvable_parent_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = HostState::new("test".into()).with_paths(vec![canonical_dir]);
        // Parent directory does not exist → canonicalize fails.
        let result = state.write_file("/definitely/does/not/exist/file.txt", b"data");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("cannot resolve parent"),
            "expected parent-resolution error, got: {err}"
        );
    }

    // --- Plugin data size limit: boundary + storage enforcement ---

    #[test]
    fn test_set_plugin_data_exact_boundary_accepted() {
        let mut state = HostState::new("test".into());
        let at_limit = vec![0_u8; MAX_PLUGIN_DATA_VALUE_SIZE];
        assert!(state.set_plugin_data("k", &at_limit).is_ok());
    }

    #[test]
    fn test_set_plugin_data_one_over_boundary_rejected() {
        let mut state = HostState::new("test".into());
        let over_limit = vec![0_u8; MAX_PLUGIN_DATA_VALUE_SIZE + 1];
        let err = state.set_plugin_data("k", &over_limit).unwrap_err();
        assert!(
            err.contains("exceeds maximum size"),
            "expected size-limit rejection, got: {err}"
        );
    }

    #[test]
    fn test_set_plugin_data_size_limit_checked_before_storage() {
        // If the size limit were enforced inside the in-memory fallback only,
        // a plugin could bypass it by attaching external storage. Verify that
        // the cap applies regardless of the storage attachment.
        use crate::host::{InMemoryPluginStore, WasmPluginStore};
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        let mut state = HostState::new("test".into()).with_storage(Arc::clone(&store));
        let over_limit = vec![0_u8; MAX_PLUGIN_DATA_VALUE_SIZE + 1];
        let err = state.set_plugin_data("k", &over_limit).unwrap_err();
        assert!(
            err.contains("exceeds maximum size"),
            "size limit must be enforced before reaching storage, got: {err}"
        );
        // Storage must remain empty — the oversized value must not have leaked through.
        assert!(store.get("test", "k").unwrap().is_none());
    }

    // --- HTTP domain gating: allowlist semantics ---

    #[test]
    fn test_check_http_domain_allowed_host_passes() {
        // No network call — check_http_domain is a pure allowlist check.
        let state = HostState::new("test".into()).with_http_domains(vec!["api.example.com".into()]);
        assert!(state
            .check_http_domain("https://api.example.com/v1/resource")
            .is_ok());
    }

    #[test]
    fn test_check_http_domain_host_not_on_allowlist() {
        let state = HostState::new("test".into()).with_http_domains(vec!["api.example.com".into()]);
        let err = state
            .check_http_domain("https://other.example.com/x")
            .unwrap_err();
        assert!(err.contains("not in the allowed list"), "got: {err}");
    }

    #[test]
    fn test_check_http_domain_suffix_injection_rejected() {
        // `example.com.evil.net` must not match an allowlist entry of
        // `example.com`. The matcher compares full host strings.
        let state = HostState::new("test".into()).with_http_domains(vec!["example.com".into()]);
        let err = state
            .check_http_domain("http://example.com.evil.net/path")
            .unwrap_err();
        assert!(
            err.contains("not in the allowed list"),
            "suffix injection must be rejected, got: {err}"
        );
        // And the "prefix" direction too: `evilexample.com` must not match.
        let err = state
            .check_http_domain("http://evilexample.com/")
            .unwrap_err();
        assert!(
            err.contains("not in the allowed list"),
            "prefix injection must be rejected, got: {err}"
        );
    }

    #[test]
    fn test_check_http_domain_invalid_url_rejected() {
        let state = HostState::new("test".into()).with_http_domains(vec!["example.com".into()]);
        // Missing scheme → extract_url_host returns Err, propagated.
        let err = state.check_http_domain("not-a-url").unwrap_err();
        assert!(
            err.contains("missing scheme") || err.contains("invalid URL"),
            "expected URL-parse error, got: {err}"
        );
    }

    #[test]
    fn test_check_http_domain_empty_allowlist_denies() {
        let state = HostState::new("test".into());
        let err = state.check_http_domain("http://example.com").unwrap_err();
        assert!(
            err.contains("no allowed domains"),
            "empty allowlist must deny, got: {err}"
        );
    }

    // --- Plugin data storage routing ---

    #[test]
    fn test_set_plugin_data_writes_to_storage_when_attached() {
        use crate::host::{InMemoryPluginStore, WasmPluginStore};
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        let mut state = HostState::new("routing-plugin".into()).with_storage(Arc::clone(&store));

        state.set_plugin_data("key", b"value").unwrap();

        // Value must land in storage under the plugin's name...
        assert_eq!(
            store.get("routing-plugin", "key").unwrap().as_deref(),
            Some(b"value".as_ref())
        );
        // ...and must NOT be in the in-memory fallback map.
        assert!(!state.plugin_data.contains_key("key"));
    }

    #[test]
    fn test_get_plugin_data_falls_back_to_in_memory() {
        // When storage has no entry for the key, get_plugin_data must fall
        // through to the in-memory map (used for seeded config).
        use crate::host::{InMemoryPluginStore, WasmPluginStore};
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        let mut state = HostState::new("fallback-plugin".into()).with_storage(store);

        // Directly seed the in-memory map (bypassing set_plugin_data so the
        // value doesn't go to storage).
        state
            .plugin_data
            .insert("seeded".into(), b"in-mem".to_vec());

        let got = state.get_plugin_data("seeded").unwrap();
        assert_eq!(got.as_deref(), Some(b"in-mem".as_ref()));
    }

    #[test]
    fn test_get_plugin_data_storage_hit_overrides_in_memory() {
        // When a key is present in both, storage wins.
        use crate::host::{InMemoryPluginStore, WasmPluginStore};
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        store
            .set("override-plugin", "key", b"from-storage")
            .unwrap();

        let mut state = HostState::new("override-plugin".into()).with_storage(store);
        state
            .plugin_data
            .insert("key".into(), b"from-memory".to_vec());

        let got = state.get_plugin_data("key").unwrap();
        assert_eq!(got.as_deref(), Some(b"from-storage".as_ref()));
    }

    #[test]
    fn test_get_plugin_data_storage_error_does_not_fall_back() {
        use crate::host::WasmPluginStore;

        struct FailingStore;

        impl WasmPluginStore for FailingStore {
            fn get(&self, plugin_name: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
                Err(format!("{plugin_name}:{key}: backend unavailable"))
            }

            fn set(&self, _plugin_name: &str, _key: &str, _value: &[u8]) -> Result<(), String> {
                Ok(())
            }

            fn delete(&self, _plugin_name: &str, _key: &str) -> Result<(), String> {
                Ok(())
            }
        }

        let mut state =
            HostState::new("failing-plugin".into()).with_storage(Arc::new(FailingStore));
        state
            .plugin_data
            .insert("config".into(), b"{\"seeded\":true}".to_vec());

        let err = state.get_plugin_data("config").unwrap_err();
        assert!(err.contains("failed to read plugin data"));
        assert!(err.contains("backend unavailable"));
    }
}
