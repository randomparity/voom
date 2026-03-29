//! FFmpeg capability probing: parse output from `ffmpeg -codecs`, `-formats`, `-hwaccels`.

use crate::hwaccel::HwAccelBackend;
use voom_domain::events::CodecCapabilities;

/// Parse `ffmpeg -codecs` output into decoder and encoder lists.
///
/// Each codec line (after the `-------` separator) has flags in columns 0-5:
/// `D` = decoding, `E` = encoding. The codec name follows after whitespace.
pub fn parse_codecs(output: &str) -> CodecCapabilities {
    let mut decoders = Vec::new();
    let mut encoders = Vec::new();
    let mut past_separator = false;

    for line in output.lines() {
        if line.starts_with(" ------") {
            past_separator = true;
            continue;
        }
        if !past_separator {
            continue;
        }
        // Lines look like: " DEV.L. h264   H.264 / AVC / MPEG-4 AVC"
        // Flags are in columns 1-6, codec name starts after whitespace
        let trimmed = line.trim_start();
        if trimmed.len() < 8 {
            continue;
        }
        let flags = &trimmed[..6];
        let rest = trimmed[6..].trim_start();
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if flags.starts_with('D') {
            decoders.push(name.clone());
        }
        if flags.chars().nth(1) == Some('E') {
            encoders.push(name);
        }
    }

    CodecCapabilities::new(decoders, encoders)
}

/// Parse `ffmpeg -formats` output into a list of supported format names.
///
/// Each format line (after the `-------` separator) has flags in columns 0-2:
/// `D` = demux, `E` = mux. We collect any format that can be muxed or demuxed.
pub fn parse_formats(output: &str) -> Vec<String> {
    let mut formats = Vec::new();
    let mut past_separator = false;

    for line in output.lines() {
        if line.starts_with(" ------") {
            past_separator = true;
            continue;
        }
        if !past_separator {
            continue;
        }
        // Lines look like: " DE matroska,webm Matroska / WebM"
        let trimmed = line.trim_start();
        if trimmed.len() < 4 {
            continue;
        }
        let rest = trimmed[2..].trim_start();
        let name_field = rest.split_whitespace().next().unwrap_or("");
        // Some formats list aliases: "matroska,webm" — take the primary
        for name in name_field.split(',') {
            let name = name.trim();
            if !name.is_empty() {
                formats.push(name.to_string());
            }
        }
    }

    formats.sort();
    formats.dedup();
    formats
}

/// Known suffixes that identify hardware-accelerated encoder/decoder
/// implementations in ffmpeg's `-encoders` / `-decoders` output.
const HW_SUFFIXES: &[&str] = &[
    "_nvenc",
    "_cuvid",
    "_qsv",
    "_vaapi",
    "_videotoolbox",
    "_amf",
    "_mf",
    "_v4l2m2m",
];

/// Parse names from `ffmpeg -encoders` or `ffmpeg -decoders` output,
/// returning only hardware-accelerated implementations.
///
/// The format mirrors `-codecs`: a flag block, then a name, after a
/// `------` separator.
pub fn parse_hw_implementations(output: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut past_separator = false;

    for line in output.lines() {
        if line.starts_with(" ------") {
            past_separator = true;
            continue;
        }
        if !past_separator {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.len() < 8 {
            continue;
        }
        let rest = trimmed[6..].trim_start();
        let name = rest.split_whitespace().next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if HW_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            result.push(name.to_string());
        }
    }

    result
}

/// Parse `ffmpeg -hwaccels` output into a list of hardware acceleration names.
///
/// Lines after "Hardware acceleration methods:" are individual backend names.
pub fn parse_hwaccels(output: &str) -> Vec<String> {
    let mut accels = Vec::new();
    let mut past_header = false;

    for line in output.lines() {
        if line.contains("Hardware acceleration methods:") {
            past_header = true;
            continue;
        }
        if past_header {
            let name = line.trim();
            if !name.is_empty() {
                accels.push(name.to_string());
            }
        }
    }

    accels
}

