use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::{Container, MediaFile, TrackType};
use crate::safeguard::SafeguardViolation;

fn epoch() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

/// A plan produced by the policy evaluator for a single file in a single phase.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub file: MediaFile,
    pub policy_name: String,
    pub phase_name: String,
    pub actions: Vec<PlannedAction>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub safeguard_violations: Vec<SafeguardViolation>,
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub policy_hash: Option<String>,
    #[serde(default = "epoch")]
    pub evaluated_at: DateTime<Utc>,
    /// Hint indicating which executor plugin should handle this plan,
    /// set by capability-aware validation when a single executor matches.
    #[serde(default)]
    pub executor_hint: Option<String>,
    /// Processing session that produced this plan. Set by the CLI before
    /// dispatching `PlanCreated`, used for session-level queries.
    #[serde(default)]
    pub session_id: Option<uuid::Uuid>,
}

impl Plan {
    /// Create a new empty plan for a file and phase.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use voom_domain::media::MediaFile;
    /// use voom_domain::plan::{
    ///     ActionParams, OperationType, Plan, PlannedAction,
    /// };
    ///
    /// let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));
    /// let plan = Plan::new(file, "my-policy", "init")
    ///     .with_action(PlannedAction::track_op(
    ///         OperationType::SetDefault,
    ///         0,
    ///         ActionParams::Empty,
    ///         "set track 0 as default",
    ///     ))
    ///     .with_warning("track has no language tag");
    ///
    /// assert!(!plan.is_empty());
    /// assert!(!plan.is_skipped());
    /// assert_eq!(plan.actions.len(), 1);
    /// assert_eq!(plan.warnings.len(), 1);
    /// ```
    #[must_use]
    pub fn new(
        file: MediaFile,
        policy_name: impl Into<String>,
        phase_name: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            file,
            policy_name: policy_name.into(),
            phase_name: phase_name.into(),
            actions: Vec::new(),
            warnings: Vec::new(),
            safeguard_violations: Vec::new(),
            skip_reason: None,
            policy_hash: None,
            evaluated_at: Utc::now(),
            executor_hint: None,
            session_id: None,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    #[must_use]
    pub fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    /// Set a skip reason and clear any existing actions.
    ///
    /// Skipped plans must have no actions, so this method enforces that invariant.
    #[must_use]
    pub fn with_skip_reason(mut self, reason: impl Into<String>) -> Self {
        self.skip_reason = Some(reason.into());
        self.actions.clear();
        self
    }

    #[must_use]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    #[must_use]
    pub fn with_action(mut self, action: PlannedAction) -> Self {
        self.actions.push(action);
        self
    }

    /// Attach a processing session ID to this plan.
    ///
    /// The session ID links plans created in the same CLI invocation so
    /// session-level queries (e.g., `voom report errors --session`) can
    /// group them.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use uuid::Uuid;
    /// use voom_domain::media::MediaFile;
    /// use voom_domain::plan::Plan;
    ///
    /// let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));
    /// let session = Uuid::new_v4();
    /// let plan = Plan::new(file, "my-policy", "normalize").with_session_id(session);
    ///
    /// assert_eq!(plan.session_id, Some(session));
    /// ```
    #[must_use]
    pub fn with_session_id(mut self, session_id: Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }
}

/// A single action within a plan.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub operation: OperationType,
    pub track_index: Option<u32>,
    pub parameters: ActionParams,
    pub description: String,
}

