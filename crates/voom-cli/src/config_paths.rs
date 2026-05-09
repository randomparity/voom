//! Shared filesystem paths for CLI configuration state.

use std::path::PathBuf;

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

/// Path to the config file.
pub fn config_path() -> PathBuf {
    voom_config_dir().join("config.toml")
}
