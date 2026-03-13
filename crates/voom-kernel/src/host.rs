//! Host state and function implementations for WASM plugins.
//!
//! Provides the host-side implementations of functions that WASM plugins
//! can call: logging, plugin data storage, tool execution, and HTTP requests.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wait_timeout::ChildExt;

/// State provided to WASM plugins via host function imports.
///
/// Each WASM plugin instance gets its own `HostState`, which holds
/// plugin-specific data and shared references to host services.
/// Maximum size for plugin data values (1 MiB).
pub const MAX_PLUGIN_DATA_VALUE_SIZE: usize = 1024 * 1024;

/// Resource limits for WASM plugin execution.
#[derive(Debug, Clone)]
pub struct WasmResourceLimits {
    /// Maximum memory in bytes a WASM module can allocate (default: 256 MiB).
    pub max_memory_bytes: usize,
    /// Epoch deadline ticks before interruption (default: 200).
    pub epoch_deadline_ticks: u64,
}

impl Default for WasmResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 256 * 1024 * 1024,
            epoch_deadline_ticks: 200,
        }
    }
}

pub struct HostState {
    /// Name of the plugin this state belongs to.
    pub plugin_name: String,
    /// In-memory key-value store for plugin data.
    /// In production this would be backed by `StorageTrait`.
    pub plugin_data: HashMap<String, Vec<u8>>,
    /// Allowed directories for tool execution (security sandbox).
    pub allowed_paths: Vec<PathBuf>,
    /// Shared storage backend for persistent plugin data.
    pub storage: Option<Arc<dyn PluginDataStore>>,
    /// Allowed tool names (empty = deny all).
    pub allowed_tools: Vec<String>,
    /// HTTP client configuration.
    pub http_allowed: bool,
    /// Capabilities declared by this plugin (used for runtime enforcement).
    pub allowed_capabilities: HashSet<String>,
    /// WASM resource limits.
    pub wasm_limits: WasmResourceLimits,
    /// Store limits for wasmtime (only used when feature = "wasm").
    #[cfg(feature = "wasm")]
    pub store_limits: wasmtime::StoreLimits,
}

/// Trait for persistent plugin data storage.
/// Implemented by the sqlite-store plugin or in-memory for testing.
pub trait PluginDataStore: Send + Sync {
    fn get(&self, plugin_name: &str, key: &str) -> Option<Vec<u8>>;
    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String>;
    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String>;
}

/// In-memory implementation of `PluginDataStore` for testing.
pub struct InMemoryDataStore {
    data: Mutex<HashMap<String, HashMap<String, Vec<u8>>>>,
}

impl InMemoryDataStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryDataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginDataStore for InMemoryDataStore {
    fn get(&self, plugin_name: &str, key: &str) -> Option<Vec<u8>> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.get(plugin_name).and_then(|m| m.get(key)).cloned()
    }

    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.entry(plugin_name.to_string())
            .or_default()
            .insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = data.get_mut(plugin_name) {
            m.remove(key);
        }
        Ok(())
    }
}

impl HostState {
    /// Create a new `HostState` for a plugin with default settings.
    #[must_use]
    pub fn new(plugin_name: String) -> Self {
        Self {
            plugin_name,
            plugin_data: HashMap::new(),
            allowed_paths: Vec::new(),
            storage: None,
            allowed_tools: Vec::new(),
            http_allowed: false,
            allowed_capabilities: HashSet::new(),
            wasm_limits: WasmResourceLimits::default(),
            #[cfg(feature = "wasm")]
            store_limits: wasmtime::StoreLimits::default(),
        }
    }

    /// Enable HTTP access for this plugin.
    #[must_use]
    pub fn with_http(mut self) -> Self {
        self.http_allowed = true;
        self
    }

