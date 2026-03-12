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
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
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
        Err(e) => Err(anyhow::anyhow!(
            "Failed to read config from {}: {e}",
            path.display()
        )),
    }
}

/// All known native plugin names (used for validation in enable/disable commands).
pub const KNOWN_PLUGIN_NAMES: &[&str] = &[
    "sqlite-store",
    "tool-detector",
    "discovery",
    "ffprobe-introspector",
    "policy-evaluator",
    "phase-orchestrator",
    "mkvtoolnix-executor",
    "ffmpeg-executor",
    "backup-manager",
    "job-manager",
    "web-server",
];

/// Save config back to the TOML file, creating the directory if needed.
pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }
    let toml_str = toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;
    std::fs::write(&path, toml_str)
        .with_context(|| format!("Failed to write config to {}", path.display()))?;
    Ok(())
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

    let disabled = &config.plugins.disabled_plugins;

    // Helper macro to conditionally register a plugin (skips if disabled)
    macro_rules! register_if_enabled {
        ($name:expr, $plugin:expr, $priority:expr, $label:expr) => {
            if !disabled.iter().any(|d| d == $name) {
                kernel
                    .init_and_register(Box::new($plugin), $priority, &ctx)
                    .map_err(|e| anyhow::anyhow!("Failed to initialize {}: {e}", $label))?;
            }
        };
    }

    // Storage plugin (highest priority — stores everything)
    register_if_enabled!(
        "sqlite-store",
        voom_sqlite_store::SqliteStorePlugin::new(),
        100,
        "storage"
    );

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