impl PlannedAction {
    /// Create a planned action for a file-level operation (no track index).
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::media::Container;
    /// use voom_domain::plan::{ActionParams, OperationType, PlannedAction};
    ///
    /// let action = PlannedAction::file_op(
    ///     OperationType::ConvertContainer,
    ///     ActionParams::Container { container: Container::Mkv },
    ///     "convert to MKV",
    /// );
    /// assert!(action.track_index.is_none());
    /// ```
    #[must_use]
    pub fn file_op(
        operation: OperationType,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Self {
        Self::try_file_op(operation, parameters, description)
            .unwrap_or_else(|error| panic!("invalid planned action: {error}"))
    }

    /// Try to create a planned action for a file-level operation.
    ///
    /// # Errors
    /// Returns an error when `operation` and `parameters` describe different action kinds.
    pub fn try_file_op(
        operation: OperationType,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Result<Self, String> {
        Self::new_checked(operation, None, parameters, description)
    }

    fn new_checked(
        operation: OperationType,
        track_index: Option<u32>,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Result<Self, String> {
        validate_action_params(operation, &parameters)?;
        Ok(Self {
            operation,
            track_index,
            parameters,
            description: description.into(),
        })
    }

    /// Create a planned action targeting a specific track.
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::plan::{ActionParams, OperationType, PlannedAction};
    ///
    /// let action = PlannedAction::track_op(
    ///     OperationType::RemoveTrack,
    ///     2,
    ///     ActionParams::RemoveTrack {
    ///         reason: "unwanted commentary".into(),
    ///         track_type: voom_domain::media::TrackType::AudioCommentary,
    ///     },
    ///     "remove commentary track",
    /// );
    /// assert_eq!(action.track_index, Some(2));
    /// ```
    #[must_use]
    pub fn track_op(
        operation: OperationType,
        track_index: u32,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Self {
        Self::try_track_op(operation, track_index, parameters, description)
            .unwrap_or_else(|error| panic!("invalid planned action: {error}"))
    }

    /// Try to create a planned action targeting a specific track.
    ///
    /// # Errors
    /// Returns an error when `operation` and `parameters` describe different action kinds.
    pub fn try_track_op(
        operation: OperationType,
        track_index: u32,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Result<Self, String> {
        Self::new_checked(operation, Some(track_index), parameters, description)
    }
}

fn validate_action_params(
    operation: OperationType,
    parameters: &ActionParams,
) -> Result<(), String> {
    match (operation, parameters) {
        (
            OperationType::SetDefault
            | OperationType::ClearDefault
            | OperationType::SetForced
            | OperationType::ClearForced,
            ActionParams::Empty,
        )
        | (OperationType::ConvertContainer, ActionParams::Container { .. })
        | (OperationType::RemoveTrack, ActionParams::RemoveTrack { .. })
        | (OperationType::ReorderTracks, ActionParams::ReorderTracks { .. })
        | (OperationType::SetLanguage, ActionParams::Language { .. })
        | (OperationType::SetTitle, ActionParams::Title { .. })
        | (
            OperationType::TranscodeVideo | OperationType::TranscodeAudio,
            ActionParams::Transcode { .. },
        )
        | (OperationType::SynthesizeAudio, ActionParams::Synthesize { .. })
        | (OperationType::SetContainerTag, ActionParams::SetTag { .. })
        | (OperationType::ClearContainerTags, ActionParams::ClearTags { .. })
        | (OperationType::DeleteContainerTag, ActionParams::DeleteTag { .. })
        | (OperationType::MuxSubtitle, ActionParams::MuxSubtitle { .. })
        | (OperationType::VerifyMedia, ActionParams::VerifyMedia(_))
        | (OperationType::Quarantine, ActionParams::Quarantine(_)) => Ok(()),
        _ => Err(format!(
            "operation {} cannot use {} parameters",
            operation.as_str(),
            parameters.kind()
        )),
    }
}

impl ActionParams {
    fn kind(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Container { .. } => "container",
            Self::RemoveTrack { .. } => "remove_track",
            Self::ReorderTracks { .. } => "reorder_tracks",
            Self::Language { .. } => "language",
            Self::Title { .. } => "title",
            Self::Transcode { .. } => "transcode",
            Self::Synthesize { .. } => "synthesize",
            Self::SetTag { .. } => "set_tag",
            Self::ClearTags { .. } => "clear_tags",
            Self::DeleteTag { .. } => "delete_tag",
            Self::MuxSubtitle { .. } => "mux_subtitle",
            Self::VerifyMedia(_) => "verify_media",
            Self::Quarantine(_) => "quarantine",
        }
    }
}

/// Channel setting for a transcode action — either a named preset or a count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TranscodeChannels {
    /// Named preset, e.g. `stereo`, `preserve`.
    Named(String),
    /// Explicit channel count, e.g. `2`, `6`.
    Count(u32),
}

/// Sampling plan for VMAF-guided transcode scoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SampleStrategy {
    Scenes { count: u32, duration: String },
    Uniform { count: u32, duration: String },
    Full,
}

/// Fallback encode settings used when VMAF scoring is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TranscodeFallback {
    pub crf: u32,
    pub preset: String,
}

impl TranscodeFallback {
    #[must_use]
    pub fn new(crf: u32, preset: impl Into<String>) -> Self {
        Self {
            crf,
            preset: preset.into(),
        }
    }
}

impl TranscodeChannels {
    /// Resolve to a concrete channel count.
    /// Returns `None` for "preserve" or unrecognized named presets.
    #[must_use]
    pub fn to_count(&self) -> Option<u32> {
        match self {
            Self::Count(n) => Some(*n),
            Self::Named(name) => match name.as_str() {
                "mono" => Some(1),
                "stereo" => Some(2),
                "5.1" | "surround" => Some(6),
                "7.1" => Some(8),
                _ => None,
            },
        }
    }
}

/// Transcode quality/encoding settings, separate from the codec choice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TranscodeSettings {
    pub crf: Option<u32>,
    pub preset: Option<String>,
    pub bitrate: Option<String>,
    pub target_vmaf: Option<u32>,
    pub max_bitrate: Option<String>,
    pub min_bitrate: Option<String>,
    pub sample_strategy: Option<SampleStrategy>,
    pub fallback: Option<TranscodeFallback>,
    pub vmaf_overrides: Option<HashMap<String, u32>>,
    pub channels: Option<TranscodeChannels>,
    /// Per-action HW acceleration preference (overrides system-wide detection).
    pub hw: Option<String>,
    /// Whether to fall back to software encoding when HW is unavailable.
    pub hw_fallback: Option<bool>,
    /// Maximum resolution (e.g. "1080p"). Downscale if source exceeds.
    pub max_resolution: Option<String>,
    /// Scaling algorithm (e.g. "lanczos").
    pub scale_algorithm: Option<String>,
    /// HDR handling mode (e.g. "preserve", "tonemap").
    pub hdr_mode: Option<String>,
    /// Preserve HDR metadata when transcoding HDR sources.
    pub preserve_hdr: Option<bool>,
    /// SDR tone-mapping algorithm (e.g. "bt2390", "hable").
    pub tonemap: Option<String>,
    /// HDR static color metadata handling (currently "copy").
    pub hdr_color_metadata: Option<String>,
    /// Dolby Vision metadata handling (currently "copy_rpu").
    pub dolby_vision: Option<String>,
    /// Encoder tuning hint (e.g. "film", "animation").
    pub tune: Option<String>,
    /// Automatic crop settings for black-bar removal.
    pub crop: Option<CropSettings>,
    /// Optional EBU R128 loudness normalization settings for audio output.
    pub loudness: Option<LoudnessNormalization>,
}

/// EBU R128 / LUFS loudness normalization settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LoudnessNormalization {
    pub preset: LoudnessPreset,
    pub target_lufs: f64,
    pub true_peak_db: f64,
    pub lra_max: Option<f64>,
    #[serde(default = "default_loudness_tolerance")]
    pub tolerance_lufs: f64,
    #[serde(default)]
    pub measured: Option<LoudnessMeasurement>,
}

/// Measurement values emitted by ffmpeg's `loudnorm` first pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LoudnessMeasurement {
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub input_thresh: f64,
    pub target_offset: f64,
}

impl LoudnessMeasurement {
    #[must_use]
    pub fn new(
        input_i: f64,
        input_tp: f64,
        input_lra: f64,
        input_thresh: f64,
        target_offset: f64,
    ) -> Self {
        Self {
            input_i,
            input_tp,
            input_lra,
            input_thresh,
            target_offset,
        }
    }
}

/// Built-in loudness target presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoudnessPreset {
    EbuR128,
    EbuR128Broadcast,
    StreamingMovies,
    StreamingMusic,
    Mobile,
    VoiceFocused,
}

fn default_loudness_tolerance() -> f64 {
    0.5
}

impl LoudnessPreset {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ebu_r128" => Some(Self::EbuR128),
            "ebu_r128_broadcast" => Some(Self::EbuR128Broadcast),
            "streaming_movies" => Some(Self::StreamingMovies),
            "streaming_music" => Some(Self::StreamingMusic),
            "mobile" => Some(Self::Mobile),
            "voice_focused" => Some(Self::VoiceFocused),
            _ => None,
        }
    }

    #[must_use]
    pub fn defaults(self) -> LoudnessNormalization {
        match self {
            Self::EbuR128 | Self::EbuR128Broadcast => {
                LoudnessNormalization::new(self, -23.0, -1.0, Some(18.0))
            }
            Self::StreamingMovies => LoudnessNormalization::new(self, -24.0, -2.0, None),
            Self::StreamingMusic => LoudnessNormalization::new(self, -14.0, -1.0, None),
            Self::Mobile => LoudnessNormalization::new(self, -16.0, -1.5, None),
            Self::VoiceFocused => LoudnessNormalization::new(self, -19.0, -1.0, Some(7.0)),
        }
    }
}

