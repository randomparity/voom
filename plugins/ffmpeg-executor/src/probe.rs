//! `FFmpeg` capability probing: parse output from `ffmpeg -codecs`, `-formats`, `-hwaccels`.

use crate::hwaccel::HwAccelBackend;
use rayon::prelude::*;
use std::time::Duration;
use voom_domain::events::CodecCapabilities;

/// Upper bound for a single hardware-encoder validation invocation.
/// On healthy hardware these finish well under 1 s; the 5 s cap keeps a
/// wedged GPU driver or stuck device node from hanging a rayon worker.
const HW_ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound for `nvidia-smi` / `vainfo` enumeration calls. Normally
/// millisecond operations; 10 s leaves headroom for first-init or
/// remote-display paths while still bounding pathological hangs.
const TOOL_ENUMERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound for a single fast capability probe (`-codecs`, `-formats`,
/// `-hwaccels`, `-encoders`, `-decoders`). These calls don't touch the GPU
/// and normally finish in under 200 ms; 5 s caps a pathological hang.
const CAPABILITY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Aggregate result of running the fast `ffmpeg` capability probes
/// concurrently.
///
/// `codecs == None` is the canonical "ffmpeg is unavailable" signal —
/// callers should disable the plugin in that case. When `codecs` is
/// `Some`, its `hw_encoders` and `hw_decoders` fields are populated from
/// separate `-encoders` / `-decoders` probes. `formats` and `hw_accels`
/// degrade independently to empty `Vec`s on probe failure, with a
/// `tracing::warn!` recording the cause.
pub struct FfmpegCapabilities {
    pub codecs: Option<CodecCapabilities>,
    pub formats: Vec<String>,
    pub hw_accels: Vec<String>,
}

/// Run a single `ffmpeg <flag> -hide_banner` probe and return its stdout
/// as a UTF-8 string on success. On spawn error, non-zero exit, or
/// timeout, logs a warning naming `flag` and returns `None`.
fn run_capability_probe(tool: &str, flag: &str) -> Option<String> {
    match voom_process::run_with_timeout(tool, &[flag, "-hide_banner"], CAPABILITY_PROBE_TIMEOUT) {
        Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            tracing::warn!(
                tool,
                flag,
                exit = ?out.status.code(),
                "ffmpeg probe exited non-zero"
            );
            None
        }
        Err(e) => {
            tracing::warn!(tool, flag, error = %e, "ffmpeg probe failed");
            None
        }
    }
}

fn probe_codecs(tool: &str) -> Option<CodecCapabilities> {
    run_capability_probe(tool, "-codecs").map(|s| parse_codecs(&s))
}

fn probe_formats(tool: &str) -> Vec<String> {
    run_capability_probe(tool, "-formats")
        .map(|s| parse_formats(&s))
        .unwrap_or_default()
}

/// Probe `tool` (an `ffmpeg` command name or path) for available hardware
/// acceleration backend names via `-hwaccels`. Bounded by
/// `CAPABILITY_PROBE_TIMEOUT`. Returns an empty `Vec` on spawn failure,
/// non-zero exit, or timeout (logged via `tracing::warn!`); empty is also
/// the legitimate "no HW backends" result, so the two are not
/// distinguishable from the return value alone.
#[must_use]
pub fn probe_hwaccels(tool: &str) -> Vec<String> {
    run_capability_probe(tool, "-hwaccels")
        .map(|s| parse_hwaccels(&s))
        .unwrap_or_default()
}

/// Probe `tool` for hardware-accelerated encoder names via `-encoders`,
/// keeping only entries whose suffix matches a known HW backend
/// (`_nvenc`, `_qsv`, `_vaapi`, etc.). See [`probe_hwaccels`] for the
/// failure-mode contract.
#[must_use]
pub fn probe_hw_encoders(tool: &str) -> Vec<String> {
    run_capability_probe(tool, "-encoders")
        .map(|s| parse_hw_implementations(&s))
        .unwrap_or_default()
}

/// Probe `tool` for hardware-accelerated decoder names via `-decoders`,
/// keeping only entries whose suffix matches a known HW backend. See
/// [`probe_hwaccels`] for the failure-mode contract.
#[must_use]
pub fn probe_hw_decoders(tool: &str) -> Vec<String> {
    run_capability_probe(tool, "-decoders")
        .map(|s| parse_hw_implementations(&s))
        .unwrap_or_default()
}

