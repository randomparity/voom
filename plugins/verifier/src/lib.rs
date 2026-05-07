//! Media file integrity verifier plugin.
//!
//! Three modes: quick (ffprobe header), thorough (ffmpeg decode),
//! hash (sha256 bit-rot detection). Library-callable for CLI; bus
//! subscriber for DSL-driven `verify` phase plans.

pub mod config;
pub mod hash;
pub mod quarantine;
pub mod quick;
pub mod thorough;

// Stub modules so cargo build succeeds.
