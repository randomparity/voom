//! Hardware acceleration detection and configuration for `FFmpeg`.

use std::process::Command;

/// Hardware acceleration backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwAccelBackend {
    /// NVIDIA NVENC/NVDEC
    Nvenc,
    /// Intel `QuickSync` Video
    Qsv,
    /// Linux VA-API
    Vaapi,
    /// macOS `VideoToolbox`
    Videotoolbox,
}

/// Hardware acceleration configuration.
#[derive(Debug, Clone)]
pub struct HwAccelConfig {
    pub backend: Option<HwAccelBackend>,
    /// HW encoders validated to work on this device. When `Some`, only
    /// encoders in the list are used; missing ones fall back to software.
    /// `None` means no validation was performed (all are trusted).
    validated_encoders: Option<Vec<String>>,
    /// Target GPU/render device. NVIDIA: "0", "1", etc.
    /// VA-API/QSV: "/dev/dri/renderD128", etc.
    device: Option<String>,
}

impl HwAccelConfig {
    /// Create a new config with no detected backend (HW accel disabled).
    #[must_use]
    pub fn new() -> Self {
        Self {
            backend: None,
            validated_encoders: None,
            device: None,
        }
    }

    /// Create a config with a specific backend (no validation).
    #[must_use]
    pub fn with_backend(backend: HwAccelBackend) -> Self {
        Self {
            backend: Some(backend),
            validated_encoders: None,
            device: None,
        }
    }

    /// Whether hardware acceleration is available.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.backend.is_some()
    }

    /// Detect available hardware acceleration by querying ffmpeg.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            backend: Self::detect_backend(),
            validated_encoders: None,
            device: None,
        }
    }

    /// Select the best backend from already-probed hwaccel names,
    /// avoiding a redundant `ffmpeg -hwaccels` subprocess.
    #[must_use]
    pub fn from_probed(hw_accels: &[String]) -> Self {
        let text: String = hw_accels
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            backend: detect_backend_from_text(&text),
            validated_encoders: None,
            device: None,
        }
    }

    /// Set the list of HW encoders validated to work on this device.
    ///
    /// When set, `encoder_name()` will only return a HW encoder if it
    /// appears in this list; otherwise it falls back to the software encoder.
    #[must_use]
    pub fn with_validated_encoders(mut self, encoders: Vec<String>) -> Self {
        self.validated_encoders = Some(encoders);
        self
    }

    /// Set the target GPU/render device for HW encoding.
    #[must_use]
    pub fn with_device(mut self, device: Option<String>) -> Self {
        self.device = device;
        self
    }

    /// Get the configured device identifier, if any.
    #[must_use]
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// Return ffmpeg CLI args for device targeting.
    ///
    /// - Vaapi: `["-vaapi_device", "<path>"]`
    /// - Qsv: `["-qsv_device", "<path>"]`
    /// - Nvenc/VideoToolbox: `[]` (handled via env var or no targeting)
    #[must_use]
    pub fn device_args(&self) -> Vec<String> {
        let device = match (&self.backend, &self.device) {
            (Some(backend), Some(dev)) => (*backend, dev.as_str()),
            _ => return Vec::new(),
        };
        match device {
            (HwAccelBackend::Vaapi, path) => {
                vec!["-vaapi_device".to_string(), path.to_string()]
            }
            (HwAccelBackend::Qsv, path) => {
                vec!["-qsv_device".to_string(), path.to_string()]
            }
            (HwAccelBackend::Nvenc | HwAccelBackend::Videotoolbox, _) => Vec::new(),
        }
    }

    /// Return an environment variable for device targeting in subprocess.
    ///
    /// - Nvenc: `Some(("CUDA_VISIBLE_DEVICES", "<id>"))`
    /// - Others: `None`
    #[must_use]
    pub fn device_env(&self) -> Option<(&str, &str)> {
        match (&self.backend, &self.device) {
            (Some(HwAccelBackend::Nvenc), Some(dev)) => {
                Some(("CUDA_VISIBLE_DEVICES", dev.as_str()))
            }
            _ => None,
        }
    }

    /// Get the `FFmpeg` encoder name for a codec with this HW backend.
    ///
    /// Falls back to the software encoder when HW accel is disabled or unavailable.
    #[must_use]
    pub fn encoder_name(&self, codec: &str) -> String {
        let backend = match self.backend {
            Some(b) => b,
            None => return software_encoder(codec).to_string(),
        };

        let Some(hw_name) = hw_encoder_for_backend(backend, codec) else {
            return software_encoder(codec).to_string();
        };

        if let Some(validated) = &self.validated_encoders {
            if validated.iter().any(|e| e == &hw_name) {
                hw_name
            } else {
                tracing::info!(
                    encoder = %hw_name,
                    "HW encoder not available on this device, \
                     falling back to software"
                );
                software_encoder(codec).to_string()
            }
        } else {
            hw_name
        }
    }

    /// Check whether a validated HW encoder exists for the given codec.
    ///
    /// Returns `true` if HW is enabled and either no validation was
    /// performed (all trusted) or the specific encoder passed validation.
    /// Returns `false` when HW is disabled or the encoder failed
    /// validation.
    #[must_use]
    pub fn has_hw_encoder(&self, codec: &str) -> bool {
        let backend = match self.backend {
            Some(b) => b,
            None => return false,
        };

        let Some(hw_name) = hw_encoder_for_backend(backend, codec) else {
            return false;
        };

        match &self.validated_encoders {
            Some(validated) => validated.iter().any(|e| e == &hw_name),
            None => true,
        }
    }

    /// Get `FFmpeg` input args for HW acceleration (e.g., `-hwaccel cuda`).
    #[must_use]
    pub fn input_args(&self) -> Vec<String> {
        let backend = match self.backend {
            Some(b) => b,
            None => return Vec::new(),
        };

        let hwaccel_name = match backend {
            HwAccelBackend::Nvenc => "cuda",
            HwAccelBackend::Qsv => "qsv",
            HwAccelBackend::Vaapi => "vaapi",
            HwAccelBackend::Videotoolbox => "videotoolbox",
        };

        vec!["-hwaccel".to_string(), hwaccel_name.to_string()]
    }

    /// Check if HW accel is available by running `ffmpeg -hwaccels`.
    fn detect_backend() -> Option<HwAccelBackend> {
        let output = match Command::new("ffmpeg")
            .args(["-hwaccels", "-hide_banner"])
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                tracing::debug!(error = %e, "failed to run ffmpeg for hwaccel detection");
                return None;
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let text = stdout.to_ascii_lowercase();
        detect_backend_from_text(&text)
    }
}

