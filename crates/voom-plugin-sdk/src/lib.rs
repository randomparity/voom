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

pub mod event;
pub mod host;
pub mod types;

// ── WASM boundary types ─────────────────────────────────────────────
// These are the types WASM plugins actually need at the host/guest boundary.

/// SDK helper types for building plugin info and returning event results.
pub use types::{OnEventResult, PluginInfoData};

/// Event serialization/deserialization helpers (`MessagePack` across WASM boundary).
pub use event::{
    deserialize_event, deserialize_json, load_plugin_config, load_plugin_config_named,
    serialize_event, serialize_json,
};

/// Host function abstractions for calling back into the kernel from WASM.
pub use host::{HostFunctions, HttpResponse, ToolOutput};

// ── Domain types used inside event payloads ─────────────────────────
// These are needed when deserializing event payloads into concrete types.
// Import additional domain types directly from `voom_domain` if needed.

pub use voom_domain::events::Event;
pub use voom_domain::plan::{ActionParams, OperationType, TranscodeChannels, TranscodeSettings};

// ── Utilities ─────────────────────────────────────────────────────────

pub use voom_domain::utils::language::language_code_from_name;

// ── Top-level domain re-exports (most-used types) ──────────────────
// These let plugins write `use voom_plugin_sdk::MediaFile` etc.
pub use voom_domain::capabilities::Capability;
pub use voom_domain::events::{
    EventResult, FileIntrospectedEvent, MetadataEnrichedEvent, PlanCompletedEvent, PlanCreatedEvent,
};
pub use voom_domain::media::{Container, MediaFile, Track, TrackType};
pub use voom_domain::plan::{ActionResult, PhaseOutcome, PhaseResult, Plan, PlannedAction};

// ── Full domain re-exports (for plugins that need deeper access) ────
// Available under `voom_plugin_sdk::domain` for explicit opt-in.
pub mod domain {
    //! Full domain type re-exports for plugins that need deeper access
    //! beyond the standard WASM boundary types.
    pub use voom_domain::capabilities::Capability;
    pub use voom_domain::events::{
        EventResult, FileIntrospectedEvent, MetadataEnrichedEvent, PlanCompletedEvent,
        PlanCreatedEvent,
    };
    pub use voom_domain::media::{Container, MediaFile, Track, TrackType};
    pub use voom_domain::plan::{ActionResult, PhaseOutcome, PhaseResult, Plan, PlannedAction};
}
