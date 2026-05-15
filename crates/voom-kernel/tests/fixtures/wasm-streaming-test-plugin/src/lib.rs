//! Test WASM plugin exercising the Phase 2 streaming on-call surface.
//!
//! Built independently from the workspace (excluded in the root Cargo.toml).
//! Build with `./build.sh`. The produced `.wasm` is consumed by
//! `crates/voom-kernel/tests/wasm_streaming_dispatch.rs`.
//!
//! Behaviour: for a `WasmCall::ScanLibrary`, emit 5 synthetic
//! `FileDiscoveredEvent`s via the host fn `emit-call-item`, optionally
//! emit a `RootWalkCompletedEvent` via `emit-root-walk-completed`, and
//! return `CallResponse::ScanLibrary(ScanSummary)`. Polls
//! `call-is-cancelled` between items.
//!
//! Mirror structs (`WasmCall`, `WasmScanOptions`, `ScanSessionId`,
//! `FileDiscoveredEvent`, `RootWalkCompletedEvent`, `ScanSummary`,
//! `DiscoveryError`, `CallResponse`) match the serde field names of
//! the canonical host types in `voom-domain`. We do not depend on
//! `voom-domain` to keep this fixture buildable for `wasm32-wasip2`
//! without pulling tokio/regex/etc.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

wit_bindgen::generate!({
    world: "voom-plugin",
    path: "../../../../voom-wit/wit",
});

use exports::voom::plugin::plugin::{
    Capability as WitCapability, DiscoverCap, EventData, EventResult, Guest as PluginGuest,
    PluginInfo,
};
use voom::plugin::host;

// ---------------------------------------------------------------------------
// Serde mirrors of the host-side types crossing the WASM boundary.
// These must stay structurally compatible with the canonical definitions in
// `voom-domain` (WasmCall, FileDiscoveredEvent, CallResponse, ScanSummary, …).
// The host uses `rmp_serde::to_vec_named` (named-field map encoding), so
// field NAMES are the contract; field ORDER is not.
// ---------------------------------------------------------------------------

/// Mirror of `voom_domain::wasm_call::WasmCall`. Externally-tagged enum:
/// MessagePack encodes as `{"<Variant>": {...payload...}}`. The fixture only
/// expects `ScanLibrary`; the other variants accept any payload shape via
/// `IgnoredAny` so deserialization wouldn't crash if the caller sent one.
#[derive(Deserialize)]
enum WasmCall {
    #[allow(dead_code)]
    EvaluatePolicy(serde::de::IgnoredAny),
    #[allow(dead_code)]
    Orchestrate(serde::de::IgnoredAny),
    ScanLibrary {
        // `uri` is part of the wire format but unused by this fixture.
        #[allow(dead_code)]
        uri: String,
        options: WasmScanOptions,
        scan_session: ScanSessionId,
    },
}

/// Mirror of `voom_domain::wasm_call::WasmScanOptions`.
#[allow(dead_code)] // fields are deserialized; we only read a couple.
#[derive(Debug, Deserialize)]
struct WasmScanOptions {
    root: PathBuf,
    recursive: bool,
    hash_files: bool,
    workers: usize,
}

/// Mirror of `voom_domain::transition::ScanSessionId`. Newtype around a Uuid.
///
/// In non-human-readable formats like MessagePack, the canonical `uuid` crate
/// serializes a Uuid as a 16-byte byte array (see
/// `uuid::external::serde_support`), so we deserialize/serialize the inner
/// `Uuid` as raw bytes here too. Storing the bytes lets the fixture round-trip
/// the value back to the host (e.g. inside `RootWalkCompletedEvent`) with
/// identical wire encoding.
#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
struct ScanSessionId(uuid_bytes::UuidBytes);

mod uuid_bytes {
    //! Tiny helper for round-tripping `Uuid` as 16 raw bytes without depending
    //! on the `uuid` crate. Matches the wire format that
    //! `rmp_serde::to_vec_named(&Uuid::new_v4())` produces.

    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    #[derive(Debug, Clone, Copy)]
    pub struct UuidBytes(pub [u8; 16]);

    impl<'de> Deserialize<'de> for UuidBytes {
        fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> de::Visitor<'de> for V {
                type Value = [u8; 16];

                fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                    f.write_str("a 16-byte UUID byte array")
                }

                fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                    if v.len() != 16 {
                        return Err(E::custom(format!(
                            "uuid bytes must be 16, got {}",
                            v.len()
                        )));
                    }
                    let mut out = [0u8; 16];
                    out.copy_from_slice(v);
                    Ok(out)
                }

                fn visit_borrowed_bytes<E: de::Error>(
                    self,
                    v: &'de [u8],
                ) -> Result<Self::Value, E> {
                    self.visit_bytes(v)
                }

                fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                    self.visit_bytes(&v)
                }

                fn visit_seq<A: de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut out = [0u8; 16];
                    for slot in &mut out {
                        *slot = seq
                            .next_element::<u8>()?
                            .ok_or_else(|| de::Error::custom("uuid byte sequence too short"))?;
                    }
                    if seq.next_element::<u8>()?.is_some() {
                        return Err(de::Error::custom("uuid byte sequence too long"));
                    }
                    Ok(out)
                }
            }
            let bytes = de.deserialize_bytes(V)?;
            Ok(UuidBytes(bytes))
        }
    }

    impl Serialize for UuidBytes {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_bytes(&self.0)
        }
    }
}

/// Mirror of `voom_domain::events::FileDiscoveredEvent`. Encoded with named
/// fields via `rmp_serde::to_vec_named` so the host can deserialize it as
/// the canonical type.
#[derive(Debug, Serialize)]
struct FileDiscoveredEvent {
    path: PathBuf,
    size: u64,
    content_hash: Option<String>,
}

/// Mirror of `voom_domain::events::RootWalkCompletedEvent`.
#[derive(Debug, Serialize)]
struct RootWalkCompletedEvent {
    root: PathBuf,
    session: ScanSessionId,
    duration_ms: u64,
}

/// Mirror of `voom_domain::call::ScanSummary`.
#[derive(Debug, Serialize)]
struct ScanSummary {
    file_count: u64,
    errors: Vec<DiscoveryError>,
    roots_scanned: u64,
}

/// Mirror of `voom_domain::call::DiscoveryError`.
#[derive(Debug, Serialize)]
struct DiscoveryError {
    path: String,
    message: String,
}

/// Mirror of `voom_domain::call::CallResponse`. Externally-tagged enum. We
/// only ever emit the `ScanLibrary` variant from this fixture.
#[derive(Serialize)]
enum CallResponse {
    ScanLibrary(ScanSummary),
}

// ---------------------------------------------------------------------------
// WIT export implementation.
// ---------------------------------------------------------------------------

struct WasmStreamingTestPlugin;

impl PluginGuest for WasmStreamingTestPlugin {
    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "wasm-streaming-test-plugin".to_string(),
            version: "0.0.1".to_string(),
            description: Some("Test WASM plugin exercising streaming on-call".to_string()),
            author: None,
            license: None,
            homepage: None,
            capabilities: vec![WitCapability::Discover(DiscoverCap {
                schemes: vec!["mock-fs".to_string()],
            })],
        }
    }

    fn handles(_event_type: String) -> bool {
        false
    }

    fn on_event(_event: EventData) -> Option<EventResult> {
        None
    }

    fn on_call(call: Vec<u8>) -> Result<Vec<u8>, String> {
        let decoded: WasmCall = rmp_serde::from_slice(&call)
            .map_err(|e| format!("decode WasmCall: {e}"))?;

        match decoded {
            WasmCall::ScanLibrary {
                uri: _, // unused; the fixture synthesizes paths from options.root
                options,
                scan_session,
            } => run_scan_library(options, scan_session),
            WasmCall::EvaluatePolicy(_) => {
                Err("EvaluatePolicy is not implemented in the test fixture".to_string())
            }
            WasmCall::Orchestrate(_) => {
                Err("Orchestrate is not implemented in the test fixture".to_string())
            }
        }
    }
}

fn run_scan_library(
    options: WasmScanOptions,
    session: ScanSessionId,
) -> Result<Vec<u8>, String> {
    let mut emitted: u64 = 0;
    for i in 0..5 {
        if host::call_is_cancelled() {
            break;
        }
        let event = FileDiscoveredEvent {
            path: options.root.join(format!("file-{i}.mkv")),
            size: 1024 + (i as u64) * 10,
            content_hash: None,
        };
        let bytes = rmp_serde::to_vec_named(&event)
            .map_err(|e| format!("encode FileDiscoveredEvent: {e}"))?;
        host::emit_call_item(&bytes).map_err(|e| format!("emit-call-item: {e}"))?;
        emitted += 1;
    }

    if emitted > 0 {
        let done = RootWalkCompletedEvent {
            root: options.root.clone(),
            session,
            duration_ms: 0,
        };
        let bytes = rmp_serde::to_vec_named(&done)
            .map_err(|e| format!("encode RootWalkCompletedEvent: {e}"))?;
        // emit-root-walk-completed returns Err when no root_done sender is
        // attached to the streaming context. That's expected in our basic
        // streaming test (we pass `root_done: None`), so we tolerate the Err
        // here rather than failing the whole call.
        let _ = host::emit_root_walk_completed(&bytes);
    }

    let summary = CallResponse::ScanLibrary(ScanSummary {
        file_count: emitted,
        errors: Vec::new(),
        roots_scanned: 1,
    });
    rmp_serde::to_vec_named(&summary).map_err(|e| format!("encode CallResponse: {e}"))
}

export!(WasmStreamingTestPlugin);
