//! Application configuration: loading, saving, and config types.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Application configuration loaded from TOML.
#[derive(Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// Optional Bearer token for authenticating API and SSE requests.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Per-plugin configuration sections. Each key is a plugin name,
    /// and the value is an arbitrary TOML table passed to the plugin at init.
    /// Example: `[plugin.ffprobe-introspector]` with `ffprobe_path = "/usr/local/bin/ffprobe"`.
    #[serde(default)]
    pub plugin: HashMap<String, toml::Table>,
}

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("data_dir", &self.data_dir)
            .field("plugins", &self.plugins)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("plugin", &self.plugin)
            .finish()
    }
}

impl AppConfig {
    /// Returns the configured ffprobe binary path, if set via
    /// `[plugin.ffprobe-introspector] ffprobe_path = "..."`.
    pub fn ffprobe_path(&self) -> Option<&str> {
        self.plugin
            .get("ffprobe-introspector")
            .and_then(|t| t.get("ffprobe_path"))
            .and_then(|v| v.as_str())
    }
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
            plugin: HashMap::new(),
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
///
/// On Unix, warns if the config file is group- or world-readable, since it
/// may contain API keys or tokens.
pub fn load_config() -> Result<AppConfig> {
    let path = config_path();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                tracing::warn!(
                    path = %path.display(),
                    "config file has loose permissions ({:o}); it may contain API keys — consider: chmod 600 {}",
                    mode & 0o777, path.display()
                );
            }
        }
    }

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
    "mkvtoolnix-executor",
    "ffmpeg-executor",
    "backup-manager",
    "job-manager",
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
# Valid names: sqlite-store, tool-detector, discovery,
#   mkvtoolnix-executor, ffmpeg-executor, backup-manager, job-manager
# disabled_plugins = ["mkvtoolnix-executor"]

# Per-plugin configuration. Use [plugin.<name>] sections to pass
# settings to specific plugins. The section name must match the plugin name.
#
# [plugin.ffprobe-introspector]
# ffprobe_path = "/usr/local/bin/ffprobe"
#
# [plugin.tvdb-metadata]
# api_key = "your-tvdb-api-key"
#
# [plugin.radarr-metadata]
# radarr_url = "http://localhost:7878"
# api_key = "your-radarr-api-key"
"#
    )
}

