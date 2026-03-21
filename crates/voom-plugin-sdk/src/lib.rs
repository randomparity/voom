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
//! // PluginInfo here is the WIT-generated record (plain struct), not the SDK builder.
//! impl Guest for MyPlugin {
//!     fn get_info() -> PluginInfo {
//!         PluginInfo {
//!             name: "my-plugin".to_string(),
//!             version: "0.1.0".to_string(),
//!             capabilities: vec![
//!                 Capability::EnrichMetadata(EnrichCap {
//!                     source: "my-plugin".to_string(),
//!                 }),
//!             ],
//!         }
//!     }
//!
//!     fn handles(event_type: String) -> bool {
//!         event_type == "file.introspected"
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

#![allow(clippy::missing_errors_doc)]

pub mod event;
pub mod host;
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

/// `PluginInfoData` mirrors the WIT `plugin-info` record (name, version, capabilities).
/// For the builder pattern in non-WIT contexts, use `types::PluginInfo` directly.
/// The builder is not re-exported here to avoid collision with WIT-generated `PluginInfo`.
pub use types::{OnEventResult, PluginInfoData};

// Re-export host abstractions for WASM plugins.
pub use host::{HostFunctions, HttpResponse, ToolOutput};
