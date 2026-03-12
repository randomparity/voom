//! Application bootstrap: config loading, plugin initialization, kernel wiring.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use voom_kernel::Kernel;

/// Application configuration loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// Optional Bearer token for authenticating API and SSE requests.
    #[serde(default)]
    pub auth_token: Option<String>,
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
            auth_token: None,
        }
    }
}

/// Base VOOM configuration directory (e.g. `~/.config/voom`).
pub fn voom_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voom")
}

fn default_data_dir() -> PathBuf {
    voom_config_dir()
}

/// Path to the config file.
pub fn config_path() -> PathBuf {
    voom_config_dir().join("config.toml")
}

/// Load config from the default path, or return defaults if not found.
pub fn load_config() -> Result<AppConfig> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config from {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(e) => Err(anyhow::anyhow!("Failed to read config from {}: {e}", path.display())),
    }
}

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
    let mut kernel = Kernel::new();
    let data_dir = &config.data_dir;

    let ctx = voom_kernel::PluginContext {
        config: serde_json::json!({}),
        data_dir: data_dir.clone(),
    };

    // Storage plugin (highest priority — stores everything)
    kernel
        .init_and_register(Box::new(voom_sqlite_store::SqliteStorePlugin::new()), 100, &ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize storage: {e}"))?;

    // Tool detector
    kernel
        .init_and_register(Box::new(voom_tool_detector::ToolDetectorPlugin::new()), 90, &ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize tool detector: {e}"))?;

    // Discovery
    kernel
        .init_and_register(Box::new(voom_discovery::DiscoveryPlugin::new()), 80, &ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize discovery: {e}"))?;

    // Introspector (reads ffprobe_path from ctx.config during init)
    kernel
        .init_and_register(
            Box::new(voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new()),
            70,
            &ctx,
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize introspector: {e}"))?;

    // Policy evaluator
    kernel
        .init_and_register(
            Box::new(voom_policy_evaluator::PolicyEvaluatorPlugin::new()),
            60,
            &ctx,
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize policy evaluator: {e}"))?;

    // Phase orchestrator
    kernel
        .init_and_register(
            Box::new(voom_phase_orchestrator::PhaseOrchestratorPlugin::new()),
            50,
            &ctx,
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize phase orchestrator: {e}"))?;

    // Executors — mkvtoolnix at 39 gets first shot at MKV plans
    kernel
        .init_and_register(
            Box::new(voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new()),
            39,
            &ctx,
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize mkvtoolnix executor: {e}"))?;

    kernel
        .init_and_register(
            Box::new(voom_ffmpeg_executor::FfmpegExecutorPlugin::new()),
            40,
            &ctx,
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize ffmpeg executor: {e}"))?;

    // Backup manager
    kernel
        .init_and_register(Box::new(voom_backup_manager::BackupManagerPlugin::new()), 30, &ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize backup manager: {e}"))?;

    // Job manager
    kernel
        .init_and_register(Box::new(voom_job_manager::JobManagerPlugin::new()), 20, &ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize job manager: {e}"))?;

    // Web server
    kernel
        .init_and_register(Box::new(voom_web_server::WebServerPlugin::new()), 10, &ctx)
        .map_err(|e| anyhow::anyhow!("Failed to initialize web server: {e}"))?;

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
