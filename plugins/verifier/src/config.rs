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
    /// Hardware-accelerated decode mode for thorough verification.
    /// Recognised: `none` (default — CPU), `auto`, `nvdec`, `vaapi`,
    /// `qsv`, `videotoolbox`. Falls back to CPU if the requested
    /// backend is not advertised by `ffmpeg -hwaccels` on this host.
    /// Unrecognised values warn and use CPU.
    pub thorough_hw_accel: String,
    /// Run a quick verification pass after `voom scan` completes.
    /// CLI `--verify` overrides for a single run.
    pub verify_on_scan: bool,
    /// Skip files whose latest quick-verification is fresher than this many
    /// days (idempotency for repeated scans). 0 means always re-verify.
    pub verify_freshness_days: u64,
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
            thorough_hw_accel: "none".into(),
            verify_on_scan: false,
            verify_freshness_days: 7,
        }
    }
}
