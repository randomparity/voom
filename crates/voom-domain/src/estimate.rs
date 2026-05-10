use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::plan::{ActionParams, OperationType, Plan, PlannedAction};

const DEFAULT_FIXED_ACTION_MS: u64 = 500;
const DEFAULT_TRANSCODE_PIXELS_PER_SECOND: f64 = 2_000_000.0;
const DEFAULT_TRANSCODE_SIZE_RATIO: f64 = 0.65;

/// Input used to estimate one policy planning run.
#[derive(Debug, Clone)]
pub struct EstimateInput {
    pub plans: Vec<Plan>,
    pub workers: usize,
    pub estimated_at: DateTime<Utc>,
}

impl EstimateInput {
    #[must_use]
    pub fn new(plans: Vec<Plan>, workers: usize, estimated_at: DateTime<Utc>) -> Self {
        Self {
            plans,
            workers,
            estimated_at,
        }
    }
}

/// Stable key used to match historical samples to planned operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EstimateOperationKey {
    pub phase_name: String,
    pub codec: String,
    pub preset: String,
    pub backend: String,
}

impl EstimateOperationKey {
    #[must_use]
    pub fn transcode(
        phase_name: impl Into<String>,
        codec: impl Into<String>,
        preset: impl Into<String>,
        backend: impl Into<String>,
    ) -> Self {
        Self {
            phase_name: phase_name.into(),
            codec: codec.into(),
            preset: preset.into(),
            backend: backend.into(),
        }
    }
}

/// Historical cost-model sample for one operation key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostModelSample {
    pub id: Uuid,
    pub key: EstimateOperationKey,
    pub pixels_per_second: f64,
    pub output_size_ratio: f64,
    pub fixed_overhead_ms: u64,
    pub completed_at: DateTime<Utc>,
}

impl CostModelSample {
    #[must_use]
    pub fn new(
        key: EstimateOperationKey,
        pixels_per_second: f64,
        output_size_ratio: f64,
        fixed_overhead_ms: u64,
        completed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            key,
            pixels_per_second,
            output_size_ratio,
            fixed_overhead_ms,
            completed_at,
        }
    }

    #[must_use]
    pub fn codec(&self) -> &str {
        &self.key.codec
    }
}

/// Read-only cost model used by the estimator.
#[derive(Debug, Clone, Default)]
pub struct EstimateModel {
    samples: Vec<CostModelSample>,
}

impl EstimateModel {
    #[must_use]
    pub fn from_samples(samples: Vec<CostModelSample>) -> Self {
        Self { samples }
    }

    fn samples_for(&self, key: &EstimateOperationKey) -> Vec<&CostModelSample> {
        self.samples
            .iter()
            .filter(|sample| sample.key == *key)
            .collect()
    }
}

/// Aggregate estimate for a full planning run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstimateRun {
    pub id: Uuid,
    pub estimated_at: DateTime<Utc>,
    pub file_count: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub bytes_saved: i64,
    pub compute_time_ms: u64,
    pub wall_time_ms: u64,
    pub high_uncertainty_files: usize,
    pub net_loss_files: usize,
    pub files: Vec<FileEstimate>,
}

/// Estimate for one planned file/phase pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEstimate {
    pub file_id: Uuid,
    pub path: String,
    pub phase_name: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub bytes_saved: i64,
    pub compute_time_ms: u64,
    pub high_uncertainty: bool,
    pub net_byte_loss: bool,
    pub actions: Vec<ActionEstimate>,
}

/// Estimate for one planned action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEstimate {
    pub operation: OperationType,
    pub codec: Option<String>,
    pub backend: Option<String>,
    pub bytes_out: u64,
    pub compute_time_ms: u64,
    pub high_uncertainty: bool,
}

