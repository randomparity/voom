//! Configuration for the verifier plugin, read from
//! `[plugin.verifier]` in `config.toml`.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VerifierConfig {
    /// Timeout in seconds for the ffprobe header check (quick mode).
    pub quick_timeout_secs: u64,
    /// Multiplier of file duration for the thorough-mode timeout.
    pub thorough_timeout_multiplier: f32,
    /// Absolute floor for the thorough-mode timeout.
    pub thorough_timeout_floor_secs: u64,
    /// Where to move quarantined files. Required when `on_error: quarantine`
    /// is referenced; if unset, the quarantine action fails loudly at runtime.
    pub quarantine_dir: Option<PathBuf>,
    /// Path to ffprobe (defaults to "ffprobe" on PATH).
    pub ffprobe_path: String,
    /// Path to ffmpeg (defaults to "ffmpeg" on PATH).
    pub ffmpeg_path: String,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            quick_timeout_secs: 30,
            thorough_timeout_multiplier: 4.0,
            thorough_timeout_floor_secs: 60,
            quarantine_dir: None,
            ffprobe_path: "ffprobe".into(),
            ffmpeg_path: "ffmpeg".into(),
        }
    }
}
