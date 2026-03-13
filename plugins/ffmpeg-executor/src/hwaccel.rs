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
    pub enabled: bool,
}

impl HwAccelConfig {
    /// Create a new config with HW accel enabled but no detected backend.
    #[must_use] 
    pub fn new() -> Self {
        Self {
            backend: None,
            enabled: true,
        }
    }

    /// Detect available hardware acceleration by querying ffmpeg.
    #[must_use] 
    pub fn detect() -> Self {
        let backend = Self::detect_backend();
        Self {
            backend,
            enabled: backend.is_some(),
        }
    }

    /// Get the `FFmpeg` encoder name for a codec with this HW backend.
    ///
    /// Falls back to the software encoder when HW accel is disabled or unavailable.
    #[must_use] 
    pub fn encoder_name(&self, codec: &str) -> String {
        if !self.enabled {
            return software_encoder(codec).to_string();
        }

        let (suffix, supported_codecs): (&str, &[&str]) = match self.backend {
            Some(HwAccelBackend::Nvenc) => ("_nvenc", &["hevc", "h264", "av1"]),
            Some(HwAccelBackend::Qsv) => ("_qsv", &["hevc", "h264", "av1", "vp9"]),
            Some(HwAccelBackend::Vaapi) => ("_vaapi", &["hevc", "h264", "av1", "vp9"]),
            Some(HwAccelBackend::Videotoolbox) => ("_videotoolbox", &["hevc", "h264"]),
            None => return software_encoder(codec).to_string(),
        };

        // Normalize codec aliases to canonical names
        let canonical = match codec {
            "h265" => "hevc",
            "avc" => "h264",
            other => other,
        };

        if supported_codecs.contains(&canonical) {
            format!("{canonical}{suffix}")
        } else {
            software_encoder(codec).to_string()
        }
    }

    /// Get `FFmpeg` input args for HW acceleration (e.g., `-hwaccel cuda`).
    #[must_use] 
    pub fn input_args(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }

        let hwaccel_name = match self.backend {
            Some(HwAccelBackend::Nvenc) => "cuda",
            Some(HwAccelBackend::Qsv) => "qsv",
            Some(HwAccelBackend::Vaapi) => "vaapi",
            Some(HwAccelBackend::Videotoolbox) => "videotoolbox",
            None => return Vec::new(),
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

        // Check in order of preference
        if text.contains("cuda") || text.contains("nvdec") {
            return Some(HwAccelBackend::Nvenc);
        }
        if text.contains("qsv") {
            return Some(HwAccelBackend::Qsv);
        }
        if text.contains("vaapi") {
            return Some(HwAccelBackend::Vaapi);
        }
        if text.contains("videotoolbox") {
            return Some(HwAccelBackend::Videotoolbox);
        }

        None
    }
}

impl Default for HwAccelConfig {
    fn default() -> Self {
        Self::new()
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
        let config = HwAccelConfig {
            backend: Some(HwAccelBackend::Nvenc),
            enabled: false,
        };
        // When disabled, should return software encoder names
        assert_eq!(config.encoder_name("hevc"), "libx265");
        assert_eq!(config.encoder_name("h264"), "libx264");
        assert_eq!(config.encoder_name("av1"), "libsvtav1");
        assert!(config.input_args().is_empty());
    }

    #[test]
    fn test_encoder_name_with_nvenc() {
        let config = HwAccelConfig {
            backend: Some(HwAccelBackend::Nvenc),
            enabled: true,
        };
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
        let config = HwAccelConfig {
            backend: Some(HwAccelBackend::Qsv),
            enabled: true,
        };
        assert_eq!(config.encoder_name("h264"), "h264_qsv");
        assert_eq!(config.encoder_name("hevc"), "hevc_qsv");
        assert_eq!(config.encoder_name("av1"), "av1_qsv");
        assert_eq!(config.encoder_name("vp9"), "vp9_qsv");
        assert_eq!(config.input_args(), vec!["-hwaccel", "qsv"]);
    }

    #[test]
    fn test_encoder_name_with_vaapi() {
        let config = HwAccelConfig {
            backend: Some(HwAccelBackend::Vaapi),
            enabled: true,
        };
        assert_eq!(config.encoder_name("hevc"), "hevc_vaapi");
        assert_eq!(config.encoder_name("h264"), "h264_vaapi");
        assert_eq!(config.input_args(), vec!["-hwaccel", "vaapi"]);
    }

    #[test]
    fn test_encoder_name_with_videotoolbox() {
        let config = HwAccelConfig {
            backend: Some(HwAccelBackend::Videotoolbox),
            enabled: true,
        };
        assert_eq!(config.encoder_name("hevc"), "hevc_videotoolbox");
        assert_eq!(config.encoder_name("h264"), "h264_videotoolbox");
        // AV1 not supported by VideoToolbox, falls back
        assert_eq!(config.encoder_name("av1"), "libsvtav1");
        assert_eq!(config.input_args(), vec!["-hwaccel", "videotoolbox"]);
    }

    #[test]
    fn test_no_backend() {
        let config = HwAccelConfig {
            backend: None,
            enabled: true,
        };
        assert_eq!(config.encoder_name("hevc"), "libx265");
        assert!(config.input_args().is_empty());
    }

    #[test]
    fn test_default() {
        let config = HwAccelConfig::default();
        assert!(config.backend.is_none());
        assert!(config.enabled);
    }
}