/// Estimate all plans in one process or standalone estimate invocation.
#[must_use]
pub fn estimate_plans(input: EstimateInput, model: &EstimateModel) -> EstimateRun {
    let files: Vec<FileEstimate> = input
        .plans
        .iter()
        .map(|plan| estimate_file(plan, model))
        .collect();
    let bytes_in = files.iter().map(|file| file.bytes_in).sum();
    let bytes_out = files.iter().map(|file| file.bytes_out).sum();
    let compute_time_ms = files.iter().map(|file| file.compute_time_ms).sum();
    let workers = u64::try_from(input.workers.max(1)).unwrap_or(u64::MAX);

    EstimateRun {
        id: Uuid::new_v4(),
        estimated_at: input.estimated_at,
        file_count: files.len(),
        bytes_in,
        bytes_out,
        bytes_saved: bytes_in as i64 - bytes_out as i64,
        compute_time_ms,
        wall_time_ms: compute_time_ms.div_ceil(workers),
        high_uncertainty_files: files.iter().filter(|file| file.high_uncertainty).count(),
        net_loss_files: files.iter().filter(|file| file.net_byte_loss).count(),
        files,
    }
}

fn estimate_file(plan: &Plan, model: &EstimateModel) -> FileEstimate {
    let actions: Vec<ActionEstimate> = plan
        .actions
        .iter()
        .map(|action| estimate_action(plan, action, model))
        .collect();
    let bytes_out = actions
        .iter()
        .map(|action| action.bytes_out)
        .max()
        .unwrap_or(plan.file.size);
    let compute_time_ms = actions.iter().map(|action| action.compute_time_ms).sum();
    let bytes_saved = plan.file.size as i64 - bytes_out as i64;

    FileEstimate {
        file_id: plan.file.id,
        path: plan.file.path.display().to_string(),
        phase_name: plan.phase_name.clone(),
        bytes_in: plan.file.size,
        bytes_out,
        bytes_saved,
        compute_time_ms,
        high_uncertainty: actions.iter().any(|action| action.high_uncertainty),
        net_byte_loss: bytes_saved < 0,
        actions,
    }
}

fn estimate_action(plan: &Plan, action: &PlannedAction, model: &EstimateModel) -> ActionEstimate {
    match &action.parameters {
        ActionParams::Transcode { codec, settings } => {
            let preset = settings.preset.as_deref().unwrap_or("default");
            let backend = settings.hw.as_deref().unwrap_or("software");
            let key = EstimateOperationKey::transcode(&plan.phase_name, codec, preset, backend);
            let samples = model.samples_for(&key);
            let profile = TranscodeProfile::from_samples(&samples);
            let compute_time_ms = estimate_transcode_ms(plan, &profile);
            let bytes_out = ((plan.file.size as f64) * profile.output_size_ratio).round() as u64;

            ActionEstimate {
                operation: action.operation,
                codec: Some(codec.clone()),
                backend: Some(backend.to_string()),
                bytes_out,
                compute_time_ms,
                high_uncertainty: samples.len() < 5 || missing_video_dimensions(plan),
            }
        }
        _ => ActionEstimate {
            operation: action.operation,
            codec: None,
            backend: None,
            bytes_out: plan.file.size,
            compute_time_ms: DEFAULT_FIXED_ACTION_MS,
            high_uncertainty: false,
        },
    }
}

struct TranscodeProfile {
    pixels_per_second: f64,
    output_size_ratio: f64,
    fixed_overhead_ms: u64,
}

impl TranscodeProfile {
    fn from_samples(samples: &[&CostModelSample]) -> Self {
        if samples.is_empty() {
            return Self {
                pixels_per_second: DEFAULT_TRANSCODE_PIXELS_PER_SECOND,
                output_size_ratio: DEFAULT_TRANSCODE_SIZE_RATIO,
                fixed_overhead_ms: DEFAULT_FIXED_ACTION_MS,
            };
        }

        Self {
            pixels_per_second: median_f64(
                samples
                    .iter()
                    .map(|sample| sample.pixels_per_second)
                    .collect(),
            ),
            output_size_ratio: median_f64(
                samples
                    .iter()
                    .map(|sample| sample.output_size_ratio)
                    .collect(),
            ),
            fixed_overhead_ms: median_u64(
                samples
                    .iter()
                    .map(|sample| sample.fixed_overhead_ms)
                    .collect(),
            ),
        }
    }
}

