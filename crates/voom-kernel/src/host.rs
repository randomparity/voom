//! Host state and function implementations for WASM plugins.
//!
//! Provides the host-side implementations of functions that WASM plugins
//! can call: logging, plugin data storage, tool execution, and HTTP requests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// State provided to WASM plugins via host function imports.
///
/// Each WASM plugin instance gets its own HostState, which holds
/// plugin-specific data and shared references to host services.
pub struct HostState {
    /// Name of the plugin this state belongs to.
    pub plugin_name: String,
    /// In-memory key-value store for plugin data.
    /// In production this would be backed by StorageTrait.
    pub plugin_data: HashMap<String, Vec<u8>>,
    /// Allowed directories for tool execution (security sandbox).
    pub allowed_paths: Vec<PathBuf>,
    /// Shared storage backend for persistent plugin data.
    pub storage: Option<Arc<dyn PluginDataStore>>,
    /// Allowed tool names (empty = allow all).
    pub allowed_tools: Vec<String>,
    /// HTTP client configuration.
    pub http_allowed: bool,
}

/// Trait for persistent plugin data storage.
/// Implemented by the sqlite-store plugin or in-memory for testing.
pub trait PluginDataStore: Send + Sync {
    fn get(&self, plugin_name: &str, key: &str) -> Option<Vec<u8>>;
    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String>;
    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String>;
}

/// In-memory implementation of PluginDataStore for testing.
pub struct InMemoryDataStore {
    data: Mutex<HashMap<String, HashMap<String, Vec<u8>>>>,
}

impl InMemoryDataStore {
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
        let data = self.data.lock().unwrap();
        data.get(plugin_name).and_then(|m| m.get(key)).cloned()
    }

    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String> {
        let mut data = self.data.lock().unwrap();
        data.entry(plugin_name.to_string())
            .or_default()
            .insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String> {
        let mut data = self.data.lock().unwrap();
        if let Some(m) = data.get_mut(plugin_name) {
            m.remove(key);
        }
        Ok(())
    }
}

impl HostState {
    /// Create a new HostState for a plugin with default settings.
    pub fn new(plugin_name: String) -> Self {
        Self {
            plugin_name,
            plugin_data: HashMap::new(),
            allowed_paths: Vec::new(),
            storage: None,
            allowed_tools: Vec::new(),
            http_allowed: false,
        }
    }

    /// Enable HTTP access for this plugin.
    pub fn with_http(mut self) -> Self {
        self.http_allowed = true;
        self
    }

    /// Set allowed tools for this plugin.
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Set persistent storage backend.
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
        // Security check: verify tool is allowed.
        if !self.allowed_tools.is_empty() && !self.allowed_tools.iter().any(|t| t == tool) {
            return Err(format!(
                "tool '{}' is not in the allowed list for plugin '{}'",
                tool, self.plugin_name
            ));
        }

        let mut child = Command::new(tool)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn tool '{}': {}", tool, e))?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|e| format!("failed to read output: {}", e))?;
                    return Ok(ToolOutput {
                        exit_code: output.status.code().unwrap_or(-1),
                        stdout: output.stdout,
                        stderr: output.stderr,
                    });
                }
                Ok(None) if Instant::now() >= deadline => {
                    child.kill().ok();
                    return Err(format!("tool '{}' timed out after {}ms", tool, timeout_ms));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(format!("error waiting for tool '{}': {}", tool, e)),
            }
        }
    }

    /// Perform an HTTP GET request.
    pub fn http_get(
        &self,
        _url: &str,
        _headers: &[(String, String)],
    ) -> Result<HttpResponse, String> {
        if !self.http_allowed {
            return Err(format!(
                "HTTP access not enabled for plugin '{}'",
                self.plugin_name
            ));
        }
        // HTTP implementation would use ureq or reqwest here.
        // For now, return an error indicating it's not yet implemented.
        Err("HTTP GET not yet implemented in host".to_string())
    }

    /// Perform an HTTP POST request.
    pub fn http_post(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &[u8],
    ) -> Result<HttpResponse, String> {
        if !self.http_allowed {
            return Err(format!(
                "HTTP access not enabled for plugin '{}'",
                self.plugin_name
            ));
        }
        Err("HTTP POST not yet implemented in host".to_string())
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
    fn test_run_tool_all_allowed() {
        // Empty allowed_tools means all tools are permitted.
        let state = HostState::new("test".into());
        let result = state.run_tool("echo", &["test".into()], 5000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_http_blocked_by_default() {
        let state = HostState::new("test".into());
        assert!(state.http_get("http://example.com", &[]).is_err());
        assert!(state.http_post("http://example.com", &[], b"").is_err());
    }

    #[test]
    fn test_run_tool_successful_with_timeout() {
        let state = HostState::new("test".into());
        let result = state.run_tool("echo", &["hello".into()], 5000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn test_run_tool_timeout() {
        let state = HostState::new("test".into());
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
}