impl LoudnessNormalization {
    #[must_use]
    pub fn new(
        preset: LoudnessPreset,
        target_lufs: f64,
        true_peak_db: f64,
        lra_max: Option<f64>,
    ) -> Self {
        Self {
            preset,
            target_lufs,
            true_peak_db,
            lra_max,
            tolerance_lufs: default_loudness_tolerance(),
            measured: None,
        }
    }

    #[must_use]
    pub fn with_measurement(mut self, measured: LoudnessMeasurement) -> Self {
        self.measured = Some(measured);
        self
    }

    #[must_use]
    pub fn is_within_target(&self, measured_lufs: f64) -> bool {
        (measured_lufs - self.target_lufs).abs() <= self.tolerance_lufs
    }
}

/// Automatic black-bar crop detection settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CropSettings {
    pub sample_duration_secs: Option<u32>,
    pub sample_count: Option<u32>,
    pub threshold: Option<u8>,
    pub minimum_crop: Option<u32>,
    pub preserve_bottom_pixels: Option<u32>,
    pub aspect_lock: Vec<String>,
}

impl CropSettings {
    #[must_use]
    pub fn auto() -> Self {
        Self {
            sample_duration_secs: None,
            sample_count: None,
            threshold: None,
            minimum_crop: None,
            preserve_bottom_pixels: None,
            aspect_lock: Vec::new(),
        }
    }
}

/// Deserialization helper for [`ActionParams`] that lifts legacy flat
/// transcode fields (`crf`, `preset`, …) into `TranscodeSettings`.
///
/// All variants except `Transcode` are structurally identical.  The
/// `Transcode` variant captures both the nested `settings` key (current
/// format) and flat sibling keys (legacy format), then merges them.
#[derive(Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
enum ActionParamsCompat {
    Empty,
    Container {
        container: Container,
    },
    RemoveTrack {
        reason: String,
        track_type: TrackType,
    },
    ReorderTracks {
        order: Vec<String>,
    },
    Language {
        language: String,
    },
    Title {
        title: String,
    },
    Transcode {
        codec: String,
        #[serde(default)]
        settings: TranscodeSettings,
        // Legacy flat fields — populated when deserializing old payloads.
        #[serde(default)]
        crf: Option<u32>,
        #[serde(default)]
        preset: Option<String>,
        #[serde(default)]
        bitrate: Option<String>,
        #[serde(default)]
        target_vmaf: Option<u32>,
        #[serde(default)]
        max_bitrate: Option<String>,
        #[serde(default)]
        min_bitrate: Option<String>,
        #[serde(default)]
        sample_strategy: Option<SampleStrategy>,
        #[serde(default)]
        fallback: Option<TranscodeFallback>,
        #[serde(default)]
        vmaf_overrides: Option<HashMap<String, u32>>,
        #[serde(default)]
        channels: Option<TranscodeChannels>,
        #[serde(default)]
        hw: Option<String>,
        #[serde(default)]
        hw_fallback: Option<bool>,
        #[serde(default)]
        max_resolution: Option<String>,
        #[serde(default)]
        scale_algorithm: Option<String>,
        #[serde(default)]
        hdr_mode: Option<String>,
        #[serde(default)]
        preserve_hdr: Option<bool>,
        #[serde(default)]
        tonemap: Option<String>,
        #[serde(default)]
        hdr_color_metadata: Option<String>,
        #[serde(default)]
        dolby_vision: Option<String>,
        #[serde(default)]
        tune: Option<String>,
        #[serde(default)]
        crop: Option<CropSettings>,
        #[serde(default)]
        loudness: Option<LoudnessNormalization>,
    },
    Synthesize {
        name: String,
        language: Option<String>,
        codec: Option<String>,
        text: Option<String>,
        bitrate: Option<String>,
        channels: Option<u32>,
        title: Option<String>,
        position: Option<String>,
        source_track: Option<u32>,
        #[serde(default)]
        loudness: Option<LoudnessNormalization>,
    },
    SetTag {
        tag: String,
        value: String,
    },
    ClearTags {
        tags: Vec<String>,
    },
    DeleteTag {
        tag: String,
    },
    MuxSubtitle {
        subtitle_path: PathBuf,
        language: String,
        forced: bool,
        title: Option<String>,
    },
    VerifyMedia(VerifyMediaParams),
    Quarantine(QuarantineParams),
}

impl From<ActionParamsCompat> for ActionParams {
    #[allow(clippy::needless_update)]
    fn from(compat: ActionParamsCompat) -> Self {
        match compat {
            ActionParamsCompat::Empty => Self::Empty,
            ActionParamsCompat::Container { container } => Self::Container { container },
            ActionParamsCompat::RemoveTrack { reason, track_type } => {
                Self::RemoveTrack { reason, track_type }
            }
            ActionParamsCompat::ReorderTracks { order } => Self::ReorderTracks { order },
            ActionParamsCompat::Language { language } => Self::Language { language },
            ActionParamsCompat::Title { title } => Self::Title { title },
            ActionParamsCompat::Transcode {
                codec,
                settings,
                crf,
                preset,
                bitrate,
                target_vmaf,
                max_bitrate,
                min_bitrate,
                sample_strategy,
                fallback,
                vmaf_overrides,
                channels,
                hw,
                hw_fallback,
                max_resolution,
                scale_algorithm,
                hdr_mode,
                preserve_hdr,
                tonemap,
                hdr_color_metadata,
                dolby_vision,
                tune,
                crop,
                loudness,
            } => {
                // If the nested `settings` object has values, use it.
                // Otherwise, lift the legacy flat fields.
                let merged = if settings == TranscodeSettings::default() {
                    TranscodeSettings {
                        crf,
                        preset,
                        bitrate,
                        target_vmaf,
                        max_bitrate,
                        min_bitrate,
                        sample_strategy,
                        fallback,
                        vmaf_overrides,
                        channels,
                        hw,
                        hw_fallback,
                        max_resolution,
                        scale_algorithm,
                        hdr_mode,
                        preserve_hdr,
                        tonemap,
                        hdr_color_metadata,
                        dolby_vision,
                        tune,
                        crop,
                        loudness,
                        ..Default::default()
                    }
                } else {
                    settings
                };
                Self::Transcode {
                    codec,
                    settings: merged,
                }
            }
            ActionParamsCompat::Synthesize {
                name,
                language,
                codec,
                text,
                bitrate,
                channels,
                title,
                position,
                source_track,
                loudness,
            } => Self::Synthesize {
                name,
                language,
                codec,
                text,
                bitrate,
                channels,
                title,
                position,
                source_track,
                loudness,
            },
            ActionParamsCompat::SetTag { tag, value } => Self::SetTag { tag, value },
            ActionParamsCompat::ClearTags { tags } => Self::ClearTags { tags },
            ActionParamsCompat::DeleteTag { tag } => Self::DeleteTag { tag },
            ActionParamsCompat::MuxSubtitle {
                subtitle_path,
                language,
                forced,
                title,
            } => Self::MuxSubtitle {
                subtitle_path,
                language,
                forced,
                title,
            },
            ActionParamsCompat::VerifyMedia(params) => Self::VerifyMedia(params),
            ActionParamsCompat::Quarantine(params) => Self::Quarantine(params),
        }
    }
}

