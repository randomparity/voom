//! Hardware-accelerated decode helpers for thorough verification.
//!
//! `voom verify --thorough` runs `ffmpeg -i <file> -f null -` end-to-end on
//! every file in the library. On large libraries this pass is the dominant
//! cost. This module lets the caller request a HW decode backend (NVDEC,
//! VAAPI, QSV, VideoToolbox), probes whether ffmpeg actually supports it on
//! this host, and falls back to CPU if not.
//!
//! Decode-only by design — the verifier never re-encodes, so no encoder
//! mapping or device targeting is required. Encoder-side hwaccel logic
//! lives in `voom-ffmpeg-executor::hwaccel`.

use std::time::Duration;

use voom_process::run_with_timeout;

/// User-facing HW decode mode.
///
/// `Auto` picks the first probed backend; `None` forces CPU. Other variants
/// pin a specific backend, falling back to CPU if it isn't available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwAccelMode {
    /// CPU decode (default).
    #[default]
    None,
    /// Probe `ffmpeg -hwaccels` and pick the first supported backend.
    Auto,
    /// NVIDIA NVDEC (`-hwaccel cuda`).
    Nvdec,
    /// Linux VA-API (`-hwaccel vaapi`).
    Vaapi,
    /// Intel `QuickSync` Video (`-hwaccel qsv`).
    Qsv,
    /// macOS `VideoToolbox` (`-hwaccel videotoolbox`).
    Videotoolbox,
}

impl HwAccelMode {
    /// Parse a canonical name as accepted by the CLI flag and config key.
    /// Recognised: `none`, `auto`, `nvdec`, `vaapi`, `qsv`, `videotoolbox`.
    /// Case-insensitive; surrounding whitespace is ignored.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "auto" => Some(Self::Auto),
            "nvdec" | "cuda" | "nvenc" => Some(Self::Nvdec),
            "vaapi" => Some(Self::Vaapi),
            "qsv" => Some(Self::Qsv),
            "videotoolbox" => Some(Self::Videotoolbox),
            _ => None,
        }
    }

    /// Canonical, user-visible name (mirrors the CLI flag values).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Auto => "auto",
            Self::Nvdec => "nvdec",
            Self::Vaapi => "vaapi",
            Self::Qsv => "qsv",
            Self::Videotoolbox => "videotoolbox",
        }
    }

    /// The `-hwaccel` argument value ffmpeg expects for this backend.
    /// Returns `None` for `None`/`Auto` (the latter is resolved separately).
    #[must_use]
    pub fn ffmpeg_name(self) -> Option<&'static str> {
        match self {
            Self::None | Self::Auto => None,
            Self::Nvdec => Some("cuda"),
            Self::Vaapi => Some("vaapi"),
            Self::Qsv => Some("qsv"),
            Self::Videotoolbox => Some("videotoolbox"),
        }
    }
}

/// Pin a concrete backend to use for decode, or `None` if CPU.
///
/// Resolution rules:
/// - `HwAccelMode::None` → `None`.
/// - `HwAccelMode::Auto` → first probed backend in priority order, or `None`.
/// - Any explicit backend → that backend if `probed` contains its ffmpeg
///   name, else `None` with a warning logged.
///
/// `probed` is the list returned by `ffmpeg -hwaccels` (lower-cased,
/// trimmed). Auto-probe order matches `voom-ffmpeg-executor`: NVDEC, QSV,
/// VAAPI, VideoToolbox.
#[must_use]
pub fn resolve(mode: HwAccelMode, probed: &[String]) -> Option<HwAccelMode> {
    let lower: Vec<String> = probed.iter().map(|s| s.to_ascii_lowercase()).collect();
    match mode {
        HwAccelMode::None => None,
        HwAccelMode::Auto => auto_pick(&lower),
        explicit => {
            let want = explicit.ffmpeg_name()?;
            if lower.iter().any(|s| s == want) {
                Some(explicit)
            } else {
                tracing::warn!(
                    backend = explicit.as_str(),
                    probed = ?lower,
                    "configured hw_accel backend not supported by this ffmpeg \
                     build; falling back to CPU decode"
                );
                None
            }
        }
    }
}

fn auto_pick(lower: &[String]) -> Option<HwAccelMode> {
    if lower.iter().any(|s| s == "cuda") {
        return Some(HwAccelMode::Nvdec);
    }
    if lower.iter().any(|s| s == "qsv") {
        return Some(HwAccelMode::Qsv);
    }
    if lower.iter().any(|s| s == "vaapi") {
        return Some(HwAccelMode::Vaapi);
    }
    if lower.iter().any(|s| s == "videotoolbox") {
        return Some(HwAccelMode::Videotoolbox);
    }
    None
}

