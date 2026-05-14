//! Application configuration: loading, saving, and config types.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::policy_map::MappingEntry;

/// How to resolve orphaned backups discovered at startup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryMode {
    /// Restore the original file from the backup (default).
    #[default]
    AlwaysRecover,
    /// Discard the backup and keep the partially-modified file.
    AlwaysDiscard,
}

/// Crash recovery configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecoveryConfig {
    #[serde(default)]
    pub mode: RecoveryMode,
}

/// Missing file pruning configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PruningConfig {
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
        }
    }
}

fn default_retention_days() -> u32 {
    30
}

/// Per-table retention bounds. Either field may be `Some(0)` to disable that
/// bound; both being `Some(0)` (or both `None`) disables retention for the table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableRetention {
    pub keep_for_days: Option<u32>,
    pub keep_last: Option<u64>,
}

impl TableRetention {
    fn for_jobs() -> Self {
        Self {
            keep_for_days: Some(7),
            keep_last: Some(50_000),
        }
    }
    fn for_event_log() -> Self {
        // event_log carries ~7 rows per process job (file.discovered,
        // file.introspected, three plan.* events for transformed files, and
        // job.started/completed). To preserve at least one full
        // `jobs.keep_last` bucket of activity, keep_last must outlive jobs
        // by roughly that ratio. See issue #194 for the diagnosis.
        Self {
            keep_for_days: Some(60),
            keep_last: Some(500_000),
        }
    }
    fn for_file_transitions() -> Self {
        Self {
            keep_for_days: Some(90),
            keep_last: Some(500_000),
        }
    }
    fn for_plugin_stats() -> Self {
        Self {
            keep_for_days: Some(30),
            keep_last: Some(100_000),
        }
    }
}

/// Top-level retention configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetentionConfig {
    #[serde(default = "default_schedule_interval_minutes")]
    pub schedule_interval_minutes: u32,
    #[serde(default = "default_run_after_cli")]
    pub run_after_cli: bool,
    #[serde(default = "TableRetention::for_jobs")]
    pub jobs: TableRetention,
    #[serde(default = "TableRetention::for_event_log")]
    pub event_log: TableRetention,
    #[serde(default = "TableRetention::for_file_transitions")]
    pub file_transitions: TableRetention,
    #[serde(default = "TableRetention::for_plugin_stats")]
    pub plugin_stats: TableRetention,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            schedule_interval_minutes: default_schedule_interval_minutes(),
            run_after_cli: default_run_after_cli(),
            jobs: TableRetention::for_jobs(),
            event_log: TableRetention::for_event_log(),
            file_transitions: TableRetention::for_file_transitions(),
            plugin_stats: TableRetention::for_plugin_stats(),
        }
    }
}

fn default_schedule_interval_minutes() -> u32 {
    60
}
fn default_run_after_cli() -> bool {
    true
}

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
    /// Default policy file applied to files that match no prefix in `policy_mapping`.
    #[serde(default)]
    pub default_policy: Option<String>,
    /// Per-directory policy mappings: longest matching prefix wins.
    #[serde(default)]
    pub policy_mapping: Vec<MappingEntry>,
    #[serde(default)]
    pub recovery: RecoveryConfig,
    #[serde(default)]
    pub pruning: PruningConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
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
            .field("default_policy", &self.default_policy)
            .field("policy_mapping", &self.policy_mapping)
            .field("recovery", &self.recovery)
            .field("pruning", &self.pruning)
            .field("retention", &self.retention)
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

    /// Returns configured animation detection mode for ffprobe introspection.
    pub fn animation_detection_mode(
        &self,
    ) -> voom_ffprobe_introspector::parser::AnimationDetectionMode {
        self.plugin
            .get("ffprobe-introspector")
            .and_then(|t| t.get("detect_animation"))
            .and_then(|v| v.as_str())
            .and_then(voom_ffprobe_introspector::parser::AnimationDetectionMode::parse)
            .unwrap_or_default()
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
            default_policy: None,
            policy_mapping: Vec::new(),
            recovery: RecoveryConfig::default(),
            pruning: PruningConfig::default(),
            retention: RetentionConfig::default(),
        }
    }
}

/// Base VOOM configuration directory (e.g. `~/.config/voom`).
///
/// Respects `XDG_CONFIG_HOME` when set, falling back to the
/// platform default via `dirs::config_dir()`.
pub fn voom_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(dirs::config_dir)
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

/// Path to the default policies directory.
pub fn policies_dir() -> PathBuf {
    voom_config_dir().join("policies")
}