impl TranscodeSettings {
    #[must_use]
    pub fn resolve_target_vmaf(&self, file: &MediaFile) -> Option<u32> {
        let target = self.target_vmaf;
        let animation_override = self
            .vmaf_overrides
            .as_ref()
            .and_then(|overrides| overrides.get("animation"))
            .copied();
        let resolved = if file.has_animation_video() {
            animation_override.or(target)
        } else {
            target
        };
        tracing::debug!(
            target_used = ?resolved,
            override_applied = resolved != target,
            "vmaf target resolution"
        );
        resolved
    }

    #[must_use]
    pub fn with_crf(mut self, crf: Option<u32>) -> Self {
        self.crf = crf;
        self
    }

    #[must_use]
    pub fn with_preset(mut self, preset: Option<String>) -> Self {
        self.preset = preset;
        self
    }

    #[must_use]
    pub fn with_bitrate(mut self, bitrate: Option<String>) -> Self {
        self.bitrate = bitrate;
        self
    }

    #[must_use]
    pub fn with_target_vmaf(mut self, target_vmaf: Option<u32>) -> Self {
        self.target_vmaf = target_vmaf;
        self
    }

    #[must_use]
    pub fn with_max_bitrate(mut self, max_bitrate: Option<String>) -> Self {
        self.max_bitrate = max_bitrate;
        self
    }

    #[must_use]
    pub fn with_min_bitrate(mut self, min_bitrate: Option<String>) -> Self {
        self.min_bitrate = min_bitrate;
        self
    }

    #[must_use]
    pub fn with_sample_strategy(mut self, sample_strategy: Option<SampleStrategy>) -> Self {
        self.sample_strategy = sample_strategy;
        self
    }

    #[must_use]
    pub fn with_fallback(mut self, fallback: Option<TranscodeFallback>) -> Self {
        self.fallback = fallback;
        self
    }

    #[must_use]
    pub fn with_vmaf_overrides(mut self, vmaf_overrides: Option<HashMap<String, u32>>) -> Self {
        self.vmaf_overrides = vmaf_overrides;
        self
    }

    #[must_use]
    pub fn with_channels(mut self, channels: Option<TranscodeChannels>) -> Self {
        self.channels = channels;
        self
    }

    #[must_use]
    pub fn with_hw(mut self, hw: Option<String>) -> Self {
        self.hw = hw;
        self
    }

    #[must_use]
    pub fn with_hw_fallback(mut self, hw_fallback: Option<bool>) -> Self {
        self.hw_fallback = hw_fallback;
        self
    }

    #[must_use]
    pub fn with_max_resolution(mut self, max_resolution: Option<String>) -> Self {
        self.max_resolution = max_resolution;
        self
    }

    #[must_use]
    pub fn with_scale_algorithm(mut self, scale_algorithm: Option<String>) -> Self {
        self.scale_algorithm = scale_algorithm;
        self
    }

    #[must_use]
    pub fn with_hdr_mode(mut self, hdr_mode: Option<String>) -> Self {
        self.hdr_mode = hdr_mode;
        self
    }

    #[must_use]
    pub fn with_preserve_hdr(mut self, preserve_hdr: Option<bool>) -> Self {
        self.preserve_hdr = preserve_hdr;
        self
    }

    #[must_use]
    pub fn with_tonemap(mut self, tonemap: Option<String>) -> Self {
        self.tonemap = tonemap;
        self
    }

    #[must_use]
    pub fn with_hdr_color_metadata(mut self, hdr_color_metadata: Option<String>) -> Self {
        self.hdr_color_metadata = hdr_color_metadata;
        self
    }

    #[must_use]
    pub fn with_dolby_vision(mut self, dolby_vision: Option<String>) -> Self {
        self.dolby_vision = dolby_vision;
        self
    }

    #[must_use]
    pub fn with_tune(mut self, tune: Option<String>) -> Self {
        self.tune = tune;
        self
    }

    #[must_use]
    pub fn with_crop(mut self, crop: Option<CropSettings>) -> Self {
        self.crop = crop;
        self
    }

    #[must_use]
    pub fn with_loudness(mut self, loudness: Option<LoudnessNormalization>) -> Self {
        self.loudness = loudness;
        self
    }
}

