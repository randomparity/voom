//! VOOM Plugin SDK — helpers for building WASM plugins.
//!
//! This crate provides type definitions, serialization helpers, and convenience
//! utilities for writing WASM plugins that run inside the VOOM kernel.
//!
//! # Writing a WASM plugin
//!
//! 1. Create a new Rust library crate with `crate-type = ["cdylib"]`
//! 2. Add `voom-plugin-sdk` and `wit-bindgen` as dependencies
//! 3. Use `wit_bindgen::generate!` to generate bindings from the VOOM WIT interfaces
//! 4. Implement the generated `Guest` trait using SDK helpers
//! 5. Build with `cargo build --target wasm32-wasip1`
//!
//! # Example
//!
//! ```rust,ignore
//! use voom_plugin_sdk::*;
//!
//! wit_bindgen::generate!({
//!     world: "voom-plugin",
//!     path: "path/to/voom-wit/wit",
//! });
//!
//! struct MyPlugin;
//!
//! impl Guest for MyPlugin {
//!     fn get_info() -> PluginInfo {
//!         PluginInfo::new("my-plugin", "0.1.0")
//!             .capability("enrich_metadata:my-plugin")
//!             .handles("file.introspected")
//!     }
//!
//!     fn on_event(event: EventData) -> Option<EventResult> {
//!         let domain_event = deserialize_event(&event.payload).ok()?;
//!         // Process event...
//!         None
//!     }
//! }
//!
//! export!(MyPlugin);
//! ```

pub mod event;
pub mod types;

// Re-export domain types commonly used by plugins.
pub use voom_domain::capabilities::Capability;
pub use voom_domain::events::{Event, EventResult};
pub use voom_domain::media::{Container, MediaFile, Track, TrackType};
pub use voom_domain::plan::{
    ActionResult, OperationType, PhaseOutcome, PhaseResult, Plan, PlannedAction,
};

// Re-export the domain crate for direct access by plugins.
pub use voom_domain;

// Re-export helpers.
pub use event::{
    deserialize_event, deserialize_json, load_plugin_config, serialize_event, serialize_json,
};
pub use types::{OnEventResult, PluginInfoData};
