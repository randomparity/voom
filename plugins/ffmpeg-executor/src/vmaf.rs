//! VMAF measurement helpers for `FFmpeg`-based quality scoring.

use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const VMAF_MODEL_DIRS: &[&str] = &[
    "/usr/share/model",
    "/usr/local/share/model",
    "/opt/homebrew/share/libvmaf/model",
];

/// VMAF model presets supported by the measurement primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmafModel {
    V061,
    V4k,
    Phone,
}

impl VmafModel {
    fn file_name(self) -> &'static str {
        match self {
            Self::V061 => "vmaf_v0.6.1.json",
            Self::V4k => "vmaf_4k_v0.6.1.json",
            Self::Phone => "vmaf_phone_v0.6.1.json",
        }
    }
}

/// Errors returned by VMAF measurement.
#[derive(Debug)]
pub enum VmafError {
    LibvmafUnavailable,
    ModelNotFound(VmafModel),
    FfmpegFailed { exit_status: i32, stderr: String },
    ParseFailed(String),
    Io(std::io::Error),
}

impl Display for VmafError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibvmafUnavailable => write!(f, "ffmpeg does not support libvmaf"),
            Self::ModelNotFound(model) => write!(f, "VMAF model not found: {model:?}"),
            Self::FfmpegFailed {
                exit_status,
                stderr,
            } => {
                write!(
                    f,
                    "ffmpeg libvmaf failed with exit status {exit_status}: {stderr}"
                )
            }
            Self::ParseFailed(message) => write!(f, "failed to parse VMAF JSON: {message}"),
            Self::Io(error) => write!(f, "I/O error during VMAF measurement: {error}"),
        }
    }
}

impl std::error::Error for VmafError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for VmafError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Sample extraction error.
#[derive(Debug)]
pub enum SampleError {
    InvalidInput(String),
    FfmpegFailed { exit_status: i32, stderr: String },
    ParseFailed(String),
    Io(std::io::Error),
}

impl Display for SampleError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid sample request: {message}"),
            Self::FfmpegFailed {
                exit_status,
                stderr,
            } => {
                write!(
                    f,
                    "ffmpeg sample extraction failed with exit status {exit_status}: {stderr}"
                )
            }
            Self::ParseFailed(message) => write!(f, "sample extraction parse error: {message}"),
            Self::Io(error) => write!(f, "sample extraction I/O error: {error}"),
        }
    }
}

impl std::error::Error for SampleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidInput(_) | Self::FfmpegFailed { .. } | Self::ParseFailed(_) => None,
        }
    }
}

impl From<std::io::Error> for SampleError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Extract a representative video sample.
pub trait SampleExtractor {
    fn extract(&self, source: &Path, dest: &Path) -> Result<(), SampleError>;
}

/// Use the full source file as the sample.
pub struct FullSample;

/// Uniformly spaced sample extraction placeholder for E6.
pub struct UniformSample {
    pub count: u32,
    pub duration_secs: u32,
}

/// Scene-based sample extraction placeholder for E6.
pub struct SceneSample {
    pub count: u32,
    pub duration_secs: u32,
}

impl SampleExtractor for FullSample {
    fn extract(&self, source: &Path, dest: &Path) -> Result<(), SampleError> {
        std::fs::copy(source, dest)?;
        Ok(())
    }
}

impl SampleExtractor for UniformSample {
    fn extract(&self, source: &Path, dest: &Path) -> Result<(), SampleError> {
        validate_sample_request(self.count, self.duration_secs)?;
        let duration = ffprobe_duration(source)?;
        let plan = uniform_sample_plan(duration, self.count, self.duration_secs)?;
        extract_sample_segments(source, dest, &plan)
    }
}