/// Typed parameters for each operation type.
/// Replaces the previous untyped `serde_json::Value` parameters field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", from = "ActionParamsCompat")]
// Keep the public Transcode variant shape stable; boxing `settings` would
// ripple into executor/plugin APIs for no runtime behavior change in this IR-only stage.
#[allow(clippy::large_enum_variant)]
pub enum ActionParams {
    /// No parameters needed (`SetDefault`, `ClearDefault`, `SetForced`, `ClearForced`).
    Empty,
    /// Container conversion target.
    Container {
        container: Container,
    },
    /// Track removal with reason and track type.
    RemoveTrack {
        reason: String,
        track_type: TrackType,
    },
    /// Track reordering.
    ReorderTracks {
        order: Vec<String>,
    },
    /// Language assignment.
    Language {
        language: String,
    },
    /// Title assignment (empty string to clear).
    Title {
        title: String,
    },
    /// Transcode settings (codec, plus optional quality/encoding parameters).
    Transcode {
        codec: String,
        #[serde(default)]
        settings: TranscodeSettings,
    },
    /// Audio synthesis parameters.
    Synthesize {
        name: String,
        language: Option<String>,
        codec: Option<String>,
        text: Option<String>,
        bitrate: Option<String>,
        channels: Option<u32>,
        title: Option<String>,
        position: Option<String>,
        source_track: Option<u32>,
        #[serde(default)]
        loudness: Option<LoudnessNormalization>,
    },
    /// Container tag operations.
    SetTag {
        tag: String,
        value: String,
    },
    ClearTags {
        tags: Vec<String>,
    },
    DeleteTag {
        tag: String,
    },
    /// Subtitle mux parameters for adding an external subtitle file to a container.
    ///
    /// Produced by executors when handling `SubtitleGenerated` events. The executor
    /// converts the event into a `PlanCreated` with this action, which flows through
    /// the normal backup-aware execution path.
    MuxSubtitle {
        subtitle_path: PathBuf,
        language: String,
        forced: bool,
        title: Option<String>,
    },
    VerifyMedia(VerifyMediaParams),
    Quarantine(QuarantineParams),
}

/// Parameters for `OperationType::VerifyMedia`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyMediaParams {
    pub mode: crate::verification::VerificationMode,
}

/// Parameters for `OperationType::Quarantine`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuarantineParams {
    pub reason: String,
}

/// The type of operation to perform on a media file.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    SetDefault,
    ClearDefault,
    SetForced,
    ClearForced,
    /// Set a track's title. An empty title string means "clear/remove the title".
    SetTitle,
    SetLanguage,
    RemoveTrack,
    ReorderTracks,
    ConvertContainer,
    TranscodeVideo,
    TranscodeAudio,
    SynthesizeAudio,
    SetContainerTag,
    ClearContainerTags,
    DeleteContainerTag,
    /// Mux an external subtitle file into the media container.
    ///
    /// Handled by `mkvtoolnix-executor` (MKV files) and `ffmpeg-executor`
    /// (non-MKV files) via the normal plan execution path.
    MuxSubtitle,
    VerifyMedia,
    Quarantine,
}

impl OperationType {
    /// Parse an `OperationType` from its canonical string representation.
    ///
    /// Returns `None` for unrecognised strings.
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::plan::OperationType;
    ///
    /// assert_eq!(OperationType::parse("set_default"), Some(OperationType::SetDefault));
    /// assert_eq!(OperationType::parse("transcode_video"), Some(OperationType::TranscodeVideo));
    /// assert_eq!(OperationType::parse("unknown"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "set_default" => Some(Self::SetDefault),
            "clear_default" => Some(Self::ClearDefault),
            "set_forced" => Some(Self::SetForced),
            "clear_forced" => Some(Self::ClearForced),
            "set_title" => Some(Self::SetTitle),
            "set_language" => Some(Self::SetLanguage),
            "remove_track" => Some(Self::RemoveTrack),
            "reorder_tracks" => Some(Self::ReorderTracks),
            "convert_container" => Some(Self::ConvertContainer),
            "transcode_video" => Some(Self::TranscodeVideo),
            "transcode_audio" => Some(Self::TranscodeAudio),
            "synthesize_audio" => Some(Self::SynthesizeAudio),
            "set_container_tag" => Some(Self::SetContainerTag),
            "clear_container_tags" => Some(Self::ClearContainerTags),
            "delete_container_tag" => Some(Self::DeleteContainerTag),
            "mux_subtitle" => Some(Self::MuxSubtitle),
            "verify_media" => Some(Self::VerifyMedia),
            "quarantine" => Some(Self::Quarantine),
            _ => None,
        }
    }

    /// The set of operation types that are metadata-only edits (no transcode or remux).
    ///
    /// Both the `FFmpeg` and `MKVToolNix` executors use this to decide whether a plan
    /// requires structural changes or can be handled via in-place metadata edits.
    pub const METADATA_OPS: &[OperationType] = &[
        OperationType::SetDefault,
        OperationType::ClearDefault,
        OperationType::SetForced,
        OperationType::ClearForced,
        OperationType::SetTitle,
        OperationType::SetLanguage,
        OperationType::SetContainerTag,
        OperationType::ClearContainerTags,
        OperationType::DeleteContainerTag,
    ];

    /// Returns `true` when this operation is a metadata-only edit (no transcode or remux).
    #[must_use]
    pub fn is_metadata_op(self) -> bool {
        Self::METADATA_OPS.contains(&self)
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationType::SetDefault => "set_default",
            OperationType::ClearDefault => "clear_default",
            OperationType::SetForced => "set_forced",
            OperationType::ClearForced => "clear_forced",
            OperationType::SetTitle => "set_title",
            OperationType::SetLanguage => "set_language",
            OperationType::RemoveTrack => "remove_track",
            OperationType::ReorderTracks => "reorder_tracks",
            OperationType::ConvertContainer => "convert_container",
            OperationType::TranscodeVideo => "transcode_video",
            OperationType::TranscodeAudio => "transcode_audio",
            OperationType::SynthesizeAudio => "synthesize_audio",
            OperationType::SetContainerTag => "set_container_tag",
            OperationType::ClearContainerTags => "clear_container_tags",
            OperationType::DeleteContainerTag => "delete_container_tag",
            OperationType::MuxSubtitle => "mux_subtitle",
            OperationType::VerifyMedia => "verify_media",
            OperationType::Quarantine => "quarantine",
        }
    }
}

/// Side-channel data a phase exposes for downstream phases to reference
/// via `<phase>.<field>` field access in conditions.
///
/// Populated by callers (CLI, orchestrator) from persisted phase results
/// before evaluating downstream phases. The evaluator itself does not
/// produce these — it only reads them through a closure-based lookup
/// passed into `EvalContext`.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseOutput {
    /// Whether the phase ran to completion (executed without skip/error).
    pub completed: bool,
    /// Whether the phase modified the file.
    pub modified: bool,
    /// Phase-specific outcome string (e.g. `"ok"`, `"error"`, `"warning"`
    /// from a verify phase).
    pub outcome: Option<String>,
    /// Number of errors recorded by the phase.
    pub error_count: u32,
    /// Number of warnings recorded by the phase.
    pub warning_count: u32,
}