/// Run the five fast `ffmpeg` capability probes concurrently using
/// `rayon::join`. Each probe is independently bounded by
/// `CAPABILITY_PROBE_TIMEOUT`; failures degrade to empty values with a
/// `tracing::warn!` recording the cause.
#[must_use]
pub fn probe_capabilities() -> FfmpegCapabilities {
    probe_capabilities_with_tool("ffmpeg")
}

/// Internal: parameterized variant of `probe_capabilities` taking the
/// tool path. Lets tests drive the failure path with a non-existent
/// binary without manipulating `PATH`.
fn probe_capabilities_with_tool(tool: &str) -> FfmpegCapabilities {
    let ((codecs, formats), (hw_accels, (hw_encoders, hw_decoders))) = rayon::join(
        || rayon::join(|| probe_codecs(tool), || probe_formats(tool)),
        || {
            rayon::join(
                || probe_hwaccels(tool),
                || rayon::join(|| probe_hw_encoders(tool), || probe_hw_decoders(tool)),
            )
        },
    );

    let codecs = codecs.map(|mut c| {
        c.hw_encoders = hw_encoders;
        c.hw_decoders = hw_decoders;
        c
    });

    FfmpegCapabilities {
        codecs,
        formats,
        hw_accels,
    }
}

/// Aggregate result of [`probe_hw_details`].
///
/// `encoders` and `decoders` come from `-encoders` / `-decoders` ffmpeg
/// probes and degrade to empty `Vec`s on probe failure. `devices`
/// follows the per-backend [`enumerate_gpus`] contract (e.g. always
/// non-empty for `Videotoolbox`, dependent on `nvidia-smi` / `vainfo`
/// availability for the others).
pub struct HwDetails {
    pub encoders: Vec<String>,
    pub decoders: Vec<String>,
    pub devices: Vec<GpuDevice>,
}

/// Run [`probe_hw_encoders`], [`probe_hw_decoders`], and
/// [`enumerate_gpus`] concurrently using nested `rayon::join`.
#[must_use]
pub fn probe_hw_details(tool: &str, backend: HwAccelBackend) -> HwDetails {
    let ((encoders, decoders), devices) = rayon::join(
        || rayon::join(|| probe_hw_encoders(tool), || probe_hw_decoders(tool)),
        || enumerate_gpus(backend),
    );
    HwDetails {
        encoders,
        decoders,
        devices,
    }
}