/// Probe `ffmpeg -hwaccels`, returning the lower-cased backend names that
/// the binary advertises. Spawn errors, non-zero exits, and timeouts all
/// degrade silently to an empty `Vec` — the caller treats that as "no HW
/// support" and falls back to CPU.
#[must_use]
pub fn probe_hwaccels(ffmpeg_path: &str) -> Vec<String> {
    let args = [std::ffi::OsString::from("-hwaccels")];
    match run_with_timeout(ffmpeg_path, &args, Duration::from_secs(5)) {
        Ok(output) if output.status.success() => parse_hwaccels(&output.stdout),
        Ok(_) | Err(_) => Vec::new(),
    }
}

fn parse_hwaccels(stdout: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .skip_while(|l| !l.contains("Hardware acceleration methods"))
        .skip(1)
        .filter_map(|l| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_ascii_lowercase())
            }
        })
        .collect()
}

/// Return ffmpeg input args for a resolved backend (`-hwaccel <name>`),
/// or an empty list for CPU decode. Always emitted before `-i`.
#[must_use]
pub fn input_args(resolved: Option<HwAccelMode>) -> Vec<String> {
    let Some(mode) = resolved else {
        return Vec::new();
    };
    let Some(name) = mode.ffmpeg_name() else {
        return Vec::new();
    };
    vec!["-hwaccel".to_string(), name.to_string()]
}