    /// Set allowed tools for this plugin.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Set allowed filesystem paths for tool execution.
    #[must_use]
    pub fn with_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.allowed_paths = paths;
        self
    }

    /// Set allowed capabilities for this plugin.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: HashSet<String>) -> Self {
        self.allowed_capabilities = capabilities;
        self
    }

    /// Set persistent storage backend.
    #[must_use]
    pub fn with_storage(mut self, storage: Arc<dyn PluginDataStore>) -> Self {
        self.storage = Some(storage);
        self
    }

    // --- Host function implementations ---

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

    /// Get plugin-specific persisted data by key.
    #[must_use]
    pub fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>> {
        // Try persistent storage first, fall back to in-memory.
        if let Some(storage) = &self.storage {
            storage.get(&self.plugin_name, key)
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
        if !self.allowed_paths.is_empty() {
            for arg in args {
                let path = Path::new(arg);
                if path.is_absolute() || arg.starts_with("./") || arg.starts_with("../") {
                    let canonical = std::fs::canonicalize(path).map_err(|e| {
                        format!(
                            "cannot resolve path '{}' for plugin '{}': {e}",
                            arg, self.plugin_name
                        )
                    })?;
                    let allowed = self
                        .allowed_paths
                        .iter()
                        .any(|allowed_dir| canonical.starts_with(allowed_dir));
                    if !allowed {
                        return Err(format!(
                            "path '{}' is not within allowed directories for plugin '{}'",
                            arg, self.plugin_name
                        ));
                    }
                }
            }
        }

        // Security check: verify plugin has execute capability.
        if !self.allowed_capabilities.is_empty()
            && !self
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
            .map_err(|e| format!("failed to spawn tool '{}': {}", tool, e))?;

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
                    .map_err(|e| format!("failed to read stdout: {}", e))?;
                let stderr = stderr_handle
                    .map(|h| h.join().unwrap_or(Ok(Vec::new())))
                    .unwrap_or(Ok(Vec::new()))
                    .map_err(|e| format!("failed to read stderr: {}", e))?;
                Ok(ToolOutput {
                    exit_code: status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                })
            }
            Ok(None) => {
                child.kill().ok();
                child.wait().ok();
                Err(format!("tool '{}' timed out after {}ms", tool, timeout_ms))
            }
            Err(e) => Err(format!("error waiting for tool '{}': {}", tool, e)),
        }
    }

    /// Perform an HTTP GET request.
    pub fn http_get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, String> {
        if !self.http_allowed {
            return Err(format!(
                "HTTP access not enabled for plugin '{}'",
                self.plugin_name
            ));
        }

        let mut request = ureq::get(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request
            .call()
            .map_err(|e| format!("HTTP GET failed: {e}"))?;

        let status = response.status();
        let header_names = response.headers_names();
        let resp_headers: Vec<(String, String)> = header_names
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

        Ok(HttpResponse {
            status,
            headers: resp_headers,
            body,
        })
    }

    /// Perform an HTTP POST request.
    pub fn http_post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, String> {
        if !self.http_allowed {
            return Err(format!(
                "HTTP access not enabled for plugin '{}'",
                self.plugin_name
            ));
        }

        let mut request = ureq::post(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request
            .send_bytes(body)
            .map_err(|e| format!("HTTP POST failed: {e}"))?;

        let status = response.status();
        let header_names = response.headers_names();
        let resp_headers: Vec<(String, String)> = header_names
            .iter()
            .filter_map(|name| {
                response
                    .header(name)
                    .map(|val| (name.clone(), val.to_string()))
            })
            .collect();
        let mut resp_body = Vec::new();
        response
            .into_reader()
            .take(10 * 1024 * 1024) // 10 MiB limit
            .read_to_end(&mut resp_body)
            .map_err(|e| format!("failed to read response body: {e}"))?;

        Ok(HttpResponse {
            status,
            headers: resp_headers,
            body: resp_body,
        })
    }
}

/// Output from running an external tool.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Response from an HTTP request.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_state_new() {
        let state = HostState::new("test-plugin".into());
        assert_eq!(state.plugin_name, "test-plugin");
        assert!(!state.http_allowed);
        assert!(state.allowed_tools.is_empty());
    }

    #[test]
    fn test_host_state_builder() {
        let state = HostState::new("test".into())
            .with_http()
            .with_tools(vec!["ffprobe".into(), "mkvmerge".into()]);
        assert!(state.http_allowed);
        assert_eq!(state.allowed_tools.len(), 2);
    }

    #[test]
    fn test_plugin_data_in_memory() {
        let mut state = HostState::new("test".into());

        assert!(state.get_plugin_data("key1").is_none());

        state.set_plugin_data("key1", b"hello world").unwrap();
        assert_eq!(state.get_plugin_data("key1").unwrap(), b"hello world");

        state.set_plugin_data("key1", b"updated").unwrap();
        assert_eq!(state.get_plugin_data("key1").unwrap(), b"updated");
    }

    #[test]
    fn test_plugin_data_persistent_store() {
        let store = Arc::new(InMemoryDataStore::new());
        let mut state = HostState::new("test".into()).with_storage(store.clone());

        state.set_plugin_data("key1", b"value1").unwrap();
        assert_eq!(state.get_plugin_data("key1").unwrap(), b"value1");

        // Verify data is in the shared store.
        assert_eq!(store.get("test", "key1").unwrap(), b"value1");
    }

    #[test]
    fn test_run_tool_blocked() {
        let state = HostState::new("test".into()).with_tools(vec!["ffprobe".into()]);
        let result = state.run_tool("rm", &["-rf".into()], 5000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in the allowed list"));
    }

    #[test]
    fn test_run_tool_allowed() {
        let state = HostState::new("test".into()).with_tools(vec!["echo".into()]);
        let result = state.run_tool("echo", &["hello".into()], 5000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn test_run_tool_empty_allowlist_denies_all() {
        // Empty allowed_tools means no tools are permitted (deny all).
        let state = HostState::new("test".into());
        let result = state.run_tool("echo", &["test".into()], 5000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in the allowed list"));
    }

    #[test]
    fn test_http_blocked_by_default() {
        let state = HostState::new("test".into());
        let get_err = state.http_get("http://example.com", &[]).unwrap_err();
        assert!(
            get_err.contains("HTTP access not enabled"),
            "expected permission error, got: {}",
            get_err
        );
        let post_err = state.http_post("http://example.com", &[], b"").unwrap_err();
        assert!(
            post_err.contains("HTTP access not enabled"),
            "expected permission error, got: {}",
            post_err
        );
    }

    #[test]
    fn test_http_get_connection_error() {
        // With HTTP enabled, a request to a non-routable address should return
        // a connection error (not "not yet implemented").
        let state = HostState::new("test".into()).with_http();
        let result = state.http_get("http://192.0.2.1:1", &[]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP GET failed"),
            "expected connection error, got: {}",
            err
        );
    }

    #[test]
    fn test_http_post_connection_error() {
        let state = HostState::new("test".into()).with_http();
        let result = state.http_post("http://192.0.2.1:1", &[], b"body");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP POST failed"),
            "expected connection error, got: {}",
            err
        );
    }

    #[test]
    fn test_run_tool_successful_with_timeout() {
        let state = HostState::new("test".into()).with_tools(vec!["echo".into()]);
        let result = state.run_tool("echo", &["hello".into()], 5000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn test_run_tool_timeout() {
        let state = HostState::new("test".into()).with_tools(vec!["sleep".into()]);
        let result = state.run_tool("sleep", &["10".into()], 100);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("timed out"),
            "expected timeout error, got: {}",
            err
        );
        assert!(err.contains("100ms"));
    }

    #[test]
    fn test_in_memory_data_store() {
        let store = InMemoryDataStore::new();

        assert!(store.get("plugin1", "key1").is_none());

        store.set("plugin1", "key1", b"data").unwrap();
        assert_eq!(store.get("plugin1", "key1").unwrap(), b"data");

        store.delete("plugin1", "key1").unwrap();
        assert!(store.get("plugin1", "key1").is_none());

        // Deleting non-existent key is fine.
        store.delete("plugin1", "nonexistent").unwrap();
    }

    #[test]
    fn test_plugin_data_size_limit() {
        let mut state = HostState::new("test".into());
        // Just under the limit should work.
        let ok_data = vec![0u8; MAX_PLUGIN_DATA_VALUE_SIZE];
        assert!(state.set_plugin_data("key", &ok_data).is_ok());

        // Over the limit should fail.
        let big_data = vec![0u8; MAX_PLUGIN_DATA_VALUE_SIZE + 1];
        let result = state.set_plugin_data("key2", &big_data);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exceeds maximum size"));
    }

    #[test]
    fn test_run_tool_path_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("file.txt");
        std::fs::write(&file_path, "test").unwrap();
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_paths(vec![dir.path().to_path_buf()]);
        let result = state.run_tool("echo", &[file_path.to_string_lossy().into()], 5000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_tool_path_blocked() {
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_paths(vec![PathBuf::from("/tmp")]);
        let result = state.run_tool("echo", &["/etc/passwd".into()], 5000);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not within allowed directories"));
    }

    #[test]
    fn test_run_tool_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file inside the allowed dir
        let allowed_dir = dir.path().join("allowed");
        std::fs::create_dir(&allowed_dir).unwrap();
        std::fs::write(allowed_dir.join("file.txt"), "ok").unwrap();
        // Try to access a file via traversal outside the allowed dir
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_paths(vec![allowed_dir]);
        // /etc/passwd exists and will canonicalize to itself — outside allowed dir
        let result = state.run_tool("echo", &["/etc/passwd".into()], 5000);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not within allowed directories"));
    }

    #[test]
    fn test_run_tool_capability_enforcement() {
        let mut caps = std::collections::HashSet::new();
        caps.insert("evaluate".to_string());
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_capabilities(caps);
        let result = state.run_tool("echo", &["hello".into()], 5000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("lacks 'execute' capability"));
    }
}
