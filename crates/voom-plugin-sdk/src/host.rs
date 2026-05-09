//! Host function abstraction for WASM plugins.
//!
//! In a real WASM plugin, these functions are provided by the host via WIT imports.
//! This module defines the shared trait and associated types so that all WASM plugins
//! use a single definition rather than each defining their own.

/// Abstraction over host-provided functions.
///
/// In a real WASM plugin, these would be WIT imports from the host interface.
/// Plugins that only need a subset of host functions can rely on the default
/// implementations, which return errors or no-ops for unimplemented functions.
pub trait HostFunctions {
    /// Read metadata for a file (sandboxed to media library paths).
    fn read_file_metadata(&self, path: &str) -> Result<Vec<u8>, String> {
        let _ = path;
        Err("read_file_metadata not available".to_string())
    }

    /// List files in a directory matching a pattern (empty pattern = all).
    fn list_files(&self, dir: &str, pattern: &str) -> Result<Vec<String>, String> {
        let _ = (dir, pattern);
        Err("list_files not available".to_string())
    }

    /// Execute an HTTP GET request via the host.
    fn http_get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, String> {
        let _ = (url, headers);
        Err("http_get not available".to_string())
    }

    /// Execute an HTTP POST request via the host.
    fn http_post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, String> {
        let _ = (url, headers, body);
        Err("http_post not available".to_string())
    }

    /// Write content to a file via the host's sandboxed file writer.
    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), String> {
        let _ = (path, content);
        Err("write_file not available".to_string())
    }

    /// Run an external tool via the host's sandboxed tool runner.
    fn run_tool(&self, tool: &str, args: &[String], timeout_ms: u64) -> Result<ToolOutput, String> {
        let _ = (tool, args, timeout_ms);
        Err("run_tool not available".to_string())
    }

    /// Retrieve plugin-specific data from the host's data store.
    fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>>;

    /// Store plugin-specific data in the host's data store.
    fn set_plugin_data(&self, key: &str, value: &[u8]) -> Result<(), String>;

    /// Query file transitions by file ID.
    fn get_file_transitions(&self, file_id: &str) -> Result<Vec<u8>, String> {
        let _ = file_id;
        Err("get_file_transitions not available".to_string())
    }

    /// Query file transitions by filesystem path.
    fn get_path_transitions(&self, path: &str) -> Result<Vec<u8>, String> {
        let _ = path;
        Err("get_path_transitions not available".to_string())
    }

    /// Log a message at the given level via the host's logging system.
    fn log(&self, level: &str, message: &str);
}

pub use voom_domain::host_types::{HttpResponse, ToolOutput};

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHost;

    impl HostFunctions for TestHost {
        fn get_plugin_data(&self, _key: &str) -> Option<Vec<u8>> {
            None
        }

        fn set_plugin_data(&self, _key: &str, _value: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn log(&self, _level: &str, _message: &str) {}
    }

    #[test]
    fn test_default_list_files_returns_error() {
        let host = TestHost;
        let result = host.list_files("/some/dir", "*.mkv");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "list_files not available");
    }

    #[test]
    fn test_default_http_get_returns_error() {
        let host = TestHost;
        let result = host.http_get("http://example.com", &[]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "http_get not available");
    }

    #[test]
    fn test_default_run_tool_returns_error() {
        let host = TestHost;
        let result = host.run_tool("ffmpeg", &[], 1000);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "run_tool not available");
    }

    #[test]
    fn test_default_transition_queries_return_error() {
        let host = TestHost;
        let by_id = host.get_file_transitions("file-id");
        let by_path = host.get_path_transitions("/media/movie.mkv");
        assert_eq!(by_id.unwrap_err(), "get_file_transitions not available");
        assert_eq!(by_path.unwrap_err(), "get_path_transitions not available");
    }
}
