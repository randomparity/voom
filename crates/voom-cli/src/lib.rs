//! Library interface for `voom-cli`.
//!
//! Re-exports the internal modules needed by integration tests and the web
//! server component. The binary entry point (`src/main.rs`) wires these
//! together; this crate exposes them for programmatic use.

pub mod app;
pub mod cli;
pub mod commands;
pub mod config;
pub mod introspect;
pub mod kernel_invoke;
pub mod lock;
pub mod output;
pub mod paths;
pub mod policy_map;
pub mod progress;
pub mod recovery;
pub mod retention;
pub mod tools;
