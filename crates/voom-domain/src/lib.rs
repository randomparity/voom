//! Shared domain types for VOOM: media files, tracks, plans, events, and capabilities.

pub mod bad_file;
pub mod capabilities;
pub mod errors;
pub mod events;
pub mod job;
pub mod media;
pub mod plan;
pub mod stats;
pub mod storage;
#[cfg(feature = "testing")]
#[allow(
    clippy::unwrap_used,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use
)]
pub mod test_support;
pub mod utils;

pub use bad_file::{BadFile, BadFileSource};
pub use capabilities::Capability;
pub use errors::{Result, VoomError};
pub use events::Event;
pub use job::{Job, JobStatus, JobUpdate};
pub use media::{Container, MediaFile, Track, TrackType};
pub use plan::{
    ActionParams, ActionResult, OperationType, PhaseOutcome, PhaseResult, Plan, PlannedAction,
};
pub use stats::{ProcessingOutcome, ProcessingStats};
pub use storage::{
    BadFileFilters, BadFileStorage, FileFilters, FileHistoryStorage, FileStorage, JobFilters,
    JobStorage, MaintenanceStorage, PlanStorage, PluginDataStorage, StatsStorage, StorageTrait,
    StoredPlan,
};
