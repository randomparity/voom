//! Application bootstrap: config loading, plugin initialization, kernel wiring.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use voom_kernel::{Kernel, Plugin};

/// Application configuration loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub plugins: PluginsConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginsConfig {
    #[serde(default)]
    pub wasm_dir: Option<PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            plugins: PluginsConfig::default(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voom")
}

/// Path to the config file.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voom")
        .join("config.toml")
}

/// Load config from the default path, or return defaults if not found.
pub fn load_config() -> Result<AppConfig> {
    let path = config_path();
    if path.exists() {
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config from {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config from {}", path.display()))
    } else {
        Ok(AppConfig::default())
    }
}

/// Bootstrap a kernel with all native plugins registered.
pub fn bootstrap_kernel(config: &AppConfig) -> Result<Kernel> {
    let mut kernel = Kernel::new();
    let data_dir = &config.data_dir;

    // Storage plugin (highest priority — stores everything)
    let mut store = voom_sqlite_store::SqliteStorePlugin::new();
    let store_ctx = voom_kernel::PluginContext {
        config: serde_json::json!({}),
        data_dir: data_dir.clone(),
    };
    store
        .init(&store_ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize storage: {e}"))?;
    kernel.register_plugin(Arc::new(store), 100);

    // Tool detector
    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();
    kernel.register_plugin(Arc::new(detector), 90);

    // Discovery
    kernel.register_plugin(Arc::new(voom_discovery::DiscoveryPlugin::new()), 80);

    // Introspector
    kernel.register_plugin(
        Arc::new(voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new()),
        70,
    );

    // Policy evaluator
    kernel.register_plugin(
        Arc::new(voom_policy_evaluator::PolicyEvaluatorPlugin::new()),
        60,
    );

    // Phase orchestrator
    kernel.register_plugin(
        Arc::new(voom_phase_orchestrator::PhaseOrchestratorPlugin::new()),
        50,
    );

    // Executors
    kernel.register_plugin(
        Arc::new(voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new()),
        40,
    );
    kernel.register_plugin(
        Arc::new(voom_ffmpeg_executor::FfmpegExecutorPlugin::new()),
        40,
    );

    // Backup manager
    kernel.register_plugin(
        Arc::new(voom_backup_manager::BackupManagerPlugin::new()),
        30,
    );

    // Job manager
    kernel.register_plugin(
        Arc::new(voom_job_manager::JobManagerPlugin::new()),
        20,
    );

    // Web server
    kernel.register_plugin(
        Arc::new(voom_web_server::WebServerPlugin::new()),
        10,
    );

    Ok(kernel)
}

/// Open a storage handle using the configured data directory.
/// Opens a second connection (SQLite WAL mode supports concurrent readers).
pub fn open_store(config: &AppConfig) -> Result<Arc<voom_sqlite_store::store::SqliteStore>> {
    let db_path = config.data_dir.join("voom.db");
    let store = voom_sqlite_store::store::SqliteStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open store: {e}"))?;
    Ok(Arc::new(store))
}
