//! External tool execution host functions.

use std::time::Duration;

use crate::host::{HostState, ToolOutput};

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
    /// Run an external tool with the given arguments.
    pub fn run_tool(
        &self,
        tool: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> Result<ToolOutput, String> {
        if self.allowed_tools.is_empty() || !self.allowed_tools.iter().any(|t| t == tool) {
            return Err(format!(
                "tool '{}' is not in the allowed list for plugin '{}'",
                tool, self.plugin_name
            ));
        }

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

        self.require_capability_kind("execute", "tool execution")?;

        let timeout = Duration::from_millis(timeout_ms);
        let output = voom_process::run_with_timeout_options(
            tool,
            args,
            timeout,
            voom_process::TimeoutOptions::default(),
        )
        .map_err(|e| host_tool_error(tool, timeout_ms, &e))?;

        Ok(ToolOutput::new(
            output.status.code().unwrap_or(-1),
            output.stdout,
            output.stderr,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_path;

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