/// Resolve a policy path: use as-is if it exists, otherwise check the
/// default policies directory. Returns the original path unchanged if
/// neither location has the file (so the caller produces a normal
/// "not found" error).
pub fn resolve_policy_path(path: &std::path::Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    // Only fall back for bare filenames (no directory component).
    if path.parent().is_some_and(|p| p != std::path::Path::new("")) {
        return path.to_path_buf();
    }
    let candidate = policies_dir().join(path);
    if candidate.exists() {
        return candidate;
    }
    path.to_path_buf()
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
    "bus-tracer",
    "capability-collector",
    "sqlite-store",
    "health-checker",
    "tool-detector",
    "discovery",
    "ffprobe-introspector",
    "mkvtoolnix-executor",
    "ffmpeg-executor",
    "backup-manager",
    "job-manager",
    "report",
    "verifier",
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
# Generate a strong token (≥32 chars): openssl rand -base64 32
# auth_token = "your-secret-token"

[plugins]

# Directory containing WASM plugin files (.wasm).
# Defaults to the config directory if not set.
# wasm_dir = "{data_dir_str}/plugins/wasm"

# List of plugin names to disable at startup.
# Valid names: sqlite-store, tool-detector, discovery,
#   mkvtoolnix-executor, ffmpeg-executor, backup-manager, job-manager
# disabled_plugins = ["mkvtoolnix-executor"]

# Default policy file applied when no --policy or --policy-map flag is given.
# Files not matching any [[policy_mapping]] prefix use this policy.
# Set to "skip" to skip unmatched files instead.
# default_policy = "standard.voom"

# Per-directory policy mappings. Longest matching prefix wins.
# Each entry needs either `policy` or `skip = true`.
# Prefixes are relative to the path argument of `voom process`.
#
# [[policy_mapping]]
# prefix = "best-hd"
# policy = "high-quality.voom"
#
# [[policy_mapping]]
# prefix = "test-bad"
# skip = true

# Per-plugin configuration. Use [plugin.<name>] sections to pass
# settings to specific plugins. The section name must match the plugin name.
#
# [plugin.ffprobe-introspector]
# ffprobe_path = "/usr/local/bin/ffprobe"
# detect_animation = "metadata-only"  # off | metadata-only | heuristic
#
# [plugin.ffmpeg-executor]
# # Override detected HW accel backend. Values: nvenc, qsv, vaapi, videotoolbox, none.
# # Omit to auto-detect.
# # hw_accel = "vaapi"
# # GPU device for HW encoding. NVIDIA: "0", "1". VA-API/QSV: "/dev/dri/renderD128".
# # Omit to use system default.
# # gpu_device = "0"
# # Maximum simultaneous NVENC encode sessions. Default: 2.
# # Increase cautiously on large GPUs after validating an e2e run.
# # Applies only when NVENC is active; 0 uses the default.
# # nvenc_max_parallel = 2
# # Opportunistically use hardware decode when a matching decoder was probed.
# # Disable this if a GPU driver or source profile fails with hwaccel enabled.
# # Defaults to true.
# # hw_decode = false
#
# [plugin.tvdb-metadata]
# api_key = "your-tvdb-api-key"
#
# [plugin.radarr-metadata]
# radarr_url = "http://localhost:7878"
# api_key = "your-radarr-api-key"

# Crash recovery: what to do with orphaned backups from interrupted executions.
# mode = "always_recover" | "always_discard"
[recovery]
mode = "always_recover"

# Missing file pruning: how long to keep records for files no longer on disk.
[pruning]
retention_days = 30

# Database retention. Bounds the size of jobs, event_log, and file_transitions
# to keep the database from growing forever. Set keep_for_days = 0 and keep_last = 0
# for any table to disable retention for that table.
#
# Invariant: event_log must outlive jobs by ~10x on both axes. Each job row
# produces ~7 event_log rows (file.discovered, file.introspected, three
# plan.* events, job.started, job.completed). If event_log is pruned while
# jobs are not, `voom events` and SSE history will undercount completed
# work. `voom env check` reports when this invariant is violated.
# See issue #194.
[retention]
# How often the periodic prune runs in `serve` mode (minutes).
schedule_interval_minutes = 60
# Whether `voom scan` and `voom process` run a single prune pass after they finish.
run_after_cli = true

[retention.jobs]
keep_for_days = 7
keep_last = 50000

[retention.event_log]
# Must outlive [retention.jobs] by ~10x: event_log carries ~7 rows per
# process job. See issue #194.
keep_for_days = 60
keep_last = 500000

[retention.file_transitions]
keep_for_days = 90
keep_last = 500000
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
    use std::path::Path;

    use super::*;

    // ── Default config ───────────────────────────────────────

    #[test]
    fn test_default_recovery_mode_is_always_recover() {
        let config = AppConfig::default();
        assert_eq!(
            config.recovery.mode,
            RecoveryMode::AlwaysRecover,
            "default recovery mode should be AlwaysRecover, not AlwaysDiscard"
        );
    }

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
            ..Default::default()
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
        assert!(KNOWN_PLUGIN_NAMES.contains(&"ffprobe-introspector"));
        assert!(KNOWN_PLUGIN_NAMES.contains(&"bus-tracer"));
        assert!(!KNOWN_PLUGIN_NAMES.contains(&"web-server"));
    }

    #[test]
    fn test_known_plugin_names_count() {
        assert_eq!(KNOWN_PLUGIN_NAMES.len(), 13);
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
        assert!(
            contents.contains("# default_policy"),
            "should document default_policy"
        );
        assert!(
            contents.contains("[[policy_mapping]]"),
            "should document policy_mapping"
        );
        assert!(
            contents.contains("[recovery]"),
            "should have recovery section"
        );
        assert!(
            contents.contains("[pruning]"),
            "should have pruning section"
        );
        assert!(
            contents.contains("[retention]"),
            "should have retention section"
        );
        assert!(contents.contains("[retention.jobs]"));
        assert!(contents.contains("[retention.event_log]"));
        assert!(contents.contains("[retention.file_transitions]"));
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
            ..Default::default()
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
            ..Default::default()
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

    // ── resolve_policy_path ──────────────────────────────────

    #[test]
    fn test_resolve_policy_path_existing_file_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("local.voom");
        std::fs::write(&file, "").unwrap();

        let resolved = resolve_policy_path(&file);
        assert_eq!(resolved, file);
    }

    #[test]
    fn test_resolve_policy_path_falls_back_to_policies_dir() {
        // Create a file in the policies dir
        let pdir = policies_dir();
        std::fs::create_dir_all(&pdir).ok();
        let policy_file = pdir.join("_test_resolve_fallback.voom");
        std::fs::write(&policy_file, "").unwrap();

        let resolved = resolve_policy_path(Path::new("_test_resolve_fallback.voom"));
        assert_eq!(resolved, policy_file);

        // Cleanup
        std::fs::remove_file(&policy_file).ok();
    }

    #[test]
    fn test_resolve_policy_path_no_fallback_for_paths_with_dirs() {
        let resolved = resolve_policy_path(Path::new("subdir/missing.voom"));
        assert_eq!(resolved, Path::new("subdir/missing.voom"));
    }

    #[test]
    fn test_resolve_policy_path_returns_original_when_not_found() {
        let resolved = resolve_policy_path(Path::new("nonexistent_xyzzy.voom"));
        assert_eq!(resolved, Path::new("nonexistent_xyzzy.voom"));
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
        let unconfigured = config.plugin.get("ffprobe-introspector").map_or_else(
            || serde_json::json!({}),
            |t| serde_json::to_value(t).unwrap_or_default(),
        );
        assert_eq!(unconfigured, serde_json::json!({}));
    }

    #[test]
    fn retention_config_roundtrips() {
        let toml_str = r#"
[retention]
schedule_interval_minutes = 30
run_after_cli = false

[retention.jobs]
keep_for_days = 14
keep_last = 100000
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.retention.schedule_interval_minutes, 30);
        assert!(!cfg.retention.run_after_cli);
        assert_eq!(cfg.retention.jobs.keep_for_days, Some(14));
        assert_eq!(cfg.retention.jobs.keep_last, Some(100_000));
        // Tables not listed get the type defaults
        assert_eq!(cfg.retention.event_log.keep_for_days, Some(60));
        assert_eq!(cfg.retention.file_transitions.keep_for_days, Some(90));
    }

    #[test]
    fn retention_defaults_when_absent() {
        let cfg: AppConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.retention.schedule_interval_minutes, 60);
        assert!(cfg.retention.run_after_cli);
        assert_eq!(cfg.retention.jobs.keep_for_days, Some(7));
        assert_eq!(cfg.retention.jobs.keep_last, Some(50_000));
        // event_log must outlive jobs by ~10x to cover all events emitted by
        // a full jobs.keep_last bucket of process jobs (~7 events each).
        // See issue #194.
        assert_eq!(cfg.retention.event_log.keep_for_days, Some(60));
        assert_eq!(cfg.retention.event_log.keep_last, Some(500_000));
    }
}