/// Save config back to the TOML file, creating the directory if needed.
///
/// On Unix, the file is created with mode `0o600` to prevent other users
/// from reading API keys or tokens stored in the config.
pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }
    let toml_str = toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| f.write_all(toml_str.as_bytes()))
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, toml_str)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Default config ───────────────────────────────────────

    #[test]
    fn test_default_config_has_expected_fields() {
        let config = AppConfig::default();
        assert!(config.auth_token.is_none());
        assert!(config.plugins.wasm_dir.is_none());
        assert!(config.plugins.disabled_plugins.is_empty());
    }

    #[test]
    fn test_default_data_dir_ends_with_voom() {
        let config = AppConfig::default();
        assert!(
            config.data_dir.ends_with("voom"),
            "data_dir should end with 'voom', got: {:?}",
            config.data_dir
        );
    }

    // ── Config paths ─────────────────────────────────────────

    #[test]
    fn test_config_path_ends_with_config_toml() {
        let path = config_path();
        assert_eq!(path.file_name().unwrap(), "config.toml");
        assert!(path.parent().unwrap().ends_with("voom"));
    }

    #[test]
    fn test_voom_config_dir_ends_with_voom() {
        let dir = voom_config_dir();
        assert!(
            dir.ends_with("voom"),
            "config dir should end with 'voom', got: {dir:?}"
        );
    }

    // ── TOML serialization round-trip ────────────────────────

    #[test]
    fn test_config_toml_round_trip() {
        let mut plugin_config = HashMap::new();
        let mut table = toml::Table::new();
        table.insert(
            "ffprobe_path".into(),
            toml::Value::String("/usr/local/bin/ffprobe".into()),
        );
        plugin_config.insert("ffprobe-introspector".into(), table);

        let config = AppConfig {
            data_dir: PathBuf::from("/tmp/voom-data"),
            plugins: PluginsConfig {
                wasm_dir: Some(PathBuf::from("/tmp/wasm")),
                disabled_plugins: vec!["web-server".into(), "backup-manager".into()],
            },
            auth_token: Some("secret-token".into()),
            plugin: plugin_config,
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
        assert_eq!(
            loaded.plugin["ffprobe-introspector"]["ffprobe_path"]
                .as_str()
                .unwrap(),
            "/usr/local/bin/ffprobe"
        );
    }

    #[test]
    fn test_empty_toml_gives_defaults() {
        let config: AppConfig = toml::from_str("").expect("empty TOML should parse");
        assert!(config.auth_token.is_none());
        assert!(config.plugins.disabled_plugins.is_empty());
        // data_dir gets the serde default
        assert!(config.data_dir.ends_with("voom"));
    }

    #[test]
    fn test_partial_toml_fills_defaults() {
        let config: AppConfig =
            toml::from_str("auth_token = \"tok123\"").expect("partial TOML should parse");
        assert_eq!(config.auth_token.as_deref(), Some("tok123"));
        assert!(config.plugins.disabled_plugins.is_empty());
    }

    // ── KNOWN_PLUGIN_NAMES ───────────────────────────────────

    #[test]
    fn test_known_plugin_names_contains_expected() {
        assert!(KNOWN_PLUGIN_NAMES.contains(&"sqlite-store"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"mkvtoolnix-executor"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"discovery"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"job-manager"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"ffmpeg-executor"));
        assert!(!KNOWN_PLUGIN_NAMES.contains(&"web-server"));
        assert!(!KNOWN_PLUGIN_NAMES.contains(&"ffprobe-introspector"));
    }

    #[test]
    fn test_known_plugin_names_count() {
        assert_eq!(KNOWN_PLUGIN_NAMES.len(), 7);
    }

    #[test]
    fn test_known_plugin_names_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for name in KNOWN_PLUGIN_NAMES {
            assert!(seen.insert(name), "duplicate plugin name: {name}");
        }
    }

    // ── default_config_contents ───────────────────────────────

    #[test]
    fn test_default_config_contents_is_valid_toml() {
        let contents = default_config_contents();
        // All options are commented out, so parsing should yield defaults
        let config: AppConfig =
            toml::from_str(&contents).expect("default config should be valid TOML");
        assert!(config.auth_token.is_none());
        assert!(config.plugins.wasm_dir.is_none());
        assert!(config.plugins.disabled_plugins.is_empty());
    }

    #[test]
    fn test_default_config_contents_documents_all_fields() {
        let contents = default_config_contents();
        assert!(contents.contains("# data_dir"), "should document data_dir");
        assert!(
            contents.contains("# auth_token"),
            "should document auth_token"
        );
        assert!(contents.contains("# wasm_dir"), "should document wasm_dir");
        assert!(
            contents.contains("# disabled_plugins"),
            "should document disabled_plugins"
        );
        assert!(
            contents.contains("[plugins]"),
            "should have plugins section"
        );
        assert!(
            contents.contains("[plugin."),
            "should document per-plugin config sections"
        );
    }

    // ── load_config with temp files ──────────────────────────

    #[test]
    fn test_load_config_from_valid_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "auth_token = \"test\"\n").unwrap();

        let contents = std::fs::read_to_string(&file).unwrap();
        let config: AppConfig = toml::from_str(&contents).unwrap();
        assert_eq!(config.auth_token.as_deref(), Some("test"));
    }

    #[test]
    fn test_load_config_from_invalid_toml_is_error() {
        let result: Result<AppConfig, _> = toml::from_str("not valid {{{{ toml");
        assert!(result.is_err());
    }

    // ── save_config ──────────────────────────────────────────

    #[test]
    fn test_save_and_reload_config() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");

        let config = AppConfig {
            data_dir: dir.path().to_path_buf(),
            plugins: PluginsConfig {
                wasm_dir: None,
                disabled_plugins: vec!["web-server".into()],
            },
            auth_token: Some("my-token".into()),
            plugin: HashMap::new(),
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&file, &toml_str).unwrap();

        let contents = std::fs::read_to_string(&file).unwrap();
        let reloaded: AppConfig = toml::from_str(&contents).unwrap();
        assert_eq!(reloaded.auth_token.as_deref(), Some("my-token"));
        assert_eq!(reloaded.plugins.disabled_plugins, vec!["web-server"]);
    }

    #[cfg(unix)]
    #[test]
    fn test_save_config_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_file = dir.path().join("config.toml");

        let config = AppConfig {
            data_dir: dir.path().to_path_buf(),
            plugins: PluginsConfig::default(),
            auth_token: Some("secret".into()),
            plugin: HashMap::new(),
        };

        // Write config using the same logic as save_config (can't override config_path(),
        // so replicate the write logic here).
        let toml_str = toml::to_string_pretty(&config).unwrap();
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&config_file)
                .and_then(|mut f| f.write_all(toml_str.as_bytes()))
                .unwrap();
        }

        let meta = std::fs::metadata(&config_file).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "config file should be owner-only (0600), got {mode:o}"
        );
    }

    // ── plugin config ──────────────────────────────────────────

    #[test]
    fn test_plugin_config_toml_roundtrip() {
        let toml_str = r#"
[plugin.tvdb-metadata]
api_key = "abc123"

[plugin.radarr-metadata]
radarr_url = "http://localhost:7878"
api_key = "xyz789"
"#;
        let config: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(
            config.plugin["tvdb-metadata"]["api_key"].as_str().unwrap(),
            "abc123"
        );
        assert_eq!(
            config.plugin["radarr-metadata"]["radarr_url"]
                .as_str()
                .unwrap(),
            "http://localhost:7878"
        );
        assert_eq!(
            config.plugin["radarr-metadata"]["api_key"]
                .as_str()
                .unwrap(),
            "xyz789"
        );
    }

    #[test]
    fn test_plugin_config_empty_by_default() {
        let config: AppConfig = toml::from_str("").expect("parse");
        assert!(config.plugin.is_empty());
    }

    #[test]
    fn test_plugin_config_partial() {
        let toml_str = r#"
[plugin.tvdb-metadata]
api_key = "abc123"
"#;
        let config: AppConfig = toml::from_str(toml_str).expect("parse");
        assert!(config.plugin.contains_key("tvdb-metadata"));
        assert!(!config.plugin.contains_key("ffprobe-introspector"));

        // Unconfigured plugin gets empty json
        let unconfigured = config
            .plugin
            .get("ffprobe-introspector")
            .map(|t| serde_json::to_value(t).unwrap_or_default())
            .unwrap_or_else(|| serde_json::json!({}));
        assert_eq!(unconfigured, serde_json::json!({}));
    }
}
