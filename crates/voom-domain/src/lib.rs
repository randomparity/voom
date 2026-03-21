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
pub mod test_support;
pub mod utils;

pub use bad_file::{BadFile, BadFileSource};
pub use capabilities::Capability;
pub use errors::{Result, VoomError};
pub use events::event_types;
pub use events::Event;
pub use job::{Job, JobStatus, JobUpdate};
pub use media::{Container, MediaFile, Track, TrackType};
pub use plan::{ActionResult, OperationType, PhaseOutcome, PhaseResult, Plan, PlannedAction};
pub use stats::{ProcessingOutcome, ProcessingStats};
pub use storage::{BadFileFilters, FileFilters, JobFilters, StorageTrait, StoredPlan};
