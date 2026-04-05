//! Host state and function implementations for WASM plugins.
//!
//! Provides the host-side implementations of functions that WASM plugins
//! can call: logging, plugin data storage, tool execution, and HTTP requests.

mod functions;
mod store;

pub use store::{
    InMemoryPluginStore, InMemoryTransitionStore, StorageBackedPluginStore,
    StorageBackedTransitionStore, WasmPluginStore, WasmTransitionStore,
};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Maximum size for plugin data values (1 MiB).
pub const MAX_PLUGIN_DATA_VALUE_SIZE: usize = 1024 * 1024;

/// Resource limits for WASM plugin execution.
#[non_exhaustive]
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

/// State provided to WASM plugins via host function imports.
///
/// Each WASM plugin instance gets its own `HostState`, which holds
/// plugin-specific data and shared references to host services.
#[non_exhaustive]
pub struct HostState {
    /// Name of the plugin this state belongs to.
    pub plugin_name: String,
    /// In-memory key-value store for plugin data.
    /// In production this would be backed by `StorageTrait`.
    pub plugin_data: HashMap<String, Vec<u8>>,
    /// Allowed directories for tool execution (security sandbox).
    pub allowed_paths: Vec<PathBuf>,
    /// Shared storage backend for persistent plugin data.
    pub storage: Option<Arc<dyn WasmPluginStore>>,
    /// Shared transition store for querying file history.
    pub transition_store: Option<Arc<dyn WasmTransitionStore>>,
    /// Allowed tool names (empty = deny all).
    pub allowed_tools: Vec<String>,
    /// Allowed HTTP domains (empty = deny all, matching `run_tool` semantics).
    pub allowed_http_domains: Vec<String>,
    /// Capabilities declared by this plugin (used for runtime enforcement).
    pub allowed_capabilities: HashSet<String>,
    /// WASM resource limits.
    pub wasm_limits: WasmResourceLimits,
    /// Store limits for wasmtime (only used when feature = "wasm").
    #[cfg(feature = "wasm")]
    pub store_limits: wasmtime::StoreLimits,
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
            transition_store: None,
            allowed_tools: Vec::new(),
            allowed_http_domains: Vec::new(),
            allowed_capabilities: HashSet::new(),
            wasm_limits: WasmResourceLimits::default(),
            #[cfg(feature = "wasm")]
            store_limits: wasmtime::StoreLimits::default(),
        }
    }

    #[must_use]
    pub fn with_http_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_http_domains = domains;
        self
    }

    #[must_use]
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    #[must_use]
    pub fn with_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.allowed_paths = paths;
        self
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: HashSet<String>) -> Self {
        self.allowed_capabilities = capabilities;
        self
    }

    #[must_use]
    pub fn with_storage(mut self, storage: Arc<dyn WasmPluginStore>) -> Self {
        self.storage = Some(storage);
        self
    }

    #[must_use]
    pub fn with_transition_store(mut self, store: Arc<dyn WasmTransitionStore>) -> Self {
        self.transition_store = Some(store);
        self
    }

    /// Pre-seed plugin data with initial configuration from the host.
    ///
    /// Stores the config as JSON bytes under the `"config"` key in the
    /// in-memory plugin data store. WASM plugins can then retrieve it
    /// via `get_plugin_data("config")`.
    ///
    /// Note: both `{}` (empty object) and `null` are treated as "no config"
    /// and do **not** seed the store. A plugin that receives no config and one
    /// that receives `{}` are therefore indistinguishable via `get_plugin_data`.
    #[must_use]
    pub fn with_initial_config(mut self, config: serde_json::Value) -> Self {
        if !config.is_null() && config != serde_json::json!({}) {
            match serde_json::to_vec(&config) {
                Ok(bytes) => {
                    self.plugin_data.insert("config".to_string(), bytes);
                }
                Err(e) => {
                    tracing::warn!(
                        plugin = %self.plugin_name,
                        "Failed to serialize initial config: {e}"
                    );
                }
            }
        }
        self
    }
}