impl PhaseOutput {
    /// Create a fully-default `PhaseOutput`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the `completed` flag.
    #[must_use]
    pub fn with_completed(mut self, completed: bool) -> Self {
        self.completed = completed;
        self
    }

    /// Set the `modified` flag.
    #[must_use]
    pub fn with_modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }

    /// Set the `outcome` field.
    #[must_use]
    pub fn with_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Set the `error_count` field.
    #[must_use]
    pub fn with_error_count(mut self, error_count: u32) -> Self {
        self.error_count = error_count;
        self
    }

    /// Set the `warning_count` field.
    #[must_use]
    pub fn with_warning_count(mut self, warning_count: u32) -> Self {
        self.warning_count = warning_count;
        self
    }
}

/// Set of field names addressable on a `PhaseOutput` via cross-phase
/// field access (e.g. `verify.outcome`).
pub const PHASE_OUTPUT_FIELDS: &[&str] = &[
    "completed",
    "modified",
    "outcome",
    "error_count",
    "warning_count",
];

/// The result of executing a single phase.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub phase_name: String,
    pub outcome: PhaseOutcome,
    pub actions: Vec<ActionResult>,
    pub file_modified: bool,
    pub skip_reason: Option<String>,
    pub duration_ms: u64,
    /// Path to the temp file used during execution, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_path: Option<String>,
}

impl PhaseResult {
    #[must_use]
    pub fn new(phase_name: impl Into<String>, outcome: PhaseOutcome) -> Self {
        Self {
            phase_name: phase_name.into(),
            outcome,
            actions: Vec::new(),
            file_modified: false,
            skip_reason: None,
            duration_ms: 0,
            temp_path: None,
        }
    }
}

/// The outcome of a phase execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseOutcome {
    Pending,
    Completed,
    Skipped,
    Failed,
}

/// Captured subprocess output from an executor invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionDetail {
    /// Shell-quoted command line.
    pub command: String,
    /// Process exit code.
    pub exit_code: Option<i32>,
    /// Last N non-empty lines of stderr. Populated on failure and on
    /// mkvmerge warnings (exit code 1). Empty on clean success.
    pub stderr_tail: String,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: u64,
}

/// The result of executing a single action within a phase.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub operation: OperationType,
    pub success: bool,
    pub description: String,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_detail: Option<ExecutionDetail>,
}

impl ActionResult {
    #[must_use]
    pub fn success(operation: OperationType, description: impl Into<String>) -> Self {
        Self {
            operation,
            success: true,
            description: description.into(),
            error: None,
            execution_detail: None,
        }
    }

