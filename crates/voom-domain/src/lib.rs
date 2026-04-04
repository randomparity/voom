//! Shared domain types for VOOM: media files, tracks, plans, events, and capabilities.

pub mod bad_file;
pub mod capabilities;
pub mod capability_map;
pub mod errors;
pub mod events;
pub mod host_types;
pub mod job;
pub mod media;
pub mod plan;
pub mod safeguard;
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
pub mod transition;
pub mod utils;

pub use bad_file::{BadFile, BadFileSource};
pub use capabilities::Capability;
pub use capability_map::CapabilityMap;
pub use errors::{Result, VoomError};
pub use events::Event;
pub use job::{DiscoveredFilePayload, Job, JobStatus, JobUpdate};
pub use media::{Container, MediaFile, Track, TrackType};
pub use plan::{
    ActionParams, ActionResult, OperationType, PhaseOutcome, PhaseResult, Plan, PlannedAction,
    TranscodeChannels, TranscodeSettings,
};
pub use safeguard::{SafeguardKind, SafeguardViolation};
pub use stats::{
    AudioStats, FileStats, JobAggregateStats, LibrarySnapshot, ProcessingAggregateStats,
    ProcessingOutcome, SnapshotTrigger, SubtitleStats, VideoStats,
};
pub use storage::{
    BadFileFilters, BadFileStorage, EventLogFilters, EventLogRecord, EventLogStorage, FileFilters,
    FileStorage, FileTransitionStorage, HealthCheckFilters, HealthCheckRecord, HealthCheckStorage,
    JobFilters, JobStorage, MaintenanceStorage, PlanStorage, PlanSummary, PluginDataStorage,
    SnapshotStorage, StorageTrait,
};
pub use transition::{
    DiscoveredFile, FileStatus, FileTransition, ReconcileResult, TransitionSource,
};
