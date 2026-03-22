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
    pub fn resolve_plugin_data(&self, key: &str) -> Option<Vec<u8>> {
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
                        "storage error in resolve_plugin_data, falling back to in-memory"
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

        parse_response(response)
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

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}
