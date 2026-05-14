//! Shared domain types for VOOM: media files, tracks, plans, events, and capabilities.

pub mod bad_file;
pub mod capabilities;
pub mod capability_map;
pub mod capability_resolution;
pub mod compiled;
pub mod evaluation;
pub mod errors;
pub mod estimate;
pub mod events;
pub mod host_types;
pub mod job;
pub mod media;
pub mod plan;
pub mod plugin_stats;
pub mod safeguard;
pub mod scan_session_mutations;
pub mod snapshot;
pub mod stats;
pub mod storage;
pub mod temp_file;
#[cfg(feature = "testing")]
#[allow(
    clippy::unwrap_used,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use
)]
pub mod test_support;
pub mod transcode;
pub mod transition;
pub mod utils;
pub mod verification;

pub use bad_file::{BadFile, BadFileSource};
pub use capabilities::Capability;
pub use capability_map::CapabilityMap;
pub use errors::{Result, VoomError};
pub use estimate::{
    ActionEstimate, CostModelSample, EstimateInput, EstimateModel, EstimateOperationKey,
    EstimateRun, FileEstimate, estimate_plans,
};
pub use events::Event;
pub use job::{DiscoveredFilePayload, Job, JobStatus, JobUpdate};
pub use media::{Container, CropDetection, CropRect, MediaFile, Track, TrackType};
pub use plan::{
    ActionParams, ActionResult, CropSettings, OperationType, PHASE_OUTPUT_FIELDS, PhaseOutcome,
    PhaseOutput, PhaseResult, Plan, PlannedAction, SampleStrategy, TranscodeChannels,
    TranscodeFallback, TranscodeSettings,
};
pub use safeguard::{SafeguardKind, SafeguardViolation};
pub use snapshot::MetadataSnapshot;
pub use stats::{
    AudioStats, FileStats, JobAggregateStats, LibrarySnapshot, ProcessingAggregateStats,
    ProcessingOutcome, SnapshotTrigger, SubtitleStats, VideoStats,
};
pub use storage::{
    BadFileFilters, BadFileStorage, CostModelSampleFilters, EstimateStorage, EventLogFilters,
    EventLogRecord, EventLogStorage, FileFilters, FileStorage, FileTransitionStorage,
    HealthCheckFilters, HealthCheckRecord, HealthCheckStorage, JobFilters, JobStorage,
    MaintenanceStorage, PlanStorage, PlanSummary, PluginDataStorage, SnapshotStorage, StorageTrait,
    TranscodeOutcomeFilters, TranscodeOutcomeStorage,
};
pub use transcode::TranscodeOutcome;
pub use transition::{
    DiscoveredFile, FileStatus, FileTransition, ReconcileResult, TransitionSource,
};
pub use verification::{
    IntegritySummary, VerificationFilters, VerificationMode, VerificationOutcome,
    VerificationRecord,
};
