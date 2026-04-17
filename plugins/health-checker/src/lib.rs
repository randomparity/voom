//! Health check plugin: runtime system readiness monitoring.
//!
//! Runs filesystem-level checks during `init()` and emits `HealthStatus`
//! events through the event bus. Provides a `run_checks()` method for
//! periodic re-checks when the server is running.

use std::path::Path;

use serde::Deserialize;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, HealthStatusEvent};
use voom_kernel::{Plugin, PluginContext};

/// Plugin configuration read from `[plugin.health-checker]` in config.toml.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct HealthCheckerConfig {
    /// Interval in seconds for periodic re-checks (0 = disabled).
    /// Only meaningful when the serve command runs the periodic loop.
    pub interval_secs: u64,
    /// Number of days to retain health check records before pruning.
    pub retention_days: u32,
}

fn default_retention_days() -> u32 {
    30
}

impl Default for HealthCheckerConfig {
    fn default() -> Self {
        Self {
            interval_secs: 300,
            retention_days: default_retention_days(),
        }
    }
}

/// Health check plugin: verifies filesystem-level readiness.
pub struct HealthCheckerPlugin {
    capabilities: Vec<Capability>,
    config: HealthCheckerConfig,
}

impl HealthCheckerPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::HealthCheck],
            config: HealthCheckerConfig::default(),
        }
    }

    /// Access the parsed plugin config.
    #[must_use]
    pub fn config(&self) -> &HealthCheckerConfig {
        &self.config
    }

    /// Run all health checks against the given data directory.
    /// Returns `HealthStatus` events suitable for dispatching.
    #[must_use]
    pub fn run_checks(&self, data_dir: &Path) -> Vec<Event> {
        let mut events = Vec::new();

        let dir_exists = data_dir.is_dir();
        events.push(Event::HealthStatus(HealthStatusEvent::new(
            "data_dir_exists",
            dir_exists,
            if dir_exists {
                Some(format!("{}", data_dir.display()))
            } else {
                Some(format!("{} does not exist", data_dir.display()))
            },
        )));

        if dir_exists {
            let writable = check_writable(data_dir);
            events.push(Event::HealthStatus(HealthStatusEvent::new(
                "data_dir_writable",
                writable,
                if writable {
                    None
                } else {
                    Some(format!("{} is not writable", data_dir.display()))
                },
            )));
        }

        events
    }
}

impl Default for HealthCheckerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for HealthCheckerPlugin {
    fn name(&self) -> &'static str {
        "health-checker"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        self.config = match ctx.parse_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("health-checker config parse failed, using defaults: {e}");
                HealthCheckerConfig::default()
            }
        };

        tracing::info!(
            interval_secs = self.config.interval_secs,
            "health checker initialized"
        );

        Ok(self.run_checks(&ctx.data_dir))
    }

    fn shutdown(&self) -> Result<()> {
        tracing::info!("health checker shutting down");
        Ok(())
    }
}

/// Test whether a directory is writable by creating and removing a temp file.
fn check_writable(dir: &Path) -> bool {
    let probe = dir.join(".voom-health-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = HealthCheckerPlugin::new();
        assert_eq!(plugin.name(), "health-checker");
        assert_eq!(plugin.capabilities()[0].kind(), "health_check");
    }

    #[test]
    fn test_default_config() {
        let config = HealthCheckerConfig::default();
        assert_eq!(config.interval_secs, 300);
        assert_eq!(config.retention_days, 30);
    }

    #[test]
    fn test_run_checks_existing_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin = HealthCheckerPlugin::new();
        let events = plugin.run_checks(tmp.path());

        assert_eq!(events.len(), 2);

        match &events[0] {
            Event::HealthStatus(e) => {
                assert_eq!(e.check_name, "data_dir_exists");
                assert!(e.passed);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        match &events[1] {
            Event::HealthStatus(e) => {
                assert_eq!(e.check_name, "data_dir_writable");
                assert!(e.passed);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn test_run_checks_missing_dir() {
        let plugin = HealthCheckerPlugin::new();
        let events = plugin.run_checks(Path::new("/nonexistent/voom/dir"));

        // Only data_dir_exists check (writable skipped when dir missing)
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::HealthStatus(e) => {
                assert_eq!(e.check_name, "data_dir_exists");
                assert!(!e.passed);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn test_init_runs_checks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut plugin = HealthCheckerPlugin::new();
        let ctx = PluginContext::new(serde_json::json!({}), tmp.path().to_path_buf());
        let events = plugin.init(&ctx).expect("init");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_init_parses_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut plugin = HealthCheckerPlugin::new();
        let ctx = PluginContext::new(
            serde_json::json!({"interval_secs": 60}),
            tmp.path().to_path_buf(),
        );
        plugin.init(&ctx).expect("init");
        assert_eq!(plugin.config().interval_secs, 60);
    }

    #[test]
    fn test_handles_no_events() {
        let plugin = HealthCheckerPlugin::new();
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::HEALTH_STATUS));
    }

    #[test]
    fn test_shutdown_succeeds() {
        let plugin = HealthCheckerPlugin::new();
        assert!(plugin.shutdown().is_ok());
    }

    #[test]
    fn test_config_deserialization() {
        let json = serde_json::json!({"interval_secs": 120});
        let config: HealthCheckerConfig = serde_json::from_value(json).expect("parse");
        assert_eq!(config.interval_secs, 120);
    }

    #[test]
    fn test_config_default_on_empty() {
        let json = serde_json::json!({});
        let config: HealthCheckerConfig = serde_json::from_value(json).expect("parse");
        assert_eq!(config.interval_secs, 300);
        assert_eq!(config.retention_days, 30);
    }

    #[test]
    fn test_config_retention_days() {
        let json = serde_json::json!({"retention_days": 7});
        let config: HealthCheckerConfig = serde_json::from_value(json).expect("parse");
        assert_eq!(config.retention_days, 7);
    }
}
