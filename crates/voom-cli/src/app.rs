//! Application bootstrap: plugin initialization and kernel wiring.

use std::sync::Arc;

use anyhow::Result;
use voom_kernel::{Kernel, Plugin};

use crate::config::AppConfig;

/// Bootstrap a kernel with all native plugins registered.
///
/// All plugins go through `init_and_register` for consistent lifecycle management.
///
// Plugin priority scheme (lower number = runs first during event dispatch):
// 100 = storage (must initialize first to be available for other plugins)
// 90  = tool detector
// 80  = discovery
// 70  = introspector (ffprobe)
// 60  = policy evaluator
// 50  = phase orchestrator
// 40  = ffmpeg executor (fallback for all plans, claims on handle)
// 39  = mkvtoolnix executor (runs before ffmpeg — first shot at MKV plans)
// 30  = backup manager
// 20  = job manager
// 10  = web server (last, depends on all other plugins being registered)
pub fn bootstrap_kernel(config: &AppConfig) -> Result<Kernel> {
    let (kernel, _store) = bootstrap_kernel_with_store(config)?;
    Ok(kernel)
}

/// Bootstrap result containing the kernel and, when the sqlite-store plugin
/// is enabled, the store handle it created during initialization.
///
/// Using the returned store avoids opening a second SQLite connection (see
/// [`open_store`]), keeping all CLI commands and the event bus on the same
/// connection pool.
pub fn bootstrap_kernel_with_store(
    config: &AppConfig,
) -> Result<(Kernel, Option<Arc<dyn voom_domain::storage::StorageTrait>>)> {
    let mut kernel = Kernel::new();
    let data_dir = &config.data_dir;

    let disabled = &config.plugins.disabled_plugins;

    // Resolve per-plugin config as JSON, with a fallback to empty object.
    let plugin_json = |name: &str| -> serde_json::Value {
        config
            .plugin
            .get(name)
            .map(|t| match serde_json::to_value(t) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(plugin = name, error = %e,
                        "failed to convert plugin config to JSON; using empty config");
                    serde_json::json!({})
                }
            })
            .unwrap_or_else(|| serde_json::json!({}))
    };

    // Helper macro to conditionally register a plugin (skips if disabled).
    macro_rules! register_if_enabled {
        ($name:expr, $plugin:expr, $priority:expr, $label:expr) => {
            if !disabled.iter().any(|d| d == $name) {
                let ctx = voom_kernel::PluginContext {
                    config: plugin_json($name),
                    data_dir: data_dir.clone(),
                };
                kernel
                    .init_and_register(Arc::new($plugin), $priority, &ctx)
                    .map_err(|e| anyhow::anyhow!("Failed to initialize {}: {e}", $label))?;
            }
        };
    }

    // Storage plugin (highest priority — stores everything).
    // Initialized manually (not via the macro) so we can capture the store
    // handle and return it to callers, avoiding a second SQLite connection.
    let mut store_handle: Option<Arc<dyn voom_domain::storage::StorageTrait>> = None;
    if !disabled.iter().any(|d| d == "sqlite-store") {
        let mut plugin = voom_sqlite_store::SqliteStorePlugin::new();
        let ctx = voom_kernel::PluginContext {
            config: plugin_json("sqlite-store"),
            data_dir: data_dir.clone(),
        };
        plugin
            .init(&ctx)
            .map_err(|e| anyhow::anyhow!("Failed to initialize storage: {e}"))?;

        // Capture the store handle before moving the plugin into an Arc
        store_handle = plugin
            .store()
            .map(|s| Arc::clone(s) as Arc<dyn voom_domain::storage::StorageTrait>);

        let plugin_arc: Arc<dyn voom_kernel::Plugin> = Arc::new(plugin);
        kernel.register_plugin(plugin_arc, 100);
    }

    // Tool detector
    register_if_enabled!(
        "tool-detector",
        voom_tool_detector::ToolDetectorPlugin::new(),
        90,
        "tool detector"
    );

    // Discovery
    register_if_enabled!(
        "discovery",
        voom_discovery::DiscoveryPlugin::new(),
        80,
        "discovery"
    );

    // Introspector (reads ffprobe_path from ctx.config during init)
    register_if_enabled!(
        "ffprobe-introspector",
        voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new(),
        70,
        "introspector"
    );

    // Policy evaluator
    register_if_enabled!(
        "policy-evaluator",
        voom_policy_evaluator::PolicyEvaluatorPlugin::new(),
        60,
        "policy evaluator"
    );

    // Phase orchestrator
    register_if_enabled!(
        "phase-orchestrator",
        voom_phase_orchestrator::PhaseOrchestratorPlugin::new(),
        50,
        "phase orchestrator"
    );

    // Executors — mkvtoolnix at 39 gets first shot at MKV plans
    register_if_enabled!(
        "mkvtoolnix-executor",
        voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new(),
        39,
        "mkvtoolnix executor"
    );

    register_if_enabled!(
        "ffmpeg-executor",
        voom_ffmpeg_executor::FfmpegExecutorPlugin::new(),
        40,
        "ffmpeg executor"
    );

    // Backup manager
    register_if_enabled!(
        "backup-manager",
        voom_backup_manager::BackupManagerPlugin::new(),
        30,
        "backup manager"
    );

    // Job manager
    register_if_enabled!(
        "job-manager",
        voom_job_manager::JobManagerPlugin::new(),
        20,
        "job manager"
    );

    // Web server
    register_if_enabled!(
        "web-server",
        voom_web_server::WebServerPlugin::new(),
        10,
        "web server"
    );

    // TODO: load WASM plugins here using loader.load_dir_with_config()
    // once WASM plugin integration is wired up (Sprint 13).

    Ok((kernel, store_handle))
}

/// Open a standalone storage handle using the configured data directory.
///
/// This creates an independent SQLite connection. When a kernel is also in use,
/// prefer [`bootstrap_kernel_with_store`] to reuse the store that the
/// sqlite-store plugin already opened, avoiding a second connection pool.
///
/// This function is still useful for commands that need storage but not the
/// full kernel (e.g. `db prune`, `jobs list`).
pub fn open_store(config: &AppConfig) -> Result<Arc<dyn voom_domain::storage::StorageTrait>> {
    let db_path = config.data_dir.join("voom.db");
    let store = voom_sqlite_store::store::SqliteStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open store: {e}"))?;
    Ok(Arc::new(store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KNOWN_PLUGIN_NAMES;

    #[test]
    fn test_known_plugin_names_matches_bootstrap_registration() {
        // Bootstrap with all plugins enabled, then verify every registered
        // plugin name appears in KNOWN_PLUGIN_NAMES and vice versa.
        let config = AppConfig::default();
        let (kernel, _store) =
            bootstrap_kernel_with_store(&config).expect("bootstrap should succeed with defaults");
        let registered = kernel.registry.plugin_names();
        for name in KNOWN_PLUGIN_NAMES {
            assert!(
                registered.iter().any(|n| n == name),
                "KNOWN_PLUGIN_NAMES contains '{name}' but it was not registered in bootstrap"
            );
        }
        for name in &registered {
            assert!(
                KNOWN_PLUGIN_NAMES.contains(&name.as_str()),
                "Plugin '{name}' is registered in bootstrap but missing from KNOWN_PLUGIN_NAMES"
            );
        }
    }

    // ── open_store ───────────────────────────────────────────

    #[test]
    fn test_open_store_creates_db_in_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            data_dir: dir.path().to_path_buf(),
            ..AppConfig::default()
        };

        let store = open_store(&config);
        assert!(store.is_ok());
        assert!(dir.path().join("voom.db").exists());
    }
}