/// Run `tool` with `args` and `env`, returning `true` only if it exits 0
/// before `timeout`. Spawn errors, non-zero exits, and timeouts all yield
/// `false`. Pass `&[]` for `env` when no extra variables are needed.
fn probe_tool_status(tool: &str, args: &[&str], timeout: Duration, env: &[(&str, &str)]) -> bool {
    voom_process::run_with_timeout_env(tool, args, timeout, env)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `tool` with `args`, returning captured stdout only if it exits 0
/// before `timeout`. Spawn errors, non-zero exits, and timeouts all yield
/// `None`.
fn probe_tool_stdout(tool: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    voom_process::run_with_timeout(tool, args, timeout)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
}

/// Parse `ffmpeg -codecs` output into decoder and encoder lists.
///
/// Each codec line (after the `-------` separator) has flags in columns 0-5:
/// `D` = decoding, `E` = encoding. The codec name follows after whitespace.
#[must_use]
fn parse_codecs(output: &str) -> CodecCapabilities {
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
#[must_use]
fn parse_formats(output: &str) -> Vec<String> {
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
#[must_use]
fn parse_hw_implementations(output: &str) -> Vec<String> {
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
#[must_use]
fn parse_hwaccels(output: &str) -> Vec<String> {
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

/// Run an encoder validator across all candidates in parallel.
///
/// Each `validate_hw_encoder*` call spawns a fresh `ffmpeg` subprocess that
/// takes ~600ms even when it fails. On a CUDA-only host with ~24 candidates
/// the sequential filter pays ~14s of startup tax. Running the validator on
/// `rayon`'s thread pool collapses that to ~1s.
///
/// `par_iter().filter().collect()` preserves the source order of
/// `encoders`, which keeps the resulting list deterministic.
pub fn validate_hw_encoders_parallel<F>(encoders: &[String], validator: F) -> Vec<String>
where
    F: Fn(&str) -> bool + Sync,
{
    encoders
        .par_iter()
        .filter(|enc| validator(enc.as_str()))
        .cloned()
        .collect()
}

/// Run an encoder validator across all candidates in parallel, returning
/// `(name, ok)` pairs in source order.
///
/// Same parallelization rationale as [`validate_hw_encoders_parallel`], but
/// keeps both passing and failing entries so callers can render per-encoder
/// status (e.g. `OK` vs `UNSUPPORTED`) instead of a filtered list.
///
/// `par_iter().map().collect()` preserves the source order of `encoders`,
/// which keeps grouped output deterministic.
pub fn validate_hw_encoders_parallel_with_status<F>(
    encoders: &[String],
    validator: F,
) -> Vec<(String, bool)>
where
    F: Fn(&str) -> bool + Sync,
{
    encoders
        .par_iter()
        .map(|enc| (enc.clone(), validator(enc.as_str())))
        .collect()
}

/// Test whether an ffmpeg HW encoder actually works on the current device.
///
/// Tries to encode a single frame from a synthetic source. Returns `false`
/// when the encoder is compiled into ffmpeg but the GPU/device doesn't
/// support it (e.g. `av1_nvenc` on a GPU without AV1 NVENC capability).
///
/// Uses 256x256 to satisfy NVENC minimum resolution requirements.
pub fn validate_hw_encoder(encoder: &str) -> bool {
    let ok = probe_tool_status(
        "ffmpeg",
        &[
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
        ],
        HW_ENCODER_PROBE_TIMEOUT,
        &[],
    );

    if ok {
        tracing::debug!(encoder, "HW encoder validated");
    } else {
        tracing::debug!(
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
#[must_use]
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
    match probe_tool_stdout(
        "nvidia-smi",
        &[
            "--query-gpu=index,name,memory.total",
            "--format=csv,noheader,nounits",
        ],
        TOOL_ENUMERATION_TIMEOUT,
    ) {
        Some(stdout) => {
            let text = String::from_utf8_lossy(&stdout);
            parse_nvidia_smi(&text)
        }
        None => Vec::new(),
    }
}

/// Probe each `(device_path, fallback_name)` candidate concurrently and
/// build a sorted `Vec<GpuDevice>`.
///
/// `probe` is called once per candidate and may run on any rayon worker
/// thread; it must therefore be `Sync`. Returning `None` causes the
/// fallback name to be used (matching the legacy single-threaded
/// behavior). The resulting list is sorted by `id` ascending so output
/// remains deterministic regardless of probe completion order.
fn build_vaapi_devices<F>(candidates: Vec<(String, String)>, probe: F) -> Vec<GpuDevice>
where
    F: Fn(&str) -> Option<String> + Sync,
{
    let mut devices: Vec<GpuDevice> = candidates
        .into_par_iter()
        .map(|(path_str, fallback)| {
            let name = probe(&path_str).unwrap_or(fallback);
            GpuDevice {
                id: path_str,
                name,
                vram_mib: None,
            }
        })
        .collect();
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    devices
}

fn enumerate_vaapi_devices() -> Vec<GpuDevice> {
    let entries = match std::fs::read_dir("/dev/dri") {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    // Collect all render-node candidates first so the per-node `vainfo`
    // calls below can fan out across rayon's pool instead of paying a
    // sequential N × TOOL_ENUMERATION_TIMEOUT worst case on multi-GPU
    // hosts. The directory walk itself is microseconds and stays serial.
    let candidates: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("renderD") {
                return None;
            }
            let path_str = entry.path().to_string_lossy().into_owned();
            Some((path_str, name_str.into_owned()))
        })
        .collect();

    build_vaapi_devices(candidates, |path_str| {
        probe_tool_stdout(
            "vainfo",
            &["--display", "drm", "--device", path_str],
            TOOL_ENUMERATION_TIMEOUT,
        )
        .and_then(|stdout| {
            let text = String::from_utf8_lossy(&stdout);
            parse_vainfo_device_name(&text)
        })
    })
}

/// Check whether NVIDIA GPU hardware is present.
pub(crate) fn has_nvidia_hardware() -> bool {
    probe_tool_status(
        "nvidia-smi",
        &["--list-gpus"],
        TOOL_ENUMERATION_TIMEOUT,
        &[],
    )
}

/// Check whether a working VA-API device exists.
///
/// Finds the first `/dev/dri/renderD*` node and runs `vainfo` against it
/// to confirm the VA-API userspace stack is functional. A render node can
/// exist without working VA-API (e.g., AMD GPU without `mesa-va-drivers`).
///
/// Returns `false` if no render nodes exist or `vainfo` is not installed.
/// Users can bypass this check with `hw_accel = "vaapi"` in config.
pub(crate) fn has_vaapi_devices() -> bool {
    let first_render_node = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("renderD"));

    let Some(entry) = first_render_node else {
        return false;
    };

    let path = entry.path();
    let path_str = path.to_string_lossy();

    let ok = probe_tool_status(
        "vainfo",
        &["--display", "drm", "--device", path_str.as_ref()],
        TOOL_ENUMERATION_TIMEOUT,
        &[],
    );

    if ok {
        tracing::debug!(device = %path_str, "VA-API device verified via vainfo");
    } else {
        tracing::info!(
            device = %path_str,
            "VA-API render node exists but vainfo failed — \
             missing drivers or vainfo not installed"
        );
    }

    ok
}

/// Check whether Intel GPU hardware is present (for QSV).
///
/// Reads `/sys/class/drm/card*/device/vendor` looking for Intel's
/// PCI vendor ID (`0x8086`).
pub(crate) fn has_intel_gpu() -> bool {
    let entries = match std::fs::read_dir("/sys/class/drm") {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }
        let vendor_path = entry.path().join("device/vendor");
        if let Ok(vendor) = std::fs::read_to_string(&vendor_path) {
            if vendor.trim() == "0x8086" {
                return true;
            }
        }
    }
    false
}

/// Parse `nvidia-smi` CSV output into GPU devices.
///
/// Expected format (one line per GPU):
/// ```text
/// 0, NVIDIA RTX A6000, 49140
/// 1, Quadro RTX 4000, 8192
/// ```
#[must_use]
fn parse_nvidia_smi(output: &str) -> Vec<GpuDevice> {
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
#[must_use]
fn parse_vainfo_device_name(output: &str) -> Option<String> {
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
            let ok = probe_tool_status(
                "ffmpeg",
                &[
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
                ],
                HW_ENCODER_PROBE_TIMEOUT,
                &[("CUDA_VISIBLE_DEVICES", device.id.as_str())],
            );
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
            let ok = probe_tool_status(
                "ffmpeg",
                &[
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.04",
                    "-vaapi_device",
                    device.id.as_str(),
                    "-vf",
                    filter,
                    "-frames:v",
                    "1",
                    "-c:v",
                    encoder,
                    "-f",
                    "null",
                    "-",
                ],
                HW_ENCODER_PROBE_TIMEOUT,
                &[],
            );
            if ok {
                tracing::debug!(
                    encoder, device = %device.id,
                    "HW encoder validated on device"
                );
            }
            ok
        }
        HwAccelBackend::Qsv => {
            let ok = probe_tool_status(
                "ffmpeg",
                &[
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.04",
                    "-qsv_device",
                    device.id.as_str(),
                    "-frames:v",
                    "1",
                    "-c:v",
                    encoder,
                    "-f",
                    "null",
                    "-",
                ],
                HW_ENCODER_PROBE_TIMEOUT,
                &[],
            );
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

    #[test]
    fn test_validate_hw_encoders_parallel_preserves_order() {
        let input: Vec<String> = vec![
            "av1_nvenc".into(),
            "h264_nvenc".into(),
            "av1_qsv".into(),
            "hevc_nvenc".into(),
            "vp9_qsv".into(),
        ];
        // Pass-through validator that accepts only `*_nvenc`.
        let result = validate_hw_encoders_parallel(&input, |name| name.ends_with("_nvenc"));
        assert_eq!(
            result,
            vec![
                "av1_nvenc".to_string(),
                "h264_nvenc".to_string(),
                "hevc_nvenc".to_string(),
            ],
            "filter must preserve source order even when run in parallel"
        );
    }

    #[test]
    fn test_validate_hw_encoders_parallel_empty_input() {
        let input: Vec<String> = Vec::new();
        let result = validate_hw_encoders_parallel(&input, |_| true);
        assert!(result.is_empty());
    }

    #[test]
    fn test_validate_hw_encoders_parallel_all_rejected() {
        let input: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let result = validate_hw_encoders_parallel(&input, |_| false);
        assert!(result.is_empty());
    }

    #[test]
    fn test_validate_hw_encoders_parallel_with_status_preserves_order() {
        let input: Vec<String> = vec![
            "av1_nvenc".into(),
            "h264_nvenc".into(),
            "av1_qsv".into(),
            "hevc_nvenc".into(),
            "vp9_qsv".into(),
        ];
        let result =
            validate_hw_encoders_parallel_with_status(&input, |name| name.ends_with("_nvenc"));
        assert_eq!(
            result,
            vec![
                ("av1_nvenc".to_string(), true),
                ("h264_nvenc".to_string(), true),
                ("av1_qsv".to_string(), false),
                ("hevc_nvenc".to_string(), true),
                ("vp9_qsv".to_string(), false),
            ],
            "with_status must preserve source order even when run in parallel"
        );
    }

    #[test]
    fn test_validate_hw_encoders_parallel_with_status_empty_input() {
        let input: Vec<String> = Vec::new();
        let result = validate_hw_encoders_parallel_with_status(&input, |_| true);
        assert!(result.is_empty());
    }

    #[test]
    fn test_validate_hw_encoders_parallel_with_status_all_rejected() {
        let input: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let result = validate_hw_encoders_parallel_with_status(&input, |_| false);
        assert_eq!(
            result,
            vec![
                ("a".to_string(), false),
                ("b".to_string(), false),
                ("c".to_string(), false),
            ],
        );
    }

    #[test]
    fn test_validate_hw_encoders_parallel_with_status_all_accepted() {
        let input: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let result = validate_hw_encoders_parallel_with_status(&input, |_| true);
        assert_eq!(
            result,
            vec![
                ("a".to_string(), true),
                ("b".to_string(), true),
                ("c".to_string(), true),
            ],
        );
    }

    #[test]
    fn probe_hw_details_with_missing_tool_returns_empty_lists() {
        // `devices` is intentionally not asserted: enumerate_gpus follows
        // a different contract (Videotoolbox returns a hardcoded entry).
        let details =
            super::probe_hw_details("/nonexistent/ffmpeg-fake", HwAccelBackend::Videotoolbox);

        assert!(details.encoders.is_empty());
        assert!(details.decoders.is_empty());
    }

    #[test]
    fn probe_capabilities_with_missing_tool_returns_empty_result() {
        // Drive the full failure path: every probe will fail to spawn
        // because the binary doesn't exist. We assert the contract the
        // plugin's init() relies on: codecs is None (the disable signal)
        // and the secondary fields are empty.
        let caps = super::probe_capabilities_with_tool("/nonexistent/ffmpeg-fake");

        assert!(
            caps.codecs.is_none(),
            "missing tool must surface as codecs == None"
        );
        assert!(caps.formats.is_empty());
        assert!(caps.hw_accels.is_empty());
    }

    #[cfg(unix)]
    mod probe_helpers {
        use super::*;
        use std::time::Instant;

        #[test]
        fn returns_true_on_success() {
            let ok = probe_tool_status("true", &[], Duration::from_secs(5), &[]);
            assert!(ok);
        }

        #[test]
        fn returns_false_on_nonzero_exit() {
            let ok = probe_tool_status("false", &[], Duration::from_secs(5), &[]);
            assert!(!ok);
        }

        #[test]
        fn returns_false_on_missing_tool() {
            let ok = probe_tool_status(
                "voom_nonexistent_probe_tool_xyz",
                &[],
                Duration::from_secs(5),
                &[],
            );
            assert!(!ok);
        }

        #[test]
        fn returns_false_on_timeout() {
            let started = Instant::now();
            let ok = probe_tool_status("sleep", &["60"], Duration::from_secs(1), &[]);
            let elapsed = started.elapsed();
            assert!(!ok);
            assert!(
                elapsed < Duration::from_secs(5),
                "probe must return within a few seconds of the timeout, took {elapsed:?}"
            );
        }

        #[test]
        fn passes_environment() {
            let ok = probe_tool_status(
                "sh",
                &["-c", "[ \"$VOOM_PROBE_TEST\" = \"yes\" ]"],
                Duration::from_secs(5),
                &[("VOOM_PROBE_TEST", "yes")],
            );
            assert!(ok);
        }

        #[test]
        fn stdout_captures_on_success() {
            let out = probe_tool_stdout("echo", &["hello"], Duration::from_secs(5))
                .expect("echo should succeed");
            assert_eq!(String::from_utf8_lossy(&out).trim(), "hello");
        }

        #[test]
        fn stdout_none_on_nonzero_exit() {
            let out = probe_tool_stdout("false", &[], Duration::from_secs(5));
            assert!(out.is_none());
        }

        #[test]
        fn stdout_none_on_missing_tool() {
            let out = probe_tool_stdout(
                "voom_nonexistent_probe_tool_xyz",
                &[],
                Duration::from_secs(5),
            );
            assert!(out.is_none());
        }

        #[test]
        fn stdout_none_on_timeout() {
            let started = Instant::now();
            let out = probe_tool_stdout("sleep", &["60"], Duration::from_secs(1));
            let elapsed = started.elapsed();
            assert!(out.is_none());
            assert!(
                elapsed < Duration::from_secs(5),
                "stdout-variant must also honor the timeout, took {elapsed:?}"
            );
        }
    }

    #[test]
    fn build_vaapi_devices_uses_probe_and_falls_back_and_sorts() {
        // Mixed input: one device whose probe returns a driver name, one
        // whose probe returns None (forcing the fallback). Provided in
        // reverse-sorted order so we also exercise the sort.
        let candidates: Vec<(String, String)> = vec![
            ("/dev/dri/renderD129".to_string(), "renderD129".to_string()),
            ("/dev/dri/renderD128".to_string(), "renderD128".to_string()),
        ];

        let probe = |path: &str| -> Option<String> {
            if path == "/dev/dri/renderD128" {
                Some("Intel iHD driver - 24.1.0".to_string())
            } else {
                None
            }
        };

        let devices = build_vaapi_devices(candidates, probe);

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, "/dev/dri/renderD128");
        assert_eq!(devices[0].name, "Intel iHD driver - 24.1.0");
        assert!(devices[0].vram_mib.is_none());
        assert_eq!(devices[1].id, "/dev/dri/renderD129");
        // Fallback path: probe returned None, so name == fallback.
        assert_eq!(devices[1].name, "renderD129");
        assert!(devices[1].vram_mib.is_none());
    }

    #[test]
    fn build_vaapi_devices_runs_probes_in_parallel() {
        // Each fake probe sleeps `per_probe`; sequential execution would
        // therefore take at least `per_probe * n`. We assert elapsed is
        // shorter than that floor by a meaningful margin so a regression
        // to sequential execution still fails the test on a slow CI host.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let per_probe = Duration::from_millis(200);
        let n = 4u32;
        let sequential_floor = per_probe * n;

        let candidates: Vec<(String, String)> = (0..n)
            .map(|i| {
                (
                    format!("/dev/dri/renderD{}", 128 + i),
                    format!("renderD{}", 128 + i),
                )
            })
            .collect();

        let counter = AtomicUsize::new(0);
        let probe = |_path: &str| -> Option<String> {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(per_probe);
            None
        };

        let started = Instant::now();
        let devices = build_vaapi_devices(candidates, probe);
        let elapsed = started.elapsed();

        assert_eq!(devices.len(), n as usize);
        assert_eq!(counter.load(Ordering::SeqCst), n as usize);
        // Allow a 100ms cushion below the sequential floor for a cold
        // rayon pool / busy CI runner; still well clear of sequential.
        let ceiling = sequential_floor - Duration::from_millis(100);
        assert!(
            elapsed < ceiling,
            "probes did not run in parallel: took {elapsed:?} (sequential floor {sequential_floor:?})"
        );
    }
}
