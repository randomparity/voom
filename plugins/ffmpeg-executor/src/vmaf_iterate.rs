//! Iterative VMAF-guided CRF selection for video transcodes.

use std::fmt::{Display, Formatter};
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use crate::vmaf::{
    compute_vmaf, pick_model_for_resolution, SampleError, SampleExtractor, VmafError, VmafModel,
};

const DEFAULT_START_CRF: u32 = 23;
const MIN_CRF: u32 = 0;
const MAX_CRF: u32 = 51;
const TARGET_TOLERANCE: f64 = 2.0;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Optional hard bitrate ceilings for iterative sample encodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitrateBounds {
    pub min_bitrate: Option<String>,
    pub max_bitrate: Option<String>,
}

/// Result from a successful VMAF iteration run.
#[derive(Debug, Clone, PartialEq)]
pub struct IterationResult {
    pub final_crf: u32,
    pub final_bitrate: Option<String>,
    pub achieved_vmaf: f64,
    pub iterations: u32,
}

/// Errors returned by VMAF-guided iteration.
#[derive(Debug)]
pub enum IterationError {
    InvalidInput(String),
    Sample(SampleError),
    Vmaf(VmafError),
    FfmpegFailed { exit_status: i32, stderr: String },
    Io(std::io::Error),
    MaxIterationsExceeded(IterationResult),
}

impl Display for IterationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid VMAF iteration input: {message}"),
            Self::Sample(error) => write!(f, "sample extraction failed: {error}"),
            Self::Vmaf(error) => write!(f, "{error}"),
            Self::FfmpegFailed {
                exit_status,
                stderr,
            } => write!(
                f,
                "ffmpeg sample encode failed with exit status {exit_status}: {stderr}"
            ),
            Self::Io(error) => write!(f, "I/O error during VMAF iteration: {error}"),
            Self::MaxIterationsExceeded(result) => write!(
                f,
                "VMAF iteration did not converge after {} iterations; last score {:.2}",
                result.iterations, result.achieved_vmaf
            ),
        }
    }
}

impl std::error::Error for IterationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sample(error) => Some(error),
            Self::Vmaf(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::InvalidInput(_) | Self::FfmpegFailed { .. } | Self::MaxIterationsExceeded(_) => {
                None
            }
        }
    }
}

impl From<SampleError> for IterationError {
    fn from(error: SampleError) -> Self {
        Self::Sample(error)
    }
}

impl From<VmafError> for IterationError {
    fn from(error: VmafError) -> Self {
        Self::Vmaf(error)
    }
}

impl From<std::io::Error> for IterationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Encodes sample attempts and measures their VMAF scores.
pub trait EncodeAttempt {
    fn encode_sample(
        &self,
        source: &Path,
        dest: &Path,
        crf: u32,
        bounds: BitrateBounds,
    ) -> Result<Option<String>, VmafError>;

    fn compute_vmaf(
        &self,
        reference: &Path,
        distorted: &Path,
        model: VmafModel,
    ) -> Result<f64, VmafError>;
}

struct FfmpegEncodeAttempt;

impl EncodeAttempt for FfmpegEncodeAttempt {
    fn encode_sample(
        &self,
        source: &Path,
        dest: &Path,
        crf: u32,
        bounds: BitrateBounds,
    ) -> Result<Option<String>, VmafError> {
        encode_sample_with_ffmpeg(source, dest, crf, &bounds).map_err(|error| {
            VmafError::FfmpegFailed {
                exit_status: -1,
                stderr: error.to_string(),
            }
        })
    }

    fn compute_vmaf(
        &self,
        reference: &Path,
        distorted: &Path,
        model: VmafModel,
    ) -> Result<f64, VmafError> {
        compute_vmaf(reference, distorted, model)
    }
}

/// Iterate CRF selection until a sample encode reaches the target VMAF.
///
/// # Errors
/// Returns an error when sampling, sample encoding, or VMAF measurement fails,
/// or when the maximum iteration count is exhausted before convergence.
pub fn iterate_to_target(
    source: &Path,
    target_vmaf: u32,
    bounds: BitrateBounds,
    sample: &dyn SampleExtractor,
    max_iterations: u32,
) -> Result<IterationResult, IterationError> {
    iterate_to_target_with(
        source,
        target_vmaf,
        bounds,
        sample,
        max_iterations,
        &FfmpegEncodeAttempt,
    )
}