fn estimate_transcode_ms(plan: &Plan, profile: &TranscodeProfile) -> u64 {
    let Some((width, height)) = video_dimensions(plan) else {
        return profile.fixed_overhead_ms + duration_fallback_ms(plan, profile);
    };
    let pixels = f64::from(width) * f64::from(height) * plan.file.duration.max(1.0);
    let encode_ms = (pixels / profile.pixels_per_second * 1_000.0).round() as u64;
    profile.fixed_overhead_ms + encode_ms
}

fn duration_fallback_ms(plan: &Plan, profile: &TranscodeProfile) -> u64 {
    (plan.file.duration.max(1.0) * 1_000.0).round() as u64 + profile.fixed_overhead_ms
}

fn video_dimensions(plan: &Plan) -> Option<(u32, u32)> {
    plan.file
        .video_tracks()
        .into_iter()
        .find_map(|track| Some((track.width?, track.height?)))
}

fn missing_video_dimensions(plan: &Plan) -> bool {
    video_dimensions(plan).is_none() || plan.file.duration <= 0.0
}

fn median_f64(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn median_u64(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;

    use crate::estimate::{
        estimate_plans, CostModelSample, EstimateInput, EstimateModel, EstimateOperationKey,
    };
    use crate::media::{Container, MediaFile, Track, TrackType};
    use crate::plan::{ActionParams, OperationType, Plan, PlannedAction, TranscodeSettings};

    fn video_file() -> MediaFile {
        let mut track = Track::new(0, TrackType::Video, "h264".into());
        track.width = Some(1920);
        track.height = Some(1080);
        track.frame_rate = Some(23.976);

        let mut file = MediaFile::new(PathBuf::from("/media/movie.mkv"))
            .with_container(Container::Mkv)
            .with_duration(120.0)
            .with_tracks(vec![track]);
        file.size = 1_000_000_000;
        file
    }

    fn transcode_plan(file: MediaFile) -> Plan {
        Plan::new(file, "archive", "video").with_action(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_preset(Some("slow".into()))
                    .with_hw(Some("nvenc".into())),
            },
            "transcode h264 to hevc",
        ))
    }

    #[test]
    fn estimate_uses_historical_transcode_samples() {
        let completed_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 10, 12, 0, 0)
            .single()
            .expect("valid sample timestamp");
        let key = EstimateOperationKey::transcode("video", "hevc", "slow", "nvenc");
        let sample = CostModelSample::new(key, 4_000_000.0, 0.42, 5_000, completed_at);
        let model = EstimateModel::from_samples(vec![sample]);
        let input = EstimateInput::new(vec![transcode_plan(video_file())], 2, completed_at);

        let estimate = estimate_plans(input, &model);

        assert_eq!(estimate.file_count, 1);
        assert_eq!(estimate.bytes_in, 1_000_000_000);
        assert_eq!(estimate.bytes_out, 420_000_000);
        assert_eq!(estimate.bytes_saved, 580_000_000);
        assert_eq!(estimate.compute_time_ms, 67_208);
        assert_eq!(estimate.wall_time_ms, 33_604);
        assert_eq!(estimate.high_uncertainty_files, 1);
        assert_eq!(estimate.net_loss_files, 0);
        assert_eq!(estimate.files[0].phase_name, "video");
        assert_eq!(
            estimate.files[0].actions[0].backend.as_deref(),
            Some("nvenc")
        );
        assert_eq!(estimate.files[0].actions[0].codec.as_deref(), Some("hevc"));
    }

    #[test]
    fn estimate_flags_files_predicted_to_grow() {
        let completed_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 10, 12, 0, 0)
            .single()
            .expect("valid sample timestamp");
        let key = EstimateOperationKey::transcode("video", "hevc", "slow", "nvenc");
        let sample = CostModelSample::new(key, 4_000_000.0, 1.20, 5_000, completed_at);
        let model = EstimateModel::from_samples(vec![sample]);
        let input = EstimateInput::new(vec![transcode_plan(video_file())], 4, completed_at);

        let estimate = estimate_plans(input, &model);

        assert_eq!(estimate.bytes_out, 1_200_000_000);
        assert_eq!(estimate.bytes_saved, -200_000_000);
        assert_eq!(estimate.net_loss_files, 1);
        assert!(estimate.files[0].net_byte_loss);
    }
}
