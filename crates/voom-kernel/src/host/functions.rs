//! Host function implementations for WASM plugins.
//!
//! Contains the implementations of functions that WASM plugins can call:
//! logging, plugin data resolution, tool execution, and HTTP requests.

use std::io::Read as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use wait_timeout::ChildExt;

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
    #[must_use]
    pub fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>> {
        if let Some(storage) = &self.storage {
            // Check persistent storage first, fall back to in-memory (seeded config).
            match storage.get(&self.plugin_name, key) {
                Ok(Some(data)) => Some(data),
                Ok(None) => self.plugin_data.get(key).cloned(),
                Err(e) => {
                    tracing::warn!(
                        plugin = %self.plugin_name,
                        key = %key,
                        error = %e,
                        "storage error in get_plugin_data, falling back to in-memory"
                    );
                    self.plugin_data.get(key).cloned()
                }
            }
        } else {
            self.plugin_data.get(key).cloned()
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
        let file_path = std::path::Path::new(path);

        let parent = file_path.parent().ok_or_else(|| {
            format!(
                "path '{}' has no parent directory for plugin '{}'",
                path, self.plugin_name
            )
        })?;

        if !self.allowed_paths.is_empty() {
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
        if !self.allowed_paths.is_empty() {
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

        let mut child = Command::new(tool)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn tool '{tool}': {e}"))?;

        // Read stdout/stderr on separate threads BEFORE waiting, to avoid
        // deadlock when pipe buffers fill up.
        let stdout_handle = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut out, &mut buf).map(|_| buf)
            })
        });
        let stderr_handle = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut err, &mut buf).map(|_| buf)
            })
        });

        let timeout = Duration::from_millis(timeout_ms);
        match child.wait_timeout(timeout) {
            Ok(Some(status)) => {
                let stdout = stdout_handle
                    .map(|h| h.join().unwrap_or(Ok(Vec::new())))
                    .unwrap_or(Ok(Vec::new()))
                    .map_err(|e| format!("failed to read stdout: {e}"))?;
                let stderr = stderr_handle
                    .map(|h| h.join().unwrap_or(Ok(Vec::new())))
                    .unwrap_or(Ok(Vec::new()))
                    .map_err(|e| format!("failed to read stderr: {e}"))?;
                Ok(ToolOutput::new(status.code().unwrap_or(-1), stdout, stderr))
            }
            Ok(None) => {
                child.kill().ok();
                child.wait().ok();
                Err(format!("tool '{tool}' timed out after {timeout_ms}ms"))
            }
            Err(e) => Err(format!("error waiting for tool '{tool}': {e}")),
        }
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
        // Empty allowed_paths = no restriction (like run_tool path check)
        assert!(result.is_ok());
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
}