/// Test whether an ffmpeg HW encoder actually works on the current device.
///
/// Tries to encode a single frame from a synthetic source. Returns `false`
/// when the encoder is compiled into ffmpeg but the GPU/device doesn't
/// support it (e.g. `av1_nvenc` on a GPU without AV1 NVENC capability).
///
/// Uses 256x256 to satisfy NVENC minimum resolution requirements.
pub fn validate_hw_encoder(encoder: &str) -> bool {
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=256x256:d=0.04",
            "-frames:v",
            "1",
            "-c:v",
            encoder,
            "-f",
            "null",
            "-",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        tracing::debug!(encoder, "HW encoder validated");
    } else {
        tracing::info!(
            encoder,
            "HW encoder not supported by device, will use software fallback"
        );
    }

    ok
}

/// A detected GPU or render device, backend-agnostic.
#[derive(Debug, Clone)]
pub struct GpuDevice {
    /// Device identifier (e.g. "0" for NVIDIA, "/dev/dri/renderD128" for VA-API).
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// VRAM in MiB, if known.
    pub vram_mib: Option<u64>,
}

/// Enumerate GPUs for the given HW acceleration backend.
///
/// Returns an empty vec if the required tool is missing or enumeration fails.
pub fn enumerate_gpus(backend: HwAccelBackend) -> Vec<GpuDevice> {
    match backend {
        HwAccelBackend::Nvenc => enumerate_nvidia_gpus(),
        HwAccelBackend::Vaapi | HwAccelBackend::Qsv => enumerate_vaapi_devices(),
        HwAccelBackend::Videotoolbox => {
            vec![GpuDevice {
                id: "default".to_string(),
                name: "macOS GPU".to_string(),
                vram_mib: None,
            }]
        }
    }
}

fn enumerate_nvidia_gpus() -> Vec<GpuDevice> {
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            parse_nvidia_smi(&stdout)
        }
        _ => Vec::new(),
    }
}

fn enumerate_vaapi_devices() -> Vec<GpuDevice> {
    let entries = match std::fs::read_dir("/dev/dri") {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut devices: Vec<GpuDevice> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("renderD") {
            continue;
        }
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();

        let device_name = std::process::Command::new("vainfo")
            .args(["--display", "drm", "--device", &path_str])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                parse_vainfo_device_name(&stdout)
            })
            .unwrap_or_else(|| name_str.to_string());

        devices.push(GpuDevice {
            id: path_str,
            name: device_name,
            vram_mib: None,
        });
    }
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    devices
}

/// Parse `nvidia-smi` CSV output into GPU devices.
///
/// Expected format (one line per GPU):
/// ```text
/// 0, NVIDIA RTX A6000, 49140
/// 1, Quadro RTX 4000, 8192
/// ```
pub fn parse_nvidia_smi(output: &str) -> Vec<GpuDevice> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, ',').collect();
        if parts.len() < 2 {
            continue;
        }
        let id = parts[0].trim().to_string();
        let name = parts[1].trim().to_string();
        let vram_mib = parts.get(2).and_then(|v| v.trim().parse::<u64>().ok());

        devices.push(GpuDevice { id, name, vram_mib });
    }
    devices
}

/// Extract the device name from `vainfo` output.
///
/// Looks for a line like `Driver version: Intel iHD driver - 24.1.0`
/// or `vainfo: Driver version: Mesa Gallium driver 23.3.1 ...`
pub fn parse_vainfo_device_name(output: &str) -> Option<String> {
    for line in output.lines() {
        if line.contains("Driver version") {
            let after_colon = line.rsplit(':').next()?.trim();
            if !after_colon.is_empty() {
                return Some(after_colon.to_string());
            }
        }
    }
    None
}

