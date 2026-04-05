//! Application bootstrap: plugin initialization and kernel wiring.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use voom_kernel::{Kernel, Plugin};

use crate::capability_collector::CapabilityCollectorPlugin;
use crate::config::AppConfig;

/// Return type for [`bootstrap_kernel_with_store`].
pub struct BootstrapResult {
    pub kernel: Kernel,
    pub store: Arc<dyn voom_domain::storage::StorageTrait>,
    pub job_queue: Arc<voom_job_manager::queue::JobQueue>,
    pub collector: Arc<CapabilityCollectorPlugin>,
}

// Plugin priority scheme (lower number = runs first during event dispatch).
// mkvtoolnix at 39 runs before ffmpeg at 40 so it gets first crack at
// MKV-specific plans (metadata, convert-to-MKV).
const PRIORITY_BUS_TRACER: i32 = 1;
const PRIORITY_STORAGE: i32 = 100;
const PRIORITY_HEALTH_CHECKER: i32 = 95;
const PRIORITY_TOOL_DETECTOR: i32 = 90;
const PRIORITY_DISCOVERY: i32 = 80;
const PRIORITY_FFPROBE_INTROSPECTOR: i32 = 60;
const PRIORITY_FFMPEG_EXECUTOR: i32 = 40;
const PRIORITY_MKVTOOLNIX_EXECUTOR: i32 = 39;
const PRIORITY_CAPABILITY_COLLECTOR: i32 = 35;
const PRIORITY_BACKUP_MANAGER: i32 = 30;
const PRIORITY_JOB_MANAGER: i32 = 20;

/// Bootstrap a kernel with all native plugins registered.
///
/// All plugins go through `init_and_register` for consistent lifecycle management.
pub fn bootstrap_kernel(config: &AppConfig) -> Result<Kernel> {
    let result = bootstrap_kernel_with_store(config)?;
    Ok(result.kernel)
}

/// Bootstrap the kernel with all native plugins and return the
/// kernel, storage handle, and shared job queue.
///
/// The store is always returned (not `Option`): if the sqlite-store
/// plugin is enabled its handle is reused so there is no second
/// pool; if the plugin is disabled a standalone pool is opened via
/// [`open_store_in`].
///
/// # Blocking
///
/// This function performs synchronous I/O (filesystem checks, SQLite
/// pool creation, plugin init) and must NOT be called from an async
/// context. Callers should invoke it before entering the tokio
/// runtime or from within `spawn_blocking`.
pub fn bootstrap_kernel_with_store(config: &AppConfig) -> Result<BootstrapResult> {
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
                let ctx = voom_kernel::PluginContext::new(plugin_json($name), data_dir.clone());
                kernel
                    .init_and_register(Arc::new($plugin), $priority, &ctx)
                    .with_context(|| format!("Failed to initialize {}", $label))?;
            }
        };
        ($name:expr, $plugin:expr, $priority:expr, $label:expr, $ctx:expr) => {
            if !disabled.iter().any(|d| d == $name) {
                kernel
                    .init_and_register(Arc::new($plugin), $priority, $ctx)
                    .with_context(|| format!("Failed to initialize {}", $label))?;
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
            let ctx =
                voom_kernel::PluginContext::new(plugin_json("sqlite-store"), data_dir.clone());
            let init_events = plugin.init(&ctx).context("Failed to initialize storage")?;

            // Capture the store handle before moving the plugin into an Arc.
            // `plugin.store()` is always Some after a successful init().
            let handle = plugin
                .store()
                .map(|s| Arc::clone(s) as Arc<dyn voom_domain::storage::StorageTrait>)
                .expect("store is Some after successful init");

            let plugin_arc: Arc<dyn voom_kernel::Plugin> = Arc::new(plugin);
            kernel.register_plugin(plugin_arc, PRIORITY_STORAGE)?;

            // Dispatch init events after registration so bus subscribers can
            // see them (e.g. health status events from other init'd plugins).
            for event in init_events {
                kernel.dispatch(event);
            }

            handle
        } else {
            // sqlite-store disabled: open a standalone pool so callers always
            // get a usable handle.  No plugin is registered, so events will
            // not be persisted, but read-only CLI commands still work.
            open_store_in(data_dir).context("Failed to open storage")?
        };

    // Create a shared job queue for plugins that need to enqueue work.
    let job_queue = Arc::new(voom_job_manager::queue::JobQueue::new(store.clone()));

    // Bus tracer — priority 1 (first to see events, before any state changes).
    register_if_enabled!(
        "bus-tracer",
        voom_bus_tracer::BusTracerPlugin::new(),
        PRIORITY_BUS_TRACER,
        "bus tracer"
    );

    // Health checker
    register_if_enabled!(
        "health-checker",
        voom_health_checker::HealthCheckerPlugin::new(),
        PRIORITY_HEALTH_CHECKER,
        "health checker"
    );

    // Halt early if the data directory is not writable — this is a critical
    // prerequisite for sqlite-store, backups, and job persistence.
    if !disabled.iter().any(|d| d == "health-checker") {
        let probe = data_dir.join(".voom-health-probe");
        if std::fs::write(&probe, b"probe").is_err() {
            tracing::error!(
                data_dir = %data_dir.display(),
                "data directory is not writable; aborting bootstrap"
            );
            anyhow::bail!(
                "data directory {} is not writable — check permissions",
                data_dir.display()
            );
        }
        let _ = std::fs::remove_file(&probe);
    }

    // Tool detector
    register_if_enabled!(
        "tool-detector",
        voom_tool_detector::ToolDetectorPlugin::new(),
        PRIORITY_TOOL_DETECTOR,
        "tool detector"
    );

    // Discovery
    register_if_enabled!(
        "discovery",
        voom_discovery::DiscoveryPlugin::new(),
        PRIORITY_DISCOVERY,
        "discovery"
    );

    // FFprobe introspector — subscribes to FileDiscovered, emits JobEnqueueRequested.
    // Registered after sqlite-store (100) so discovered files are persisted first.
    register_if_enabled!(
        "ffprobe-introspector",
        voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new(),
        PRIORITY_FFPROBE_INTROSPECTOR,
        "ffprobe introspector"
    );

    // Capability collector — captures ExecutorCapabilities events for the evaluator.
    // Registered before executors so it sees their init-time announcements.
    // Uses manual init + register_plugin (like sqlite-store) because the caller
    // needs an Arc<CapabilityCollectorPlugin> handle for snapshot() after bootstrap.
    let collector = {
        let mut plugin = CapabilityCollectorPlugin::new();
        let ctx =
            voom_kernel::PluginContext::new(plugin_json("capability-collector"), data_dir.clone());
        plugin
            .init(&ctx)
            .context("Failed to initialize capability collector")?;
        Arc::new(plugin)
    };
    kernel.register_plugin(collector.clone(), PRIORITY_CAPABILITY_COLLECTOR)?;

    // Executor — mkvtoolnix (MKV metadata, track removal/reorder, convert-to-MKV)
    register_if_enabled!(
        "mkvtoolnix-executor",
        voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new(),
        PRIORITY_MKVTOOLNIX_EXECUTOR,
        "mkvtoolnix executor"
    );

    // Executor — ffmpeg (transcode, non-MKV metadata, container conversion)
    register_if_enabled!(
        "ffmpeg-executor",
        voom_ffmpeg_executor::FfmpegExecutorPlugin::new(),
        PRIORITY_FFMPEG_EXECUTOR,
        "ffmpeg executor"
    );

    // Backup manager
    register_if_enabled!(
        "backup-manager",
        voom_backup_manager::BackupManagerPlugin::new(),
        PRIORITY_BACKUP_MANAGER,
        "backup manager"
    );

    // Job manager — receives the shared job queue via PluginContext so it can
    // handle JobEnqueueRequested events from other plugins.
    {
        let mut ctx = voom_kernel::PluginContext::new(plugin_json("job-manager"), data_dir.clone());
        ctx.register_resource(job_queue.clone());
        register_if_enabled!(
            "job-manager",
            voom_job_manager::JobManagerPlugin::new(),
            PRIORITY_JOB_MANAGER,
            "job manager",
            &ctx
        );
    }

    #[cfg(feature = "wasm")]
    load_wasm_plugins(&mut kernel, config, disabled, store.clone())?;

    Ok(BootstrapResult {
        kernel,
        store,
        job_queue,
        collector,
    })
}

