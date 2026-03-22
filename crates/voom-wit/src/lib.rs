//! WIT type conversion utilities for VOOM WASM plugin boundary.
//!
//! This crate provides bidirectional conversion between `voom-domain` types
//! and the serialized forms used at the WASM component model boundary.
//! Event payloads cross the WASM boundary as MessagePack-encoded bytes.

#![allow(clippy::missing_errors_doc)]

pub mod convert;

pub use convert::{
    capability_from_wit, capability_to_wit, event_from_wasm, event_result_from_wasm,
    event_result_to_wasm, event_to_wasm, WasmEventResult,
};

/// The path to the WIT interface definitions, for use by `bindgen!` and `wit_bindgen::generate!`.
pub const WIT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/wit");