/// Testable iteration entry point with injected encode/measure behavior.
///
/// # Errors
/// Returns an error when sampling, sample encoding, or VMAF measurement fails,
/// or when the maximum iteration count is exhausted before convergence.
pub fn iterate_to_target_with(
    source: &Path,
    target_vmaf: u32,
    bounds: BitrateBounds,
    sample: &dyn SampleExtractor,
    max_iterations: u32,
    attempt: &dyn EncodeAttempt,
) -> Result<IterationResult, IterationError> {
    validate_inputs(target_vmaf, max_iterations)?;
    let work_dir = std::env::temp_dir().join(format!("voom-vmaf-iterate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work_dir)?;
    let result = iterate_in_dir(
        source,
        target_vmaf,
        bounds,
        sample,
        max_iterations,
        attempt,
        &work_dir,
    );
    cleanup_work_dir(result, &work_dir)
}

fn iterate_in_dir(
    source: &Path,
    target_vmaf: u32,
    bounds: BitrateBounds,
    sample: &dyn SampleExtractor,
    max_iterations: u32,
    attempt: &dyn EncodeAttempt,
    work_dir: &Path,
) -> Result<IterationResult, IterationError> {
    let reference = work_dir.join("reference.mkv");
    sample.extract(source, &reference)?;
    let model = model_for_source(source);
    run_bisection(
        &reference,
        target_vmaf,
        bounds,
        max_iterations,
        attempt,
        model,
        work_dir,
    )
}

fn run_bisection(
    reference: &Path,
    target_vmaf: u32,
    bounds: BitrateBounds,
    max_iterations: u32,
    attempt: &dyn EncodeAttempt,
    model: VmafModel,
    work_dir: &Path,
) -> Result<IterationResult, IterationError> {
    let mut low = MIN_CRF;
    let mut high = MAX_CRF;
    let mut crf = DEFAULT_START_CRF;
    let mut last = None;
    for iteration in 1..=max_iterations {
        let encoded = work_dir.join(format!("encoded-{iteration}.mkv"));
        let bitrate = attempt.encode_sample(reference, &encoded, crf, bounds.clone())?;
        let achieved = attempt.compute_vmaf(reference, &encoded, model)?;
        let result = IterationResult {
            final_crf: crf,
            final_bitrate: bitrate,
            achieved_vmaf: achieved,
            iterations: iteration,
        };
        tracing::info!(
            target = target_vmaf,
            achieved,
            crf,
            iter = iteration,
            "vmaf iteration"
        );
        if (achieved - f64::from(target_vmaf)).abs() <= TARGET_TOLERANCE {
            return Ok(result);
        }
        update_bounds(achieved, target_vmaf, crf, &mut low, &mut high);
        last = Some(result);
        crf = low + ((high - low) / 2);
    }
    Err(IterationError::MaxIterationsExceeded(last.ok_or_else(
        || IterationError::InvalidInput("max_iterations must be greater than zero".to_string()),
    )?))
}

fn cleanup_work_dir(
    result: Result<IterationResult, IterationError>,
    work_dir: &Path,
) -> Result<IterationResult, IterationError> {
    let cleanup = std::fs::remove_dir_all(work_dir);
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(IterationError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

fn model_for_source(source: &Path) -> VmafModel {
    source_resolution(source)
        .map(|(width, height)| pick_model_for_resolution(width, height))
        .unwrap_or(VmafModel::V061)
}

fn source_resolution(source: &Path) -> Option<(u32, u32)> {
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-select_streams".to_string(),
        "v:0".to_string(),
        "-show_entries".to_string(),
        "stream=width,height".to_string(),
        "-of".to_string(),
        "json".to_string(),
        source.display().to_string(),
    ];
    let output = voom_process::run_with_timeout("ffprobe", &args, Duration::from_secs(30)).ok()?;
    parse_resolution_output(output)
}

fn parse_resolution_output(output: Output) -> Option<(u32, u32)> {
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let stream = value.pointer("/streams/0")?;
    let width = stream.get("width")?.as_u64()?.try_into().ok()?;
    let height = stream.get("height")?.as_u64()?.try_into().ok()?;
    Some((width, height))
}

fn validate_inputs(target_vmaf: u32, max_iterations: u32) -> Result<(), IterationError> {
    if !(60..=100).contains(&target_vmaf) {
        return Err(IterationError::InvalidInput(
            "target_vmaf must be from 60 to 100".to_string(),
        ));
    }
    if max_iterations == 0 {
        return Err(IterationError::InvalidInput(
            "max_iterations must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn update_bounds(achieved: f64, target_vmaf: u32, crf: u32, low: &mut u32, high: &mut u32) {
    if achieved < f64::from(target_vmaf) {
        *high = crf.saturating_sub(1);
    } else {
        *low = crf.saturating_add(1).min(MAX_CRF);
    }
}

fn encode_sample_with_ffmpeg(
    source: &Path,
    dest: &Path,
    crf: u32,
    bounds: &BitrateBounds,
) -> Result<Option<String>, IterationError> {
    let args = sample_encode_args(source, dest, crf, bounds);
    let output = voom_process::run_with_timeout("ffmpeg", &args, FFMPEG_TIMEOUT);
    match output {
        Ok(output) if output.status.success() => Ok(selected_bitrate(bounds)),
        Ok(output) => Err(IterationError::FfmpegFailed {
            exit_status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Err(error) => Err(IterationError::FfmpegFailed {
            exit_status: -1,
            stderr: error.to_string(),
        }),
    }
}

fn sample_encode_args(source: &Path, dest: &Path, crf: u32, bounds: &BitrateBounds) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-i".to_string(),
        source.display().to_string(),
        "-map".to_string(),
        "0:v:0".to_string(),
        "-an".to_string(),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-crf".to_string(),
        crf.to_string(),
    ];
    add_bitrate_args(&mut args, bounds);
    args.push(dest.display().to_string());
    args
}

fn add_bitrate_args(args: &mut Vec<String>, bounds: &BitrateBounds) {
    if let Some(max_bitrate) = &bounds.max_bitrate {
        args.push("-b:v".to_string());
        args.push(max_bitrate.clone());
        args.push("-maxrate".to_string());
        args.push(max_bitrate.clone());
        args.push("-bufsize".to_string());
        args.push(max_bitrate.clone());
    } else if let Some(min_bitrate) = &bounds.min_bitrate {
        args.push("-b:v".to_string());
        args.push(min_bitrate.clone());
    }
    if let Some(min_bitrate) = &bounds.min_bitrate {
        args.push("-minrate".to_string());
        args.push(min_bitrate.clone());
    }
}

fn selected_bitrate(bounds: &BitrateBounds) -> Option<String> {
    bounds
        .max_bitrate
        .clone()
        .or_else(|| bounds.min_bitrate.clone())
}