pub use voom_domain::host_types::{HttpResponse, ToolOutput};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_state_new() {
        let state = HostState::new("test-plugin".into());
        assert_eq!(state.plugin_name, "test-plugin");
        assert!(state.allowed_http_domains.is_empty());
        assert!(state.allowed_tools.is_empty());
    }

    #[test]
    fn test_host_state_builder() {
        let state = HostState::new("test".into())
            .with_http_domains(vec!["example.com".into()])
            .with_tools(vec!["ffprobe".into(), "mkvmerge".into()]);
        assert_eq!(state.allowed_http_domains, vec!["example.com"]);
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
        let store = Arc::new(InMemoryPluginStore::new());
        let mut state = HostState::new("test".into()).with_storage(store.clone());

        state.set_plugin_data("key1", b"value1").unwrap();
        assert_eq!(state.get_plugin_data("key1").unwrap(), b"value1");

        // Verify data is in the shared store.
        assert_eq!(store.get("test", "key1").unwrap().unwrap(), b"value1");
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
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
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
            get_err.contains("no allowed domains"),
            "expected permission error, got: {get_err}"
        );
        let post_err = state.http_post("http://example.com", &[], b"").unwrap_err();
        assert!(
            post_err.contains("no allowed domains"),
            "expected permission error, got: {post_err}"
        );
    }

    #[test]
    fn test_http_domain_not_in_allowlist() {
        let state = HostState::new("test".into()).with_http_domains(vec!["api.example.com".into()]);
        let err = state.http_get("http://evil.com/data", &[]).unwrap_err();
        assert!(
            err.contains("not in the allowed list"),
            "expected domain rejection, got: {err}"
        );
    }

    #[test]
    fn test_http_empty_allowlist_denies_all() {
        let state = HostState::new("test".into()).with_http_domains(vec![]);
        let err = state.http_get("http://example.com", &[]).unwrap_err();
        assert!(
            err.contains("no allowed domains"),
            "expected denial, got: {err}"
        );
    }

    #[test]
    fn test_http_get_connection_error() {
        // With HTTP enabled, a request to a non-routable address should return
        // a connection error (not "not yet implemented").
        let state = HostState::new("test".into()).with_http_domains(vec!["192.0.2.1".into()]);
        let result = state.http_get("http://192.0.2.1:1", &[]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP GET failed"),
            "expected connection error, got: {err}"
        );
    }

    #[test]
    fn test_http_post_connection_error() {
        let state = HostState::new("test".into()).with_http_domains(vec!["192.0.2.1".into()]);
        let result = state.http_post("http://192.0.2.1:1", &[], b"body");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP POST failed"),
            "expected connection error, got: {err}"
        );
    }

    #[test]
    fn test_run_tool_successful_with_timeout() {
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
        let result = state.run_tool("echo", &["hello".into()], 5000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn test_run_tool_timeout() {
        let state = HostState::new("test".into())
            .with_tools(vec!["sleep".into()])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
        let result = state.run_tool("sleep", &["10".into()], 100);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("timed out"),
            "expected timeout error, got: {err}"
        );
        assert!(err.contains("100ms"));
    }

    #[test]
    fn test_in_memory_data_store() {
        let store = InMemoryPluginStore::new();

        assert!(store.get("plugin1", "key1").unwrap().is_none());

        store.set("plugin1", "key1", b"data").unwrap();
        assert_eq!(store.get("plugin1", "key1").unwrap().unwrap(), b"data");

        store.delete("plugin1", "key1").unwrap();
        assert!(store.get("plugin1", "key1").unwrap().is_none());

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
        // Canonicalize to resolve symlinks (macOS /tmp -> /private/tmp)
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = canonical_dir.join("file.txt");
        std::fs::write(&file_path, "test").unwrap();
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_paths(vec![canonical_dir])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
        let result = state.run_tool("echo", &[file_path.to_string_lossy().into()], 5000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_tool_path_blocked() {
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_paths(vec![PathBuf::from("/tmp")])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
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
            .with_paths(vec![allowed_dir])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
        // /etc/passwd exists and will canonicalize to itself — outside allowed dir
        let result = state.run_tool("echo", &["/etc/passwd".into()], 5000);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not within allowed directories"));
    }

    #[test]
    fn test_run_tool_flag_path_blocked() {
        let state = HostState::new("test".into())
            .with_tools(vec!["echo".into()])
            .with_paths(vec![PathBuf::from("/tmp")])
            .with_capabilities(HashSet::from(["execute:tool".to_string()]));
        let result = state.run_tool("echo", &["--input=/etc/passwd".into()], 5000);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("not within allowed directories"),
            "expected path rejection for --flag=/path pattern"
        );
    }

    #[test]
    fn test_with_initial_config() {
        let config = serde_json::json!({"api_key": "abc123", "url": "http://localhost"});
        let state = HostState::new("test".into()).with_initial_config(config.clone());
        let data = state
            .get_plugin_data("config")
            .expect("config should be seeded");
        let loaded: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn test_with_initial_config_empty_does_not_seed() {
        let state = HostState::new("test".into()).with_initial_config(serde_json::json!({}));
        assert!(state.get_plugin_data("config").is_none());
    }

    #[test]
    fn test_with_initial_config_null_does_not_seed() {
        let state = HostState::new("test".into()).with_initial_config(serde_json::Value::Null);
        assert!(state.get_plugin_data("config").is_none());
    }

    #[test]
    fn test_with_initial_config_with_storage() {
        // Seeded config should be accessible even when persistent storage is attached.
        let store = Arc::new(InMemoryPluginStore::new());
        let config = serde_json::json!({"api_key": "abc123"});
        let state = HostState::new("test".into())
            .with_storage(store.clone())
            .with_initial_config(config.clone());

        // get_plugin_data should fall through to in-memory seeded config.
        let data = state
            .get_plugin_data("config")
            .expect("seeded config should be accessible with storage attached");
        let loaded: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn test_with_initial_config_storage_overrides_seed() {
        // A value written to storage should take precedence over the seeded default.
        let store = Arc::new(InMemoryPluginStore::new());
        let config = serde_json::json!({"api_key": "original"});
        let mut state = HostState::new("test".into())
            .with_storage(store.clone())
            .with_initial_config(config);

        // Override via set_plugin_data (writes to storage).
        let override_config = serde_json::json!({"api_key": "overridden"});
        let override_bytes = serde_json::to_vec(&override_config).unwrap();
        state.set_plugin_data("config", &override_bytes).unwrap();

        // Should get the storage value, not the seeded one.
        let data = state.get_plugin_data("config").unwrap();
        let loaded: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(loaded, override_config);
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