    #[must_use]
    pub fn failure(
        operation: OperationType,
        description: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            success: false,
            description: description.into(),
            error: Some(error.into()),
            execution_detail: None,
        }
    }

    /// Attach execution detail to this result.
    #[must_use]
    pub fn with_execution_detail(mut self, detail: ExecutionDetail) -> Self {
        self.execution_detail = Some(detail);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaFile, Track};
    use std::path::PathBuf;

    fn sample_plan() -> Plan {
        Plan {
            id: Uuid::new_v4(),
            file: MediaFile::new(PathBuf::from("/test.mkv")),
            policy_name: "default".into(),
            phase_name: "normalize".into(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: ActionParams::Empty,
                description: "Set track 1 as default".into(),
            }],
            warnings: vec![],
            safeguard_violations: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: Utc::now(),
            executor_hint: None,
            session_id: None,
        }
    }

    #[test]
    fn test_plan_is_empty() {
        let mut plan = sample_plan();
        assert!(!plan.is_empty());
        plan.actions.clear();
        assert!(plan.is_empty());
    }

    #[test]
    fn test_plan_is_skipped() {
        let mut plan = sample_plan();
        assert!(!plan.is_skipped());
        plan.skip_reason = Some("codec already correct".into());
        assert!(plan.is_skipped());
    }

    #[test]
    fn test_plan_serde_json_roundtrip() {
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        let deserialized: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.policy_name, "default");
        assert_eq!(deserialized.actions.len(), 1);
        assert_eq!(deserialized.actions[0].operation, OperationType::SetDefault);
    }

    #[test]
    fn test_plan_serde_msgpack_roundtrip() {
        let plan = sample_plan();
        let bytes = rmp_serde::to_vec(&plan).unwrap();
        let deserialized: Plan = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.phase_name, "normalize");
        assert_eq!(deserialized.actions.len(), 1);
    }

    #[test]
    fn test_operation_type_as_str() {
        assert_eq!(OperationType::SetDefault.as_str(), "set_default");
        assert_eq!(OperationType::TranscodeVideo.as_str(), "transcode_video");
    }

    #[test]
    fn test_plan_builder_methods() {
        let plan = sample_plan();
        let action = PlannedAction {
            operation: OperationType::RemoveTrack,
            track_index: Some(2),
            parameters: ActionParams::RemoveTrack {
                reason: "test".into(),
                track_type: TrackType::AudioMain,
            },
            description: "Remove track 2".into(),
        };

        let plan = plan.with_warning("test warning").with_action(action);

        assert_eq!(plan.warnings, vec!["test warning"]);
        assert_eq!(plan.actions.len(), 2);

        // with_skip_reason clears actions to avoid inconsistent state
        let plan = plan.with_skip_reason("no changes needed");
        assert!(plan.is_skipped());
        assert_eq!(plan.skip_reason.as_deref(), Some("no changes needed"));
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn planned_action_try_constructors_accept_matching_params() {
        let file_action = PlannedAction::try_file_op(
            OperationType::ConvertContainer,
            ActionParams::Container {
                container: Container::Mkv,
            },
            "convert",
        )
        .expect("valid file action");
        assert_eq!(file_action.track_index, None);

        let track_action = PlannedAction::try_track_op(
            OperationType::SetLanguage,
            1,
            ActionParams::Language {
                language: "eng".into(),
            },
            "language",
        )
        .expect("valid track action");
        assert_eq!(track_action.track_index, Some(1));
    }

    #[test]
    fn planned_action_try_constructors_reject_mismatched_params() {
        let error = PlannedAction::try_file_op(
            OperationType::ConvertContainer,
            ActionParams::Language {
                language: "eng".into(),
            },
            "bad",
        )
        .unwrap_err();

        assert!(error.contains("convert_container"));
        assert!(error.contains("language"));
    }

    #[test]
    fn test_transcode_channels_to_count_named() {
        assert_eq!(TranscodeChannels::Named("mono".into()).to_count(), Some(1));
        assert_eq!(
            TranscodeChannels::Named("stereo".into()).to_count(),
            Some(2)
        );
        assert_eq!(TranscodeChannels::Named("5.1".into()).to_count(), Some(6));
        assert_eq!(
            TranscodeChannels::Named("surround".into()).to_count(),
            Some(6)
        );
        assert_eq!(TranscodeChannels::Named("7.1".into()).to_count(), Some(8));
    }

    #[test]
    fn test_transcode_channels_to_count_numeric() {
        assert_eq!(TranscodeChannels::Count(1).to_count(), Some(1));
        assert_eq!(TranscodeChannels::Count(2).to_count(), Some(2));
        assert_eq!(TranscodeChannels::Count(6).to_count(), Some(6));
    }

    #[test]
    fn test_transcode_channels_to_count_preserve_returns_none() {
        assert_eq!(TranscodeChannels::Named("preserve".into()).to_count(), None);
    }

    #[test]
    fn test_transcode_channels_to_count_unknown_returns_none() {
        assert_eq!(
            TranscodeChannels::Named("quadraphonic".into()).to_count(),
            None
        );
    }

    #[test]
    fn test_transcode_old_flat_format_preserves_settings() {
        let json = r#"{"type":"Transcode","codec":"hevc","crf":23,"preset":"medium"}"#;
        let parsed: ActionParams = serde_json::from_str(json).unwrap();
        if let ActionParams::Transcode { codec, settings } = parsed {
            assert_eq!(codec, "hevc");
            assert_eq!(settings.crf, Some(23));
            assert_eq!(settings.preset.as_deref(), Some("medium"));
        } else {
            panic!("expected Transcode variant");
        }
    }

    #[test]
    fn test_transcode_nested_format_preserves_settings() {
        let json = r#"{"type":"Transcode","codec":"hevc","settings":{"crf":18,"preset":"slow"}}"#;
        let parsed: ActionParams = serde_json::from_str(json).unwrap();
        if let ActionParams::Transcode { codec, settings } = parsed {
            assert_eq!(codec, "hevc");
            assert_eq!(settings.crf, Some(18));
            assert_eq!(settings.preset.as_deref(), Some("slow"));
        } else {
            panic!("expected Transcode variant");
        }
    }

    #[test]
    fn test_transcode_settings_serde_roundtrip() {
        let settings = TranscodeSettings::default()
            .with_crf(Some(23))
            .with_preset(Some("slow".into()))
            .with_bitrate(Some("5M".into()))
            .with_target_vmaf(Some(93))
            .with_max_bitrate(Some("8M".into()))
            .with_min_bitrate(Some("2M".into()))
            .with_sample_strategy(Some(SampleStrategy::Scenes {
                count: 5,
                duration: "4s".into(),
            }))
            .with_fallback(Some(TranscodeFallback::new(24, "medium")))
            .with_vmaf_overrides(Some(HashMap::from([("animation".into(), 88)])))
            .with_channels(Some(TranscodeChannels::Count(6)))
            .with_hw(Some("nvenc".into()))
            .with_hw_fallback(Some(false))
            .with_max_resolution(Some("1080p".into()))
            .with_scale_algorithm(Some("lanczos".into()))
            .with_hdr_mode(Some("tonemap".into()))
            .with_tune(Some("film".into()));

        let json = serde_json::to_string(&settings).unwrap();
        let restored: TranscodeSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(settings, restored);

        let msgpack = rmp_serde::to_vec(&settings).unwrap();
        let restored: TranscodeSettings = rmp_serde::from_slice(&msgpack).unwrap();
        assert_eq!(settings, restored);
    }

    #[test]
    fn test_resolve_target_vmaf_uses_animation_override_for_animation_track() {
        let settings = TranscodeSettings::default()
            .with_target_vmaf(Some(93))
            .with_vmaf_overrides(Some(HashMap::from([("animation".into(), 88)])));
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        let mut track = Track::new(0, TrackType::Video, "h264".into());
        track.is_animation = Some(true);
        file.tracks.push(track);

        assert_eq!(settings.resolve_target_vmaf(&file), Some(88));
    }

    #[test]
    fn test_resolve_target_vmaf_ignores_animation_override_when_unknown() {
        let settings = TranscodeSettings::default()
            .with_target_vmaf(Some(93))
            .with_vmaf_overrides(Some(HashMap::from([("animation".into(), 88)])));
        let file = MediaFile::new(PathBuf::from("/test.mkv")).with_tracks(vec![Track::new(
            0,
            TrackType::Video,
            "h264".into(),
        )]);

        assert_eq!(settings.resolve_target_vmaf(&file), Some(93));
    }

    #[test]
    fn test_transcode_channels_serde_roundtrip() {
        // Count variant serializes as a bare number (untagged)
        let count = TranscodeChannels::Count(6);
        let json = serde_json::to_string(&count).unwrap();
        assert_eq!(json, "6");
        let restored: TranscodeChannels = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, count);

        // Named variant serializes as a bare string (untagged)
        let named = TranscodeChannels::Named("stereo".into());
        let json = serde_json::to_string(&named).unwrap();
        assert_eq!(json, "\"stereo\"");
        let restored: TranscodeChannels = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, named);
    }

    #[test]
    fn test_plan_serde_backward_compat() {
        // JSON without id/policy_hash/evaluated_at should deserialize with defaults
        let json = r#"{
            "file": {"id":"00000000-0000-0000-0000-000000000000","path":"/test.mkv","size":0,"content_hash":"","container":"Other","duration":0.0,"bitrate":null,"tracks":[],"tags":{},"plugin_metadata":{},"introspected_at":"2024-01-01T00:00:00Z"},
            "policy_name": "test",
            "phase_name": "init",
            "actions": [],
            "warnings": [],
            "skip_reason": null
        }"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.policy_name, "test");
        assert!(plan.policy_hash.is_none());
    }

    #[test]
    fn test_operation_type_mux_subtitle_roundtrip() {
        assert_eq!(
            OperationType::parse("mux_subtitle"),
            Some(OperationType::MuxSubtitle)
        );
        assert_eq!(OperationType::MuxSubtitle.as_str(), "mux_subtitle");
    }

    #[test]
    fn test_action_params_mux_subtitle_serde_json_roundtrip() {
        let params = ActionParams::MuxSubtitle {
            subtitle_path: PathBuf::from("/media/movie.eng.srt"),
            language: "eng".into(),
            forced: true,
            title: Some("English (Forced)".into()),
        };
        let json = serde_json::to_string(&params).unwrap();
        let restored: ActionParams = serde_json::from_str(&json).unwrap();
        match restored {
            ActionParams::MuxSubtitle {
                subtitle_path,
                language,
                forced,
                title,
            } => {
                assert_eq!(subtitle_path, PathBuf::from("/media/movie.eng.srt"));
                assert_eq!(language, "eng");
                assert!(forced);
                assert_eq!(title.as_deref(), Some("English (Forced)"));
            }
            other => panic!("expected MuxSubtitle, got {other:?}"),
        }
    }

    #[test]
    fn test_action_params_mux_subtitle_serde_msgpack_roundtrip() {
        let params = ActionParams::MuxSubtitle {
            subtitle_path: PathBuf::from("/media/movie.jpn.srt"),
            language: "jpn".into(),
            forced: false,
            title: None,
        };
        let bytes = rmp_serde::to_vec(&params).unwrap();
        let restored: ActionParams = rmp_serde::from_slice(&bytes).unwrap();
        match restored {
            ActionParams::MuxSubtitle {
                subtitle_path,
                language,
                forced,
                title,
            } => {
                assert_eq!(subtitle_path, PathBuf::from("/media/movie.jpn.srt"));
                assert_eq!(language, "jpn");
                assert!(!forced);
                assert!(title.is_none());
            }
            other => panic!("expected MuxSubtitle, got {other:?}"),
        }
    }

    #[test]
    fn test_operation_type_verify_media_roundtrip() {
        assert_eq!(
            OperationType::parse("verify_media"),
            Some(OperationType::VerifyMedia)
        );
        assert_eq!(OperationType::VerifyMedia.as_str(), "verify_media");
    }

    #[test]
    fn test_action_params_verify_media_serde_json_roundtrip() {
        use crate::verification::VerificationMode;
        let params = ActionParams::VerifyMedia(VerifyMediaParams {
            mode: VerificationMode::Quick,
        });
        let json = serde_json::to_string(&params).unwrap();
        let restored: ActionParams = serde_json::from_str(&json).unwrap();
        match restored {
            ActionParams::VerifyMedia(p) => {
                assert_eq!(p.mode, VerificationMode::Quick);
            }
            other => panic!("expected VerifyMedia, got {other:?}"),
        }
    }

    #[test]
    fn test_action_params_verify_media_serde_msgpack_roundtrip() {
        use crate::verification::VerificationMode;
        let params = ActionParams::VerifyMedia(VerifyMediaParams {
            mode: VerificationMode::Hash,
        });
        let bytes = rmp_serde::to_vec(&params).unwrap();
        let restored: ActionParams = rmp_serde::from_slice(&bytes).unwrap();
        match restored {
            ActionParams::VerifyMedia(p) => {
                assert_eq!(p.mode, VerificationMode::Hash);
            }
            other => panic!("expected VerifyMedia, got {other:?}"),
        }
    }

    #[test]
    fn test_operation_type_quarantine_roundtrip() {
        assert_eq!(
            OperationType::parse("quarantine"),
            Some(OperationType::Quarantine)
        );
        assert_eq!(OperationType::Quarantine.as_str(), "quarantine");
    }

    #[test]
    fn test_action_params_quarantine_serde_json_roundtrip() {
        let params = ActionParams::Quarantine(QuarantineParams {
            reason: "test".into(),
        });
        let json = serde_json::to_string(&params).unwrap();
        let restored: ActionParams = serde_json::from_str(&json).unwrap();
        match restored {
            ActionParams::Quarantine(p) => {
                assert_eq!(p.reason, "test");
            }
            other => panic!("expected Quarantine, got {other:?}"),
        }
    }

    #[test]
    fn test_action_params_quarantine_serde_msgpack_roundtrip() {
        let params = ActionParams::Quarantine(QuarantineParams {
            reason: "test".into(),
        });
        let bytes = rmp_serde::to_vec(&params).unwrap();
        let restored: ActionParams = rmp_serde::from_slice(&bytes).unwrap();
        match restored {
            ActionParams::Quarantine(p) => {
                assert_eq!(p.reason, "test");
            }
            other => panic!("expected Quarantine, got {other:?}"),
        }
    }

    #[test]
    fn test_phase_result_serde_with_temp_path() {
        let mut pr = PhaseResult::new("normalize", PhaseOutcome::Completed);
        pr.temp_path = Some("/media/movie.voom_tmp_abc.mkv".into());
        let json = serde_json::to_string(&pr).unwrap();
        assert!(json.contains("temp_path"));
        let restored: PhaseResult = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.temp_path.as_deref(),
            Some("/media/movie.voom_tmp_abc.mkv")
        );
    }

    #[test]
    fn test_phase_result_serde_without_temp_path() {
        let pr = PhaseResult::new("normalize", PhaseOutcome::Completed);
        let json = serde_json::to_string(&pr).unwrap();
        assert!(!json.contains("temp_path"));
        let restored: PhaseResult = serde_json::from_str(&json).unwrap();
        assert!(restored.temp_path.is_none());
    }

    #[test]
    fn test_phase_output_default() {
        let out = PhaseOutput::new();
        assert!(!out.completed);
        assert!(!out.modified);
        assert!(out.outcome.is_none());
        assert_eq!(out.error_count, 0);
        assert_eq!(out.warning_count, 0);
    }

    #[test]
    fn test_phase_output_serde_roundtrip() {
        let out = PhaseOutput {
            completed: true,
            modified: false,
            outcome: Some("ok".into()),
            error_count: 1,
            warning_count: 2,
        };
        let json = serde_json::to_string(&out).unwrap();
        let restored: PhaseOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out, restored);
    }

    #[test]
    fn test_phase_output_field_set_matches_struct() {
        // Sanity check: the const list and struct fields stay in sync.
        for name in PHASE_OUTPUT_FIELDS {
            match *name {
                "completed" | "modified" | "outcome" | "error_count" | "warning_count" => {}
                other => panic!("unexpected phase-output field {other}"),
            }
        }
    }

    #[test]
    fn test_phase_result_backward_compat_missing_temp_path() {
        let json = r#"{"phase_name":"normalize","outcome":"Completed","actions":[],"file_modified":false,"skip_reason":null,"duration_ms":0}"#;
        let pr: PhaseResult = serde_json::from_str(json).unwrap();
        assert!(pr.temp_path.is_none());
    }
}