impl SampleExtractor for SceneSample {
    fn extract(&self, source: &Path, dest: &Path) -> Result<(), SampleError> {
        validate_sample_request(self.count, self.duration_secs)?;
        let duration = ffprobe_duration(source)?;
        let mut timestamps = scene_change_timestamps(source)?;
        if timestamps.is_empty() {
            timestamps = uniform_sample_timestamps(duration, self.count)?;
        }
        let plan = timestamp_sample_plan(duration, self.count, self.duration_secs, &timestamps)?;
        extract_sample_segments(source, dest, &plan)
    }
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    start_secs: f64,
    duration_secs: f64,
}

fn validate_sample_request(count: u32, duration_secs: u32) -> Result<(), SampleError> {
    if count == 0 {
        return Err(SampleError::InvalidInput(
            "count must be greater than zero".to_string(),
        ));
    }
    if duration_secs == 0 {
        return Err(SampleError::InvalidInput(
            "duration_secs must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn ffprobe_duration(path: &Path) -> Result<f64, SampleError> {
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-show_entries".to_string(),
        "format=duration".to_string(),
        "-of".to_string(),
        "default=noprint_wrappers=1:nokey=1".to_string(),
        path.display().to_string(),
    ];
    let output = voom_process::run_with_timeout("ffprobe", &args, Duration::from_secs(30))
        .map_err(|error| SampleError::FfmpegFailed {
            exit_status: -1,
            stderr: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(sample_ffmpeg_failed(output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let duration = text
        .trim()
        .parse::<f64>()
        .map_err(|error| SampleError::ParseFailed(error.to_string()))?;
    if duration.is_finite() && duration > 0.0 {
        Ok(duration)
    } else {
        Err(SampleError::ParseFailed(format!(
            "invalid source duration: {duration}"
        )))
    }
}

fn uniform_sample_plan(
    source_duration_secs: f64,
    count: u32,
    duration_secs: u32,
) -> Result<Vec<Segment>, SampleError> {
    let timestamps = uniform_sample_timestamps(source_duration_secs, count)?;
    timestamp_sample_plan(source_duration_secs, count, duration_secs, &timestamps)
}

fn uniform_sample_timestamps(
    source_duration_secs: f64,
    count: u32,
) -> Result<Vec<f64>, SampleError> {
    if count == 0 {
        return Err(SampleError::InvalidInput(
            "count must be greater than zero".to_string(),
        ));
    }
    let count_f64 = f64::from(count);
    Ok((0..count)
        .map(|index| source_duration_secs * (f64::from(index) + 0.5) / count_f64)
        .collect())
}

fn timestamp_sample_plan(
    source_duration_secs: f64,
    count: u32,
    duration_secs: u32,
    timestamps: &[f64],
) -> Result<Vec<Segment>, SampleError> {
    if count == 0 {
        return Err(SampleError::InvalidInput(
            "count must be greater than zero".to_string(),
        ));
    }
    if duration_secs == 0 {
        return Err(SampleError::InvalidInput(
            "duration_secs must be greater than zero".to_string(),
        ));
    }
    let requested_duration_secs = f64::from(count) * f64::from(duration_secs);
    if source_duration_secs <= requested_duration_secs {
        return Ok(vec![Segment {
            start_secs: 0.0,
            duration_secs: source_duration_secs,
        }]);
    }
    let mut segments = Vec::new();
    for timestamp in timestamps.iter().copied().take(count as usize) {
        if !timestamp.is_finite() || timestamp >= source_duration_secs {
            continue;
        }
        let duration_secs = f64::from(duration_secs).min(source_duration_secs - timestamp);
        if duration_secs > 0.0 {
            segments.push(Segment {
                start_secs: timestamp.max(0.0),
                duration_secs,
            });
        }
    }
    if segments.is_empty() {
        return Err(SampleError::ParseFailed(
            "no valid sample timestamps found".to_string(),
        ));
    }
    Ok(segments)
}

fn scene_change_timestamps(source: &Path) -> Result<Vec<f64>, SampleError> {
    let args = vec![
        "-hide_banner".to_string(),
        "-i".to_string(),
        source.display().to_string(),
        "-lavfi".to_string(),
        r"select=gt(scene\,0.4),metadata=print:file=-".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let output =
        voom_process::run_with_timeout("ffmpeg", &args, FFMPEG_TIMEOUT).map_err(|error| {
            SampleError::FfmpegFailed {
                exit_status: -1,
                stderr: error.to_string(),
            }
        })?;
    if !output.status.success() {
        return Err(sample_ffmpeg_failed(output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_scene_metadata(&stdout))
}

fn parse_scene_metadata(metadata: &str) -> Vec<f64> {
    let mut scenes = Vec::new();
    let mut pending_time = None;
    for line in metadata.lines() {
        if let Some(time) = parse_scene_pts_time(line) {
            pending_time = Some(time);
            continue;
        }
        if let Some(score) = line
            .strip_prefix("lavfi.scene_score=")
            .and_then(|value| value.parse::<f64>().ok())
        {
            if let Some(time) = pending_time.take() {
                scenes.push((time, score));
            }
        }
    }
    scenes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scenes.into_iter().map(|(time, _score)| time).collect()
}

fn parse_scene_pts_time(line: &str) -> Option<f64> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix("pts_time:"))
        .and_then(|value| value.parse::<f64>().ok())
}

fn extract_sample_segments(
    source: &Path,
    dest: &Path,
    segments: &[Segment],
) -> Result<(), SampleError> {
    let work_dir = std::env::temp_dir().join(format!("voom-sample-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work_dir)?;
    let result = extract_sample_segments_in_dir(source, dest, segments, &work_dir);
    let cleanup = std::fs::remove_dir_all(&work_dir);
    result?;
    cleanup?;
    Ok(())
}

fn extract_sample_segments_in_dir(
    source: &Path,
    dest: &Path,
    segments: &[Segment],
    work_dir: &Path,
) -> Result<(), SampleError> {
    let mut segment_paths = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let path = work_dir.join(format!("segment-{index:03}.mkv"));
        extract_segment(source, &path, *segment)?;
        segment_paths.push(path);
    }
    ffmpeg_concat(&segment_paths, dest, work_dir)
}

fn extract_segment(source: &Path, dest: &Path, segment: Segment) -> Result<(), SampleError> {
    let args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-ss".to_string(),
        format_timestamp(segment.start_secs),
        "-i".to_string(),
        source.display().to_string(),
        "-t".to_string(),
        format_timestamp(segment.duration_secs),
        "-map".to_string(),
        "0:v:0".to_string(),
        "-an".to_string(),
        "-c:v".to_string(),
        "ffv1".to_string(),
        dest.display().to_string(),
    ];
    run_ffmpeg_sample_command(&args)
}

fn ffmpeg_concat(segments: &[PathBuf], dest: &Path, work_dir: &Path) -> Result<(), SampleError> {
    let list_path = work_dir.join("segments.txt");
    let list = segments
        .iter()
        .map(|segment| format!("file '{}'\n", concat_file_path(segment)))
        .collect::<String>();
    std::fs::write(&list_path, list)?;
    let args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "concat".to_string(),
        "-safe".to_string(),
        "0".to_string(),
        "-i".to_string(),
        list_path.display().to_string(),
        "-c".to_string(),
        "copy".to_string(),
        dest.display().to_string(),
    ];
    run_ffmpeg_sample_command(&args)
}

fn run_ffmpeg_sample_command(args: &[String]) -> Result<(), SampleError> {
    let output =
        voom_process::run_with_timeout("ffmpeg", args, FFMPEG_TIMEOUT).map_err(|error| {
            SampleError::FfmpegFailed {
                exit_status: -1,
                stderr: error.to_string(),
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(sample_ffmpeg_failed(output))
    }
}

fn sample_ffmpeg_failed(output: Output) -> SampleError {
    SampleError::FfmpegFailed {
        exit_status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn format_timestamp(seconds: f64) -> String {
    format!("{seconds:.6}")
}

fn concat_file_path(path: &Path) -> String {
    path.display().to_string().replace('\'', r"'\''")
}

#[derive(Debug)]
struct VmafEnvironment {
    libvmaf_supported: bool,
    model_dir: Option<PathBuf>,
}

/// Pick the default VMAF model for a video resolution.
#[must_use]
pub fn pick_model_for_resolution(width: u32, height: u32) -> VmafModel {
    let short_edge = width.min(height);
    let long_edge = width.max(height);
    if short_edge >= 2160 || long_edge >= 3840 {
        VmafModel::V4k
    } else if width <= 1080 && height > width {
        VmafModel::Phone
    } else {
        VmafModel::V061
    }
}

/// Compute the pooled VMAF score for a reference/distorted video pair.
///
/// # Errors
/// Returns a typed error when `ffmpeg` lacks libvmaf support, the requested
/// model is missing, the subprocess fails, or its JSON output cannot be parsed.
pub fn compute_vmaf(
    reference: &Path,
    distorted: &Path,
    model: VmafModel,
) -> Result<f64, VmafError> {
    let env = detect_vmaf_environment();
    compute_vmaf_with_environment(reference, distorted, model, &env)
}

fn compute_vmaf_with_environment(
    reference: &Path,
    distorted: &Path,
    model: VmafModel,
    env: &VmafEnvironment,
) -> Result<f64, VmafError> {
    if !env.libvmaf_supported {
        return Err(VmafError::LibvmafUnavailable);
    }
    let model_path = model_path(model, env)?;
    let log_path = std::env::temp_dir().join(format!("voom-vmaf-{}.json", uuid::Uuid::new_v4()));
    let args = ffmpeg_vmaf_args(reference, distorted, model_path.as_deref(), &log_path);
    let output = voom_process::run_with_timeout("ffmpeg", &args, FFMPEG_TIMEOUT);
    match output {
        Ok(output) if output.status.success() => {
            let stdout_score = parse_vmaf_score(&output.stdout);
            let result = match stdout_score {
                Ok(score) => Ok(score),
                Err(_) => parse_vmaf_score(&std::fs::read(&log_path)?),
            };
            let _ = std::fs::remove_file(&log_path);
            result
        }
        Ok(output) => {
            let _ = std::fs::remove_file(&log_path);
            Err(ffmpeg_failed(output))
        }
        Err(voom_domain::errors::VoomError::ToolNotFound { .. }) => {
            let _ = std::fs::remove_file(&log_path);
            Err(VmafError::LibvmafUnavailable)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&log_path);
            Err(VmafError::FfmpegFailed {
                exit_status: -1,
                stderr: error.to_string(),
            })
        }
    }
}

fn detect_vmaf_environment() -> VmafEnvironment {
    VmafEnvironment {
        libvmaf_supported: ffmpeg_reports_libvmaf(),
        model_dir: resolve_vmaf_model_dir(),
    }
}

fn ffmpeg_reports_libvmaf() -> bool {
    let filters = run_ffmpeg_probe(&["-hide_banner", "-filters"]);
    if filters.as_deref().is_some_and(output_reports_libvmaf) {
        return true;
    }
    run_ffmpeg_probe(&["-version"])
        .as_deref()
        .is_some_and(output_reports_libvmaf)
}

fn run_ffmpeg_probe(args: &[&str]) -> Option<String> {
    let output = voom_process::run_with_timeout("ffmpeg", args, Duration::from_secs(5)).ok()?;
    if !output.status.success() {
        return None;
    }
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(combined)
}

fn output_reports_libvmaf(output: &str) -> bool {
    output.contains("--enable-libvmaf")
        || output
            .lines()
            .any(|line| line.split_whitespace().any(|token| token == "libvmaf"))
}

fn resolve_vmaf_model_dir() -> Option<PathBuf> {
    model_dir_candidates()
        .into_iter()
        .find(|candidate| candidate.is_dir())
}

fn model_dir_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = VMAF_MODEL_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".config/voom/vmaf-models"));
    }
    candidates
}

fn model_path(model: VmafModel, env: &VmafEnvironment) -> Result<Option<PathBuf>, VmafError> {
    let Some(model_dir) = &env.model_dir else {
        return if model == VmafModel::V061 {
            Ok(None)
        } else {
            Err(VmafError::ModelNotFound(model))
        };
    };
    let path = model_dir.join(model.file_name());
    if path.is_file() {
        Ok(Some(path))
    } else if model == VmafModel::V061 {
        Ok(None)
    } else {
        Err(VmafError::ModelNotFound(model))
    }
}

fn ffmpeg_vmaf_args(
    reference: &Path,
    distorted: &Path,
    model_path: Option<&Path>,
    log_path: &Path,
) -> Vec<String> {
    let filter = if let Some(model_path) = model_path {
        format!(
            "libvmaf=model=path={}:log_path={}:log_fmt=json",
            model_path.display(),
            log_path.display()
        )
    } else {
        format!("libvmaf=log_path={}:log_fmt=json", log_path.display())
    };
    vec![
        "-hide_banner".to_string(),
        "-i".to_string(),
        reference.display().to_string(),
        "-i".to_string(),
        distorted.display().to_string(),
        "-lavfi".to_string(),
        filter,
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ]
}

fn ffmpeg_failed(output: Output) -> VmafError {
    VmafError::FfmpegFailed {
        exit_status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn parse_vmaf_score(stdout: &[u8]) -> Result<f64, VmafError> {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|e| VmafError::ParseFailed(e.to_string()))?;
    value
        .pointer("/pooled_metrics/vmaf/mean")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| VmafError::ParseFailed("missing pooled_metrics.vmaf.mean".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pooled_vmaf_mean() {
        let json = br#"{"pooled_metrics":{"vmaf":{"mean":91.25}}}"#;

        let score = parse_vmaf_score(json).unwrap();

        assert_eq!(score, 91.25);
    }

    #[test]
    fn parse_fails_when_mean_is_missing() {
        let err = parse_vmaf_score(br#"{"frames":[]}"#).unwrap_err();

        assert!(matches!(err, VmafError::ParseFailed(_)));
    }

    #[test]
    fn environment_without_libvmaf_returns_unavailable_before_model_lookup() {
        let env = VmafEnvironment {
            libvmaf_supported: false,
            model_dir: None,
        };

        let err = compute_vmaf_with_environment(
            Path::new("reference.mkv"),
            Path::new("distorted.mkv"),
            VmafModel::V061,
            &env,
        )
        .unwrap_err();

        assert!(matches!(err, VmafError::LibvmafUnavailable));
    }

    #[test]
    fn missing_default_model_uses_libvmaf_builtin_default() {
        let dir = tempfile::tempdir().unwrap();
        let env = VmafEnvironment {
            libvmaf_supported: true,
            model_dir: Some(dir.path().to_path_buf()),
        };

        let err = compute_vmaf_with_environment(
            Path::new("reference.mkv"),
            Path::new("distorted.mkv"),
            VmafModel::V061,
            &env,
        )
        .unwrap_err();

        assert!(matches!(err, VmafError::FfmpegFailed { .. }));
    }

    #[test]
    fn missing_non_default_model_returns_model_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let env = VmafEnvironment {
            libvmaf_supported: true,
            model_dir: Some(dir.path().to_path_buf()),
        };

        let err = compute_vmaf_with_environment(
            Path::new("reference.mkv"),
            Path::new("distorted.mkv"),
            VmafModel::V4k,
            &env,
        )
        .unwrap_err();

        assert!(matches!(err, VmafError::ModelNotFound(VmafModel::V4k)));
    }
}