/// Resolve the HW encoder name for a backend+codec pair.
///
/// Returns `None` if the backend doesn't support the codec. Handles
/// codec alias normalization (`h265→hevc`, `avc→h264`).
fn hw_encoder_for_backend(backend: HwAccelBackend, codec: &str) -> Option<String> {
    let (suffix, supported_codecs): (&str, &[&str]) = match backend {
        HwAccelBackend::Nvenc => ("_nvenc", &["hevc", "h264", "av1"]),
        HwAccelBackend::Qsv => ("_qsv", &["hevc", "h264", "av1", "vp9"]),
        HwAccelBackend::Vaapi => ("_vaapi", &["hevc", "h264", "av1", "vp9"]),
        HwAccelBackend::Videotoolbox => ("_videotoolbox", &["hevc", "h264"]),
    };

    let canonical = match codec {
        "h265" => "hevc",
        "avc" => "h264",
        other => other,
    };

    if supported_codecs.contains(&canonical) {
        Some(format!("{canonical}{suffix}"))
    } else {
        None
    }
}

/// Collect all matching backends from lowercased hwaccel text, in
/// priority order.
fn candidates_from_text(text: &str) -> Vec<HwAccelBackend> {
    let mut candidates = Vec::new();
    if text.contains("cuda") || text.contains("nvdec") {
        candidates.push(HwAccelBackend::Nvenc);
    }
    if text.contains("qsv") {
        candidates.push(HwAccelBackend::Qsv);
    }
    if text.contains("vaapi") {
        candidates.push(HwAccelBackend::Vaapi);
    }
    if text.contains("videotoolbox") {
        candidates.push(HwAccelBackend::Videotoolbox);
    }
    candidates
}