/// Test whether an ffmpeg HW encoder works on a specific device.
///
/// Like [`validate_hw_encoder`] but targets a specific GPU/render device:
/// - **Nvenc**: sets `CUDA_VISIBLE_DEVICES` env var
/// - **Vaapi**: adds `-vaapi_device <path> -vf format=nv12,hwupload`
/// - **Qsv**: adds `-qsv_device <path>`
/// - **Videotoolbox**: delegates to [`validate_hw_encoder`]
pub fn validate_hw_encoder_on_device(
    encoder: &str,
    backend: HwAccelBackend,
    device: &GpuDevice,
) -> bool {
    match backend {
        HwAccelBackend::Nvenc => {
            let ok = std::process::Command::new("ffmpeg")
                .env("CUDA_VISIBLE_DEVICES", &device.id)
                .args([
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.04",
                    "-frames:v",
                    "1",
                    "-c:v",
                    encoder,
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                tracing::debug!(
                    encoder, gpu = %device.id,
                    "HW encoder validated on device"
                );
            }
            ok
        }
        HwAccelBackend::Vaapi => {
            let filter = "format=nv12,hwupload";
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.04",
                    "-vaapi_device",
                    &device.id,
                    "-vf",
                    filter,
                    "-frames:v",
                    "1",
                    "-c:v",
                    encoder,
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                tracing::debug!(
                    encoder, device = %device.id,
                    "HW encoder validated on device"
                );
            }
            ok
        }
        HwAccelBackend::Qsv => {
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.04",
                    "-qsv_device",
                    &device.id,
                    "-frames:v",
                    "1",
                    "-c:v",
                    encoder,
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                tracing::debug!(
                    encoder, device = %device.id,
                    "HW encoder validated on device"
                );
            }
            ok
        }
        HwAccelBackend::Videotoolbox => validate_hw_encoder(encoder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codecs() {
        let output = "\
Codecs:
 -------
 DEVIL. h264                 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
 DEV.L. hevc                 H.265 / HEVC
 D.A.L. aac                  AAC (Advanced Audio Coding)
 .EA.L. opus                 Opus (Opus Interactive Audio Codec)
 ..S... srt                  SubRip subtitle
";
        let caps = parse_codecs(output);
        assert!(caps.decoders.contains(&"h264".to_string()));
        assert!(caps.decoders.contains(&"hevc".to_string()));
        assert!(caps.decoders.contains(&"aac".to_string()));
        assert!(!caps.decoders.contains(&"opus".to_string()));
        assert!(caps.encoders.contains(&"h264".to_string()));
        assert!(caps.encoders.contains(&"hevc".to_string()));
        assert!(caps.encoders.contains(&"opus".to_string()));
        assert!(!caps.encoders.contains(&"aac".to_string()));
    }

    #[test]
    fn test_parse_codecs_empty_output() {
        let caps = parse_codecs("");
        assert!(caps.decoders.is_empty());
        assert!(caps.encoders.is_empty());
    }

    #[test]
    fn test_parse_formats() {
        let output = "\
File formats:
 -------
 DE matroska,webm  Matroska / WebM
  E mp4            MP4 (MPEG-4 Part 14)
 D  avi            AVI (Audio Video Interleaved)
 DE flac           raw FLAC
";
        let formats = parse_formats(output);
        assert!(formats.contains(&"matroska".to_string()));
        assert!(formats.contains(&"webm".to_string()));
        assert!(formats.contains(&"mp4".to_string()));
        assert!(formats.contains(&"avi".to_string()));
        assert!(formats.contains(&"flac".to_string()));
    }

    #[test]
    fn test_parse_formats_empty_output() {
        let formats = parse_formats("");
        assert!(formats.is_empty());
    }

    #[test]
    fn test_parse_hwaccels() {
        let output = "\
Hardware acceleration methods:
videotoolbox
cuda
vaapi
";
        let accels = parse_hwaccels(output);
        assert_eq!(accels, vec!["videotoolbox", "cuda", "vaapi"]);
    }

    #[test]
    fn test_parse_hwaccels_empty_output() {
        let accels = parse_hwaccels("");
        assert!(accels.is_empty());
    }

    #[test]
    fn test_parse_hwaccels_no_methods() {
        let output = "Hardware acceleration methods:\n";
        let accels = parse_hwaccels(output);
        assert!(accels.is_empty());
    }

    #[test]
    fn test_parse_hw_implementations_encoders() {
        let output = "\
Encoders:
 V..... = Video
 ------
 V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 V....D h264_vaapi           H.264/AVC (VAAPI) (codec h264)
 V....D hevc_nvenc           NVIDIA NVENC hevc encoder (codec hevc)
 V..... hevc_qsv             HEVC (Intel Quick Sync Video acceleration) (codec hevc)
 V....D av1_nvenc            NVIDIA NVENC av1 encoder (codec av1)
 V....D av1_amf              AMD AMF AV1 encoder (codec av1)
 A....D aac                  AAC (Advanced Audio Coding)
";
        let hw = parse_hw_implementations(output);
        assert_eq!(
            hw,
            vec![
                "h264_nvenc",
                "h264_vaapi",
                "hevc_nvenc",
                "hevc_qsv",
                "av1_nvenc",
                "av1_amf",
            ]
        );
        // Software encoders excluded
        assert!(!hw.contains(&"libx264".to_string()));
        assert!(!hw.contains(&"aac".to_string()));
    }

    #[test]
    fn test_parse_hw_implementations_decoders() {
        let output = "\
Decoders:
 ------
 V....D h264                 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
 V....D h264_cuvid           Nvidia CUVID H264 decoder (codec h264)
 V....D h264_qsv             H264 video (Intel Quick Sync Video acceleration) (codec h264)
 V....D hevc                 HEVC (High Efficiency Video Coding)
";
        let hw = parse_hw_implementations(output);
        assert_eq!(hw, vec!["h264_cuvid", "h264_qsv"]);
    }

    #[test]
    fn test_parse_hw_implementations_empty() {
        let hw = parse_hw_implementations("");
        assert!(hw.is_empty());
    }

    #[test]
    fn test_parse_nvidia_smi() {
        let output = "\
0, NVIDIA RTX A6000, 49140
1, Quadro RTX 4000, 8192
";
        let gpus = parse_nvidia_smi(output);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].id, "0");
        assert_eq!(gpus[0].name, "NVIDIA RTX A6000");
        assert_eq!(gpus[0].vram_mib, Some(49140));
        assert_eq!(gpus[1].id, "1");
        assert_eq!(gpus[1].name, "Quadro RTX 4000");
        assert_eq!(gpus[1].vram_mib, Some(8192));
    }

    #[test]
    fn test_parse_nvidia_smi_empty() {
        let gpus = parse_nvidia_smi("");
        assert!(gpus.is_empty());
    }

    #[test]
    fn test_parse_nvidia_smi_no_vram() {
        let output = "\
0, NVIDIA RTX A6000
1, Quadro RTX 4000
";
        let gpus = parse_nvidia_smi(output);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "NVIDIA RTX A6000");
        assert!(gpus[0].vram_mib.is_none());
        assert_eq!(gpus[1].name, "Quadro RTX 4000");
        assert!(gpus[1].vram_mib.is_none());
    }

    #[test]
    fn test_parse_nvidia_smi_malformed() {
        let output = "\
garbage line
0, NVIDIA RTX A6000, 49140
just-one-field
";
        let gpus = parse_nvidia_smi(output);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA RTX A6000");
    }

    #[test]
    fn test_parse_vainfo_device_name() {
        let output = "\
vainfo: VA-API version: 1.20 (libva 2.20.1)
vainfo: Driver version: Intel iHD driver - 24.1.0
vainfo: Supported profile and entrypoint
";
        let name = parse_vainfo_device_name(output);
        assert_eq!(name.as_deref(), Some("Intel iHD driver - 24.1.0"));
    }

    #[test]
    fn test_parse_vainfo_device_name_not_found() {
        let output = "some random output\nwithout driver info\n";
        let name = parse_vainfo_device_name(output);
        assert!(name.is_none());
    }
}
