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
pub use events::Event;
pub use job::{DiscoveredFilePayload, Job, JobStatus, JobUpdate};
pub use media::{Container, MediaFile, Track, TrackType};
pub use plan::{
    ActionParams, ActionResult, OperationType, PhaseOutcome, PhaseOutput, PhaseResult, Plan,
    PlannedAction, TranscodeChannels, TranscodeSettings, PHASE_OUTPUT_FIELDS,
};
pub use safeguard::{SafeguardKind, SafeguardViolation};
pub use snapshot::MetadataSnapshot;
pub use stats::{
    AudioStats, FileStats, JobAggregateStats, LibrarySnapshot, LibrarySnapshotInput,
    ProcessingAggregateStats, ProcessingOutcome, SavingsBucketInput, SnapshotTrigger,
    SubtitleStats, VideoStats,
};
pub use transcode::TranscodeOutcome;
pub use transition::{
    DiscoveredFile, FileStatus, FileTransition, ReconcileResult, TransitionSource,
};
pub use verification::{
    IntegritySummary, VerificationFilters, VerificationMode, VerificationOutcome,
    VerificationRecord, VerificationRecordInput,
};