/// Return true if `line` is benign HW-vendor diagnostic output that should
/// not count as a verification error.
///
/// Hardware decoders surface a handful of `-v error` level messages that
/// don't indicate corruption — context-init churn, fallback notices,
/// per-frame format warnings. Counting these as decode errors would turn
/// every HW-accelerated thorough run into a false-positive. The list
/// below is intentionally conservative: only patterns that are
/// independent of the file under test are filtered.
#[must_use]
pub fn is_hwaccel_noise(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() {
        return true;
    }
    // Common HW-init diagnostics that ffmpeg emits before transparent
    // CPU fallback.  None of these indicate a corrupt source.
    const NOISE_FRAGMENTS: &[&str] = &[
        "Failed setup for format",
        "hwaccel initialisation returned error",
        "No device available for decoder",
        "Cannot load nvcuvid",
        "Cannot load libcuda",
        "Cannot allocate memory in static TLS block",
        "Could not dynamically load CUDA",
        "vaapi_hwaccel: Failed to initialise",
        "qsv_hwaccel: Failed to initialise",
        "Hardware does not support accelerated",
    ];
    NOISE_FRAGMENTS.iter().any(|frag| l.contains(frag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_none_and_auto() {
        assert_eq!(HwAccelMode::parse("none"), Some(HwAccelMode::None));
        assert_eq!(HwAccelMode::parse(""), Some(HwAccelMode::None));
        assert_eq!(HwAccelMode::parse("auto"), Some(HwAccelMode::Auto));
        assert_eq!(HwAccelMode::parse("AUTO"), Some(HwAccelMode::Auto));
        assert_eq!(HwAccelMode::parse(" auto "), Some(HwAccelMode::Auto));
    }

    #[test]
    fn parse_explicit_backends() {
        assert_eq!(HwAccelMode::parse("nvdec"), Some(HwAccelMode::Nvdec));
        assert_eq!(HwAccelMode::parse("cuda"), Some(HwAccelMode::Nvdec));
        assert_eq!(HwAccelMode::parse("VAAPI"), Some(HwAccelMode::Vaapi));
        assert_eq!(HwAccelMode::parse("qsv"), Some(HwAccelMode::Qsv));
        assert_eq!(
            HwAccelMode::parse("videotoolbox"),
            Some(HwAccelMode::Videotoolbox)
        );
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(HwAccelMode::parse("d3d11va"), None);
        assert_eq!(HwAccelMode::parse("opencl"), None);
    }

    #[test]
    fn ffmpeg_name_maps_correctly() {
        assert_eq!(HwAccelMode::Nvdec.ffmpeg_name(), Some("cuda"));
        assert_eq!(HwAccelMode::Vaapi.ffmpeg_name(), Some("vaapi"));
        assert_eq!(HwAccelMode::Qsv.ffmpeg_name(), Some("qsv"));
        assert_eq!(
            HwAccelMode::Videotoolbox.ffmpeg_name(),
            Some("videotoolbox")
        );
        assert!(HwAccelMode::None.ffmpeg_name().is_none());
        assert!(HwAccelMode::Auto.ffmpeg_name().is_none());
    }

    #[test]
    fn resolve_none_disables() {
        let probed = vec!["cuda".into(), "vaapi".into()];
        assert!(resolve(HwAccelMode::None, &probed).is_none());
    }

    #[test]
    fn resolve_auto_prefers_cuda() {
        let probed = vec!["cuda".into(), "vaapi".into()];
        assert_eq!(
            resolve(HwAccelMode::Auto, &probed),
            Some(HwAccelMode::Nvdec)
        );
    }

    #[test]
    fn resolve_auto_falls_through_priority() {
        let probed = vec!["vaapi".into(), "qsv".into()];
        assert_eq!(resolve(HwAccelMode::Auto, &probed), Some(HwAccelMode::Qsv));
    }

    #[test]
    fn resolve_auto_picks_videotoolbox() {
        let probed = vec!["videotoolbox".into()];
        assert_eq!(
            resolve(HwAccelMode::Auto, &probed),
            Some(HwAccelMode::Videotoolbox)
        );
    }

    #[test]
    fn resolve_auto_empty_returns_none() {
        let probed: Vec<String> = vec![];
        assert!(resolve(HwAccelMode::Auto, &probed).is_none());
    }

    #[test]
    fn resolve_explicit_match_returns_self() {
        let probed = vec!["cuda".into()];
        assert_eq!(
            resolve(HwAccelMode::Nvdec, &probed),
            Some(HwAccelMode::Nvdec)
        );
    }

    #[test]
    fn resolve_explicit_missing_falls_back_to_cpu() {
        let probed = vec!["vaapi".into()];
        // User asked for nvdec but ffmpeg has only vaapi → CPU.
        assert!(resolve(HwAccelMode::Nvdec, &probed).is_none());
    }

    #[test]
    fn resolve_is_case_insensitive_against_probed_list() {
        let probed = vec!["CUDA".into()];
        assert_eq!(
            resolve(HwAccelMode::Nvdec, &probed),
            Some(HwAccelMode::Nvdec)
        );
    }

    #[test]
    fn parse_hwaccels_handles_typical_output() {
        let stdout = b"Hardware acceleration methods:\ncuda\nvaapi\nvideotoolbox\n";
        let parsed = parse_hwaccels(stdout);
        assert_eq!(parsed, vec!["cuda", "vaapi", "videotoolbox"]);
    }

    #[test]
    fn parse_hwaccels_lower_cases() {
        let stdout = b"Hardware acceleration methods:\nCUDA\nVAAPI\n";
        let parsed = parse_hwaccels(stdout);
        assert_eq!(parsed, vec!["cuda", "vaapi"]);
    }

    #[test]
    fn parse_hwaccels_skips_blank_lines() {
        let stdout = b"Hardware acceleration methods:\n\ncuda\n\nvaapi\n";
        let parsed = parse_hwaccels(stdout);
        assert_eq!(parsed, vec!["cuda", "vaapi"]);
    }

    #[test]
    fn parse_hwaccels_without_header_is_empty() {
        let stdout = b"unrelated banner\nstuff\n";
        assert!(parse_hwaccels(stdout).is_empty());
    }

    #[test]
    fn input_args_for_cpu_is_empty() {
        assert!(input_args(None).is_empty());
        assert!(input_args(Some(HwAccelMode::None)).is_empty());
        assert!(input_args(Some(HwAccelMode::Auto)).is_empty());
    }

    #[test]
    fn input_args_emit_hwaccel_flag() {
        assert_eq!(
            input_args(Some(HwAccelMode::Nvdec)),
            vec!["-hwaccel", "cuda"]
        );
        assert_eq!(
            input_args(Some(HwAccelMode::Vaapi)),
            vec!["-hwaccel", "vaapi"]
        );
        assert_eq!(input_args(Some(HwAccelMode::Qsv)), vec!["-hwaccel", "qsv"]);
        assert_eq!(
            input_args(Some(HwAccelMode::Videotoolbox)),
            vec!["-hwaccel", "videotoolbox"]
        );
    }

    #[test]
    fn noise_filter_matches_known_patterns() {
        assert!(is_hwaccel_noise(
            "[h264 @ 0x55] Failed setup for format cuda: hwaccel initialisation returned error"
        ));
        assert!(is_hwaccel_noise("Cannot load nvcuvid"));
        assert!(is_hwaccel_noise(
            "[hevc @ 0x42] No device available for decoder"
        ));
        assert!(is_hwaccel_noise(
            "Hardware does not support accelerated decoding"
        ));
    }

    #[test]
    fn noise_filter_rejects_real_decode_errors() {
        assert!(!is_hwaccel_noise(
            "[matroska,webm @ 0x55] Truncated packet, error in reference frame"
        ));
        assert!(!is_hwaccel_noise(
            "[h264 @ 0x42] error while decoding MB 100 50, bytestream"
        ));
        assert!(!is_hwaccel_noise(
            "Invalid data found when processing input"
        ));
    }

    #[test]
    fn noise_filter_treats_blank_lines_as_noise() {
        assert!(is_hwaccel_noise(""));
        assert!(is_hwaccel_noise("   "));
    }
}