/// Load WASM plugins from the configured directory into the kernel.
#[cfg(feature = "wasm")]
fn load_wasm_plugins(
    kernel: &mut Kernel,
    config: &AppConfig,
    disabled: &[String],
    store: Arc<dyn voom_domain::storage::StorageTrait>,
) -> Result<()> {
    let wasm_dir = config
        .plugins
        .wasm_dir
        .clone()
        .unwrap_or_else(|| config.data_dir.join("plugins").join("wasm"));

    if !wasm_dir.is_dir() {
        tracing::debug!(
            dir = %wasm_dir.display(),
            "WASM plugins directory not found, skipping"
        );
        return Ok(());
    }

    let loader = match voom_kernel::loader::wasm::WasmPluginLoader::new() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create WASM plugin loader");
            return Ok(());
        }
    };

    let skip_set: std::collections::HashSet<String> = disabled.iter().cloned().collect();
    let results =
        loader.load_dir_with_config_skip(&wasm_dir, &config.plugin, &skip_set, Some(store));
    for result in results {
        match result {
            Ok((plugin, priority)) => {
                let name = plugin.name().to_string();
                kernel.register_plugin(plugin, priority)?;
                tracing::info!(plugin = %name, priority, "WASM plugin loaded");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load WASM plugin");
            }
        }
    }

    Ok(())
}

/// Open a standalone storage handle for commands that need only storage,
/// not a full kernel (e.g. `db prune`, `jobs list`, `report`).
///
/// Delegates to [`open_store_in`] with the configured data directory.
pub fn open_store(config: &AppConfig) -> Result<Arc<dyn voom_domain::storage::StorageTrait>> {
    open_store_in(&config.data_dir)
}

/// Open a `SQLite` store rooted at `data_dir`, creating the directory if needed.
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
        .with_context(|| format!("Failed to create data dir {}", data_dir.display()))?;
    let db_path = data_dir.join("voom.db");
    let store =
        voom_sqlite_store::store::SqliteStore::open(&db_path).context("Failed to open store")?;
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
        let result =
            bootstrap_kernel_with_store(&config).expect("bootstrap should succeed with defaults");
        let registered = result.kernel.registry.plugin_names();
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
        let result = bootstrap_kernel_with_store(&config).expect("bootstrap should succeed");
        // Verify the store is functional
        assert!(result
            .store
            .list_files(&voom_domain::FileFilters::default())
            .is_ok());
    }
}