/// Check whether the actual hardware for a backend is present.
fn verify_backend(backend: HwAccelBackend) -> bool {
    match backend {
        HwAccelBackend::Nvenc => crate::probe::has_nvidia_hardware(),
        HwAccelBackend::Qsv => crate::probe::has_intel_gpu(),
        HwAccelBackend::Vaapi => crate::probe::has_vaapi_devices(),
        HwAccelBackend::Videotoolbox => true,
    }
}

/// Pick the best backend: first candidate whose hardware is present.
fn detect_backend_with_verifier(
    text: &str,
    verifier: fn(HwAccelBackend) -> bool,
) -> Option<HwAccelBackend> {
    candidates_from_text(text)
        .into_iter()
        .find(|b| verifier(*b))
}

/// Match lowercased hwaccel text to a backend, verifying hardware
/// is actually present before committing.
fn detect_backend_from_text(text: &str) -> Option<HwAccelBackend> {
    detect_backend_with_verifier(text, verify_backend)
}

impl Default for HwAccelConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Create an `HwAccelConfig` from a canonical backend name (as used in
/// the DSL `hw:` setting). Returns a disabled config for unrecognized names.
#[must_use]
pub fn config_from_backend_name(name: &str) -> HwAccelConfig {
    let backend = match name {
        "nvenc" => Some(HwAccelBackend::Nvenc),
        "qsv" => Some(HwAccelBackend::Qsv),
        "vaapi" => Some(HwAccelBackend::Vaapi),
        "videotoolbox" => Some(HwAccelBackend::Videotoolbox),
        _ => None,
    };
    HwAccelConfig {
        backend,
        validated_encoders: None,
        device: None,
    }
}

/// Create an override config that inherits validated encoders from the
/// system-wide config when the backends match. This prevents per-action
/// `hw:` overrides from bypassing encoder validation.
#[must_use]
pub fn config_from_backend_with_system(
    name: &str,
    system: Option<&HwAccelConfig>,
) -> HwAccelConfig {
    let mut config = config_from_backend_name(name);
    // Inherit validated encoders when the override matches the system
    // backend — the validation results are still applicable.
    if let (Some(override_be), Some(sys)) = (config.backend, system) {
        if sys.backend == Some(override_be) {
            config.validated_encoders = sys.validated_encoders.clone();
            config.device = sys.device.clone();
        }
    }
    config
}

/// Map a codec name to the `FFmpeg` software encoder name.
#[must_use]
pub fn software_encoder(codec: &str) -> &str {
    match codec {
        "hevc" | "h265" => "libx265",
        "h264" | "avc" => "libx264",
        "av1" => "libsvtav1",
        "vp9" => "libvpx-vp9",
        "aac" => "aac",
        "ac3" => "ac3",
        "eac3" => "eac3",
        "opus" => "libopus",
        "flac" => "flac",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_software_encoder_mapping() {
        assert_eq!(software_encoder("hevc"), "libx265");
        assert_eq!(software_encoder("h265"), "libx265");
        assert_eq!(software_encoder("h264"), "libx264");
        assert_eq!(software_encoder("avc"), "libx264");
        assert_eq!(software_encoder("av1"), "libsvtav1");
        assert_eq!(software_encoder("vp9"), "libvpx-vp9");
        assert_eq!(software_encoder("aac"), "aac");
        assert_eq!(software_encoder("ac3"), "ac3");
        assert_eq!(software_encoder("eac3"), "eac3");
        assert_eq!(software_encoder("opus"), "libopus");
        assert_eq!(software_encoder("flac"), "flac");
        assert_eq!(software_encoder("pcm_s16le"), "pcm_s16le");
    }

    #[test]
    fn test_hwaccel_config_disabled() {
        let config = HwAccelConfig::new();
        // When disabled, should return software encoder names
        assert_eq!(config.encoder_name("hevc"), "libx265");
        assert_eq!(config.encoder_name("h264"), "libx264");
        assert_eq!(config.encoder_name("av1"), "libsvtav1");
        assert!(config.input_args().is_empty());
    }

    #[test]
    fn test_encoder_name_with_nvenc() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc);
        assert_eq!(config.encoder_name("hevc"), "hevc_nvenc");
        assert_eq!(config.encoder_name("h265"), "hevc_nvenc");
        assert_eq!(config.encoder_name("h264"), "h264_nvenc");
        assert_eq!(config.encoder_name("avc"), "h264_nvenc");
        assert_eq!(config.encoder_name("av1"), "av1_nvenc");
        // VP9 has no NVENC support, falls back to software
        assert_eq!(config.encoder_name("vp9"), "libvpx-vp9");
        // Audio codecs always use software
        assert_eq!(config.encoder_name("aac"), "aac");
        assert_eq!(config.input_args(), vec!["-hwaccel", "cuda"]);
    }

    #[test]
    fn test_encoder_name_with_qsv() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Qsv);
        assert_eq!(config.encoder_name("h264"), "h264_qsv");
        assert_eq!(config.encoder_name("hevc"), "hevc_qsv");
        assert_eq!(config.encoder_name("av1"), "av1_qsv");
        assert_eq!(config.encoder_name("vp9"), "vp9_qsv");
        assert_eq!(config.input_args(), vec!["-hwaccel", "qsv"]);
    }

    #[test]
    fn test_encoder_name_with_vaapi() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Vaapi);
        assert_eq!(config.encoder_name("hevc"), "hevc_vaapi");
        assert_eq!(config.encoder_name("h264"), "h264_vaapi");
        assert_eq!(config.input_args(), vec!["-hwaccel", "vaapi"]);
    }

    #[test]
    fn test_encoder_name_with_videotoolbox() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Videotoolbox);
        assert_eq!(config.encoder_name("hevc"), "hevc_videotoolbox");
        assert_eq!(config.encoder_name("h264"), "h264_videotoolbox");
        // AV1 not supported by VideoToolbox, falls back
        assert_eq!(config.encoder_name("av1"), "libsvtav1");
        assert_eq!(config.input_args(), vec!["-hwaccel", "videotoolbox"]);
    }

    #[test]
    fn test_no_backend() {
        let config = HwAccelConfig::new();
        assert_eq!(config.encoder_name("hevc"), "libx265");
        assert!(config.input_args().is_empty());
    }

    #[test]
    fn test_default() {
        let config = HwAccelConfig::default();
        assert!(config.backend.is_none());
        assert!(!config.enabled());
    }

    #[test]
    fn test_validated_encoders_fallback() {
        // NVENC backend detected, but only h264_nvenc and hevc_nvenc validated
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()]);
        assert_eq!(config.encoder_name("h264"), "h264_nvenc");
        assert_eq!(config.encoder_name("hevc"), "hevc_nvenc");
        // av1_nvenc NOT validated — should fall back to software
        assert_eq!(config.encoder_name("av1"), "libsvtav1");
    }

    #[test]
    fn test_unvalidated_trusts_all() {
        // No validation performed — all HW encoders trusted (backward compat)
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc);
        assert_eq!(config.encoder_name("av1"), "av1_nvenc");
    }

    // ── device targeting ────────────────────────────────────────

    #[test]
    fn test_device_args_nvenc() {
        let config =
            HwAccelConfig::with_backend(HwAccelBackend::Nvenc).with_device(Some("1".into()));
        // NVENC uses env var, not CLI args
        assert!(config.device_args().is_empty());
    }

    #[test]
    fn test_device_args_vaapi() {
        let path = "/dev/dri/renderD129";
        let config =
            HwAccelConfig::with_backend(HwAccelBackend::Vaapi).with_device(Some(path.into()));
        assert_eq!(config.device_args(), vec!["-vaapi_device", path]);
    }

    #[test]
    fn test_device_args_qsv() {
        let path = "/dev/dri/renderD128";
        let config =
            HwAccelConfig::with_backend(HwAccelBackend::Qsv).with_device(Some(path.into()));
        assert_eq!(config.device_args(), vec!["-qsv_device", path]);
    }

    #[test]
    fn test_device_args_none() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc);
        assert!(config.device_args().is_empty());
    }

    #[test]
    fn test_device_env_nvenc() {
        let config =
            HwAccelConfig::with_backend(HwAccelBackend::Nvenc).with_device(Some("1".into()));
        assert_eq!(config.device_env(), Some(("CUDA_VISIBLE_DEVICES", "1")));
    }

    #[test]
    fn test_device_env_vaapi() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Vaapi)
            .with_device(Some("/dev/dri/renderD129".into()));
        assert!(config.device_env().is_none());
    }

    #[test]
    fn test_device_env_no_device() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc);
        assert!(config.device_env().is_none());
    }

    // ── has_hw_encoder ─────────────────────────────────────────

    #[test]
    fn test_has_hw_encoder_validated() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()]);
        assert!(config.has_hw_encoder("h264"));
        assert!(config.has_hw_encoder("hevc"));
        assert!(!config.has_hw_encoder("av1"));
        assert!(!config.has_hw_encoder("vp9"));
    }

    #[test]
    fn test_has_hw_encoder_unvalidated() {
        let config = HwAccelConfig::with_backend(HwAccelBackend::Nvenc);
        assert!(config.has_hw_encoder("av1"));
        assert!(config.has_hw_encoder("h264"));
    }

    #[test]
    fn test_has_hw_encoder_disabled() {
        let config = HwAccelConfig::new();
        assert!(!config.has_hw_encoder("h264"));
    }

    // ── config_from_backend_with_system ────────────────────────

    #[test]
    fn test_override_inherits_validation_same_backend() {
        let system = HwAccelConfig::with_backend(HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()])
            .with_device(Some("0".into()));

        let over = config_from_backend_with_system("nvenc", Some(&system));
        // Should inherit validated encoders — av1_nvenc not validated
        assert_eq!(over.encoder_name("av1"), "libsvtav1");
        assert_eq!(over.encoder_name("h264"), "h264_nvenc");
        assert_eq!(over.device(), Some("0"));
    }

    #[test]
    fn test_override_no_inherit_different_backend() {
        let system = HwAccelConfig::with_backend(HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into()]);

        let over = config_from_backend_with_system("qsv", Some(&system));
        // Different backend — no validation data, trusts all
        assert_eq!(over.encoder_name("h264"), "h264_qsv");
        assert!(over.device().is_none());
    }

    #[test]
    fn test_override_no_system_config() {
        let over = config_from_backend_with_system("nvenc", None);
        // No system config — trusts all (backward compat)
        assert_eq!(over.encoder_name("av1"), "av1_nvenc");
    }

    // ── candidates + verifier ─────────────────────────────────────

    #[test]
    fn test_candidates_cuda_and_vaapi() {
        let c = candidates_from_text("cuda vaapi");
        assert_eq!(c, vec![HwAccelBackend::Nvenc, HwAccelBackend::Vaapi]);
    }

    #[test]
    fn test_candidates_vaapi_only() {
        let c = candidates_from_text("vaapi");
        assert_eq!(c, vec![HwAccelBackend::Vaapi]);
    }

    #[test]
    fn test_candidates_all() {
        let c = candidates_from_text("cuda qsv vaapi videotoolbox");
        assert_eq!(
            c,
            vec![
                HwAccelBackend::Nvenc,
                HwAccelBackend::Qsv,
                HwAccelBackend::Vaapi,
                HwAccelBackend::Videotoolbox,
            ]
        );
    }

    #[test]
    fn test_candidates_empty() {
        assert!(candidates_from_text("").is_empty());
        assert!(candidates_from_text("vulkan opencl").is_empty());
    }

    #[test]
    fn test_verifier_skips_nvenc_picks_vaapi() {
        // AMD scenario: cuda compiled in but no NVIDIA hardware
        fn reject_nvenc(b: HwAccelBackend) -> bool {
            b != HwAccelBackend::Nvenc
        }
        let result = detect_backend_with_verifier("cuda vaapi", reject_nvenc);
        assert_eq!(result, Some(HwAccelBackend::Vaapi));
    }

    #[test]
    fn test_verifier_accepts_nvenc() {
        // NVIDIA scenario: real hardware present
        fn accept_all(_: HwAccelBackend) -> bool {
            true
        }
        let result = detect_backend_with_verifier("cuda vaapi", accept_all);
        assert_eq!(result, Some(HwAccelBackend::Nvenc));
    }

    #[test]
    fn test_verifier_rejects_all() {
        fn reject_all(_: HwAccelBackend) -> bool {
            false
        }
        let result = detect_backend_with_verifier("cuda vaapi qsv", reject_all);
        assert_eq!(result, None);
    }
}
