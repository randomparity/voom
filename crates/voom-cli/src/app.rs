//! Application bootstrap: plugin initialization and kernel wiring.

use std::path::Path;
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
// 60  = policy evaluator
// 50  = phase orchestrator
// 39  = mkvtoolnix executor
// 40  = ffmpeg executor
// 30  = backup manager
// 20  = job manager
pub fn bootstrap_kernel(config: &AppConfig) -> Result<Kernel> {
    let (kernel, _store) = bootstrap_kernel_with_store(config)?;
    Ok(kernel)
}

/// Bootstrap the kernel with all native plugins and return both the kernel
/// and the storage handle.
///
/// The store is always returned (not `Option`): if the sqlite-store plugin is
/// enabled its handle is reused so there is no second pool; if the plugin is
/// disabled a standalone pool is opened via [`open_store_in`] — the same
/// helper used by store-only commands.
pub fn bootstrap_kernel_with_store(
    config: &AppConfig,
) -> Result<(Kernel, Arc<dyn voom_domain::storage::StorageTrait>)> {
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
    // handle and return it to callers, keeping all CLI commands and the event
    // bus on the same connection pool.
    let store: Arc<dyn voom_domain::storage::StorageTrait> =
        if !disabled.iter().any(|d| d == "sqlite-store") {
            let mut plugin = voom_sqlite_store::SqliteStorePlugin::new();
            let ctx = voom_kernel::PluginContext {
                config: plugin_json("sqlite-store"),
                data_dir: data_dir.clone(),
            };
            plugin
                .init(&ctx)
                .map_err(|e| anyhow::anyhow!("Failed to initialize storage: {e}"))?;

            // Capture the store handle before moving the plugin into an Arc.
            // `plugin.store()` is always Some after a successful init().
            let handle = plugin
                .store()
                .map(|s| Arc::clone(s) as Arc<dyn voom_domain::storage::StorageTrait>)
                .expect("store is Some after successful init");

            let plugin_arc: Arc<dyn voom_kernel::Plugin> = Arc::new(plugin);
            kernel.register_plugin(plugin_arc, 100);

            handle
        } else {
            // sqlite-store disabled: open a standalone pool so callers always
            // get a usable handle.  No plugin is registered, so events will
            // not be persisted, but read-only CLI commands still work.
            open_store_in(data_dir).map_err(|e| anyhow::anyhow!("Failed to open storage: {e}"))?
        };

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

    // Executor — mkvtoolnix (MKV metadata, track removal/reorder, convert-to-MKV)
    register_if_enabled!(
        "mkvtoolnix-executor",
        voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new(),
        39,
        "mkvtoolnix executor"
    );

    // Executor — ffmpeg (transcode, non-MKV metadata, container conversion)
    // Priority 40: runs after mkvtoolnix (39) so mkvtoolnix gets first crack
    // at plans it can handle (MKV metadata, convert-to-MKV).
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

    // WASM plugins — loaded from the configured directory (if it exists).
    #[cfg(feature = "wasm")]
    {
        let wasm_dir = config
            .plugins
            .wasm_dir
            .clone()
            .unwrap_or_else(|| config.data_dir.join("plugins").join("wasm"));

        if wasm_dir.is_dir() {
            match voom_kernel::loader::wasm::WasmPluginLoader::new() {
                Ok(loader) => {
                    let results = loader.load_dir_with_config(&wasm_dir, &config.plugin);
                    for result in results {
                        match result {
                            Ok(plugin) => {
                                let name = plugin.name().to_string();
                                if disabled.iter().any(|d| d == &name) {
                                    tracing::info!(plugin = %name, "WASM plugin disabled, skipping");
                                    continue;
                                }
                                // WASM plugins are already initialized during load,
                                // register directly with priority 70 (after storage,
                                // before policy evaluation).
                                kernel.register_plugin(plugin, 70);
                                tracing::info!(plugin = %name, "WASM plugin loaded");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to load WASM plugin");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create WASM plugin loader");
                }
            }
        } else {
            tracing::debug!(dir = %wasm_dir.display(), "WASM plugins directory not found, skipping");
        }
    }

    Ok((kernel, store))
}

/// Open a standalone storage handle for commands that need only storage,
/// not a full kernel (e.g. `db prune`, `jobs list`, `report`).
///
/// Delegates to [`open_store_in`] with the configured data directory.
pub fn open_store(config: &AppConfig) -> Result<Arc<dyn voom_domain::storage::StorageTrait>> {
    open_store_in(&config.data_dir)
}

/// Open a SQLite store rooted at `data_dir`, creating the directory if needed.
///
/// This is the single authoritative place that calls
/// [`voom_sqlite_store::store::SqliteStore::open`].  Both
/// [`bootstrap_kernel_with_store`] (disabled-plugin fallback) and
/// [`open_store`] (store-only commands) go through here so there is never
/// more than one code path for pool creation.
pub(crate) fn open_store_in(
    data_dir: &Path,
) -> Result<Arc<dyn voom_domain::storage::StorageTrait>> {
    // Ensure the directory exists (mirrors what the plugin's init() does).
    std::fs::create_dir_all(data_dir)
        .map_err(|e| anyhow::anyhow!("Failed to create data dir {}: {e}", data_dir.display()))?;
    let db_path = data_dir.join("voom.db");
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

    #[test]
    fn test_bootstrap_kernel_with_store_always_returns_store() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            data_dir: dir.path().to_path_buf(),
            ..AppConfig::default()
        };
        let (_kernel, store) =
            bootstrap_kernel_with_store(&config).expect("bootstrap should succeed");
        // Verify the store is functional
        assert!(store
            .list_files(&voom_domain::FileFilters::default())
            .is_ok());
    }
}
