//! Capability checks and logging host functions.

use crate::host::HostState;

impl HostState {
    /// Return true when the manifest granted a capability with this kind.
    #[must_use]
    pub fn has_capability_kind(&self, kind: &str) -> bool {
        self.allowed_capabilities.iter().any(|capability| {
            capability == kind
                || capability
                    .strip_prefix(kind)
                    .is_some_and(|rest| rest.starts_with([':', '/']))
        })
    }

    /// Require a manifest capability kind before exposing a host operation.
    ///
    /// # Errors
    /// Returns an error when the plugin manifest did not grant `kind`.
    pub fn require_capability_kind(&self, kind: &str, operation: &str) -> Result<(), String> {
        if self.has_capability_kind(kind) {
            return Ok(());
        }
        Err(format!(
            "plugin '{}' lacks '{kind}' capability required for {operation}",
            self.plugin_name
        ))
    }

    /// Require at least one capability kind that is allowed to touch files.
    ///
    /// # Errors
    /// Returns an error when the plugin manifest did not grant any
    /// filesystem-capable kind.
    pub fn require_filesystem_capability(&self, operation: &str) -> Result<(), String> {
        const FILESYSTEM_KINDS: &[&str] = &[
            "backup",
            "discover",
            "execute",
            "generate_subtitle",
            "introspect",
            "synthesize",
            "transcribe",
            "verify",
        ];
        if FILESYSTEM_KINDS
            .iter()
            .any(|kind| self.has_capability_kind(kind))
        {
            return Ok(());
        }
        Err(format!(
            "plugin '{}' lacks a filesystem capability required for {operation}",
            self.plugin_name
        ))
    }

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
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::host::HostState;

    #[test]
    fn capability_kind_accepts_exact_and_scoped_values() {
        let mut state = HostState::new("test".into());
        state.allowed_capabilities = [
            "execute".to_string(),
            "enrich_metadata:tvdb".to_string(),
            "store/plugin-data".to_string(),
        ]
        .into_iter()
        .collect();

        assert!(state.has_capability_kind("execute"));
        assert!(state.has_capability_kind("enrich_metadata"));
        assert!(state.has_capability_kind("store"));
        assert!(!state.has_capability_kind("serve_http"));
    }

    #[test]
    fn require_capability_kind_reports_missing_kind() {
        let state = HostState::new("test-plugin".into());

        let error = state
            .require_capability_kind("serve_http", "HTTP GET")
            .unwrap_err();

        assert!(error.contains("test-plugin"));
        assert!(error.contains("serve_http"));
        assert!(error.contains("HTTP GET"));
    }

    #[test]
    fn require_filesystem_capability_accepts_file_oriented_kinds() {
        let mut state = HostState::new("test-plugin".into());
        state.allowed_capabilities = HashSet::from(["generate_subtitle".to_string()]);

        assert!(state.require_filesystem_capability("file writing").is_ok());
    }
}
