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

/// Generate a default config.toml with all options commented out and documented.
pub fn default_config_contents() -> String {
    let data_dir = default_data_dir();
    let data_dir_str = data_dir.display();

    format!(
        r#"# VOOM configuration file
# See https://github.com/randomparity/voom for documentation.

# Directory where VOOM stores its database and plugin data.
# data_dir = "{data_dir_str}"

# Optional bearer token for authenticating REST API and SSE requests.
# When set, all API requests must include an "Authorization: Bearer <token>" header.
# auth_token = "your-secret-token"

[plugins]

# Directory containing WASM plugin files (.wasm).
# Defaults to the config directory if not set.
# wasm_dir = "{data_dir_str}/plugins/wasm"

# List of plugin names to disable at startup.
# Valid names: sqlite-store, tool-detector, discovery, ffprobe-introspector,
#   policy-evaluator, phase-orchestrator, mkvtoolnix-executor, ffmpeg-executor,
#   backup-manager, job-manager, web-server
# disabled_plugins = ["web-server"]
"#
    )
}

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
                    .init_and_register(Arc::new($plugin), $priority, &ctx)
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
/// Opens a second connection (`SQLite` WAL mode supports concurrent readers).
pub fn open_store(config: &AppConfig) -> Result<Arc<voom_sqlite_store::store::SqliteStore>> {
    let db_path = config.data_dir.join("voom.db");
    let store = voom_sqlite_store::store::SqliteStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open store: {e}"))?;
    Ok(Arc::new(store))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Default config ───────────────────────────────────────

    #[test]
    fn default_config_has_expected_fields() {
        let config = AppConfig::default();
        assert!(config.auth_token.is_none());
        assert!(config.plugins.wasm_dir.is_none());
        assert!(config.plugins.disabled_plugins.is_empty());
    }

    #[test]
    fn default_data_dir_ends_with_voom() {
        let config = AppConfig::default();
        assert!(
            config.data_dir.ends_with("voom"),
            "data_dir should end with 'voom', got: {:?}",
            config.data_dir
        );
    }

    // ── Config paths ─────────────────────────────────────────

    #[test]
    fn config_path_ends_with_config_toml() {
        let path = config_path();
        assert_eq!(path.file_name().unwrap(), "config.toml");
        assert!(path.parent().unwrap().ends_with("voom"));
    }

    #[test]
    fn voom_config_dir_ends_with_voom() {
        let dir = voom_config_dir();
        assert!(
            dir.ends_with("voom"),
            "config dir should end with 'voom', got: {:?}",
            dir
        );
    }

    // ── TOML serialization round-trip ────────────────────────

    #[test]
    fn config_toml_round_trip() {
        let config = AppConfig {
            data_dir: PathBuf::from("/tmp/voom-data"),
            plugins: PluginsConfig {
                wasm_dir: Some(PathBuf::from("/tmp/wasm")),
                disabled_plugins: vec!["web-server".into(), "backup-manager".into()],
            },
            auth_token: Some("secret-token".into()),
        };

        let toml_str = toml::to_string_pretty(&config).expect("serialize");
        let loaded: AppConfig = toml::from_str(&toml_str).expect("deserialize");

        assert_eq!(loaded.data_dir, config.data_dir);
        assert_eq!(loaded.plugins.wasm_dir, config.plugins.wasm_dir);
        assert_eq!(
            loaded.plugins.disabled_plugins,
            config.plugins.disabled_plugins
        );
        assert_eq!(loaded.auth_token, config.auth_token);
    }

    #[test]
    fn empty_toml_gives_defaults() {
        let config: AppConfig = toml::from_str("").expect("empty TOML should parse");
        assert!(config.auth_token.is_none());
        assert!(config.plugins.disabled_plugins.is_empty());
        // data_dir gets the serde default
        assert!(config.data_dir.ends_with("voom"));
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let config: AppConfig =
            toml::from_str("auth_token = \"tok123\"").expect("partial TOML should parse");
        assert_eq!(config.auth_token.as_deref(), Some("tok123"));
        assert!(config.plugins.disabled_plugins.is_empty());
    }

    // ── KNOWN_PLUGIN_NAMES ───────────────────────────────────

    #[test]
    fn known_plugin_names_contains_expected() {
        assert!(KNOWN_PLUGIN_NAMES.contains(&"sqlite-store"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"ffmpeg-executor"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"web-server"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"discovery"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"job-manager"));
    }

    #[test]
    fn known_plugin_names_count() {
        assert_eq!(KNOWN_PLUGIN_NAMES.len(), 11);
    }

    #[test]
    fn known_plugin_names_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for name in KNOWN_PLUGIN_NAMES {
            assert!(seen.insert(name), "duplicate plugin name: {name}");
        }
    }

    // ── default_config_contents ───────────────────────────────

    #[test]
    fn default_config_contents_is_valid_toml() {
        let contents = default_config_contents();
        // All options are commented out, so parsing should yield defaults
        let config: AppConfig = toml::from_str(&contents).expect("default config should be valid TOML");
        assert!(config.auth_token.is_none());
        assert!(config.plugins.wasm_dir.is_none());
        assert!(config.plugins.disabled_plugins.is_empty());
    }

    #[test]
    fn default_config_contents_documents_all_fields() {
        let contents = default_config_contents();
        assert!(contents.contains("# data_dir"), "should document data_dir");
        assert!(contents.contains("# auth_token"), "should document auth_token");
        assert!(contents.contains("# wasm_dir"), "should document wasm_dir");
        assert!(
            contents.contains("# disabled_plugins"),
            "should document disabled_plugins"
        );
        assert!(contents.contains("[plugins]"), "should have plugins section");
    }

    // ── load_config with temp files ──────────────────────────

    #[test]
    fn load_config_from_valid_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "auth_token = \"test\"\n").unwrap();

        let contents = std::fs::read_to_string(&file).unwrap();
        let config: AppConfig = toml::from_str(&contents).unwrap();
        assert_eq!(config.auth_token.as_deref(), Some("test"));
    }

    #[test]
    fn load_config_from_invalid_toml_is_error() {
        let result: Result<AppConfig, _> = toml::from_str("not valid {{{{ toml");
        assert!(result.is_err());
    }

    // ── save_config ──────────────────────────────────────────

    #[test]
    fn save_and_reload_config() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");

        let config = AppConfig {
            data_dir: dir.path().to_path_buf(),
            plugins: PluginsConfig {
                wasm_dir: None,
                disabled_plugins: vec!["web-server".into()],
            },
            auth_token: Some("my-token".into()),
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&file, &toml_str).unwrap();

        let contents = std::fs::read_to_string(&file).unwrap();
        let reloaded: AppConfig = toml::from_str(&contents).unwrap();
        assert_eq!(reloaded.auth_token.as_deref(), Some("my-token"));
        assert_eq!(reloaded.plugins.disabled_plugins, vec!["web-server"]);
    }

    // ── open_store ───────────────────────────────────────────

    #[test]
    fn open_store_creates_db_in_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            data_dir: dir.path().to_path_buf(),
            plugins: PluginsConfig::default(),
            auth_token: None,
        };

        let store = open_store(&config);
        assert!(store.is_ok());
        assert!(dir.path().join("voom.db").exists());
    }
}
