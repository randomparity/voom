//! Shared domain types for VOOM: media files, tracks, plans, events, and capabilities.

pub mod capabilities;
pub mod errors;
pub mod events;
pub mod job;
pub mod media;
pub mod plan;
pub mod stats;
pub mod storage;
#[cfg(feature = "testing")]
pub mod test_support;
pub mod utils;

pub use capabilities::Capability;
pub use errors::{Result, VoomError};
pub use events::Event;
pub use job::{Job, JobStatus, JobUpdate};
pub use media::{Container, MediaFile, Track, TrackType};
pub use plan::{ActionResult, OperationType, PhaseOutcome, PhaseResult, Plan, PlannedAction};
pub use stats::ProcessingStats;
pub use storage::{FileFilters, StorageTrait, StoredPlan};
