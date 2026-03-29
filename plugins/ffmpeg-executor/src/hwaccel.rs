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
}

impl HwAccelConfig {
    /// Create a new config with no detected backend (HW accel disabled).
    #[must_use]
    pub fn new() -> Self {
        Self {
            backend: None,
            validated_encoders: None,
        }
    }

    /// Create a config with a specific backend (no validation).
    #[must_use]
    pub fn with_backend(backend: HwAccelBackend) -> Self {
        Self {
            backend: Some(backend),
            validated_encoders: None,
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

    /// Get the `FFmpeg` encoder name for a codec with this HW backend.
    ///
    /// Falls back to the software encoder when HW accel is disabled or unavailable.
    #[must_use]
    pub fn encoder_name(&self, codec: &str) -> String {
        let backend = match self.backend {
            Some(b) => b,
            None => return software_encoder(codec).to_string(),
        };

        let (suffix, supported_codecs): (&str, &[&str]) = match backend {
            HwAccelBackend::Nvenc => ("_nvenc", &["hevc", "h264", "av1"]),
            HwAccelBackend::Qsv => ("_qsv", &["hevc", "h264", "av1", "vp9"]),
            HwAccelBackend::Vaapi => ("_vaapi", &["hevc", "h264", "av1", "vp9"]),
            HwAccelBackend::Videotoolbox => ("_videotoolbox", &["hevc", "h264"]),
        };

        // Normalize codec aliases to canonical names
        let canonical = match codec {
            "h265" => "hevc",
            "avc" => "h264",
            other => other,
        };

        if supported_codecs.contains(&canonical) {
            let hw_name = format!("{canonical}{suffix}");
            // If we validated encoders at init, only use ones that work
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
        } else {
            software_encoder(codec).to_string()
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

/// Match lowercased hwaccel text to a backend in priority order.
fn detect_backend_from_text(text: &str) -> Option<HwAccelBackend> {
    if text.contains("cuda") || text.contains("nvdec") {
        Some(HwAccelBackend::Nvenc)
    } else if text.contains("qsv") {
        Some(HwAccelBackend::Qsv)
    } else if text.contains("vaapi") {
        Some(HwAccelBackend::Vaapi)
    } else if text.contains("videotoolbox") {
        Some(HwAccelBackend::Videotoolbox)
    } else {
        None
    }
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
    }
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
}
