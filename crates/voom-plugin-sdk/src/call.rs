//! Typed Call/CallResponse helpers for WASM plugin authors. These wrap the
//! MessagePack ABI defined by `voom-wit::on-call` so plugin code can work
//! with native Rust types end-to-end.
//!
//! Plugins call `decode_call` at the top of their `on-call` handler to get a
//! typed `WasmCall`, do their work (using `emit_file_discovered` /
//! `emit_root_walk_completed` / `is_cancelled` to drive streaming Calls),
//! then call `encode_response` to encode their `CallResponse` for return.

pub use voom_domain::call::{CallResponse, DiscoveryError, ScanSummary};
pub use voom_domain::wasm_call::{WasmCall, WasmScanOptions};

use crate::host::HostFunctions;

/// Decode the host-supplied Call bytes (the `call: list<u8>` parameter of
/// `on-call`) into the typed `WasmCall` enum. Plugins call this at the top
/// of their `on-call` implementation.
///
/// # Errors
/// Returns an error string suitable for returning directly from `on-call` if
/// the bytes cannot be decoded.
pub fn decode_call(bytes: &[u8]) -> Result<WasmCall, String> {
    rmp_serde::from_slice(bytes).map_err(|e| format!("failed to decode WasmCall: {e}"))
}

/// Encode a `CallResponse` to `MessagePack` bytes for returning from `on-call`.
///
/// # Errors
/// Returns an error string if encoding fails. `CallResponse` derives
/// `Serialize` so the encode-failure path is defensive.
pub fn encode_response(response: &CallResponse) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec_named(response).map_err(|e| format!("failed to encode CallResponse: {e}"))
}

/// Emit a `FileDiscoveredEvent` to the active streaming Call's sink via the
/// host adapter. Use inside an `on-call` handler for `WasmCall::ScanLibrary`.
///
/// Calling outside an active streaming Call returns Err (the host returns
/// "no active streaming call ...").
///
/// # Errors
/// Returns the host's error string verbatim if emission fails (no active
/// call, payload too large, encode failure).
pub fn emit_file_discovered(
    host: &dyn HostFunctions,
    event: &voom_domain::events::FileDiscoveredEvent,
) -> Result<(), String> {
    let bytes =
        rmp_serde::to_vec_named(event).map_err(|e| format!("encode FileDiscoveredEvent: {e}"))?;
    host.emit_call_item(&bytes)
}

/// Emit a `RootWalkCompletedEvent` to the active streaming Call's
/// `root_done` sender. Use inside an `on-call` handler for
/// `WasmCall::ScanLibrary` after each root finishes.
///
/// # Errors
/// Returns Err if no streaming Call is active OR if the active call has no
/// `root_done` sender (caller didn't ask for per-root signalling).
pub fn emit_root_walk_completed(
    host: &dyn HostFunctions,
    event: &voom_domain::events::RootWalkCompletedEvent,
) -> Result<(), String> {
    let bytes = rmp_serde::to_vec_named(event)
        .map_err(|e| format!("encode RootWalkCompletedEvent: {e}"))?;
    host.emit_root_walk_completed(&bytes)
}

/// Poll the cancellation token for the active streaming Call. Returns false
/// when no streaming Call is active.
pub fn is_cancelled(host: &dyn HostFunctions) -> bool {
    host.call_is_cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::FileDiscoveredEvent;

    /// Stub host whose emit_* methods return what they're told to.
    struct StubHost {
        emit_result: std::cell::RefCell<Result<(), String>>,
        cancelled: bool,
    }
    impl HostFunctions for StubHost {
        fn get_plugin_data(&self, _: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn set_plugin_data(&self, _: &str, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn log(&self, _: &str, _: &str) {}
        fn emit_call_item(&self, _: &[u8]) -> Result<(), String> {
            self.emit_result.borrow().clone()
        }
        fn emit_root_walk_completed(&self, _: &[u8]) -> Result<(), String> {
            self.emit_result.borrow().clone()
        }
        fn call_is_cancelled(&self) -> bool {
            self.cancelled
        }
    }

    #[test]
    fn decode_call_roundtrip_orchestrate() {
        let wasm = WasmCall::Orchestrate {
            plans: vec![],
            policy_name: "demo".into(),
        };
        let bytes = rmp_serde::to_vec_named(&wasm).expect("encode");
        let back = decode_call(&bytes).expect("decode");
        match back {
            WasmCall::Orchestrate { policy_name, .. } => assert_eq!(policy_name, "demo"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_call_errors_on_malformed_bytes() {
        let err = decode_call(b"not msgpack").unwrap_err();
        assert!(err.contains("failed to decode WasmCall"));
    }

    #[test]
    fn encode_response_roundtrip_scan_library() {
        let resp = CallResponse::ScanLibrary(ScanSummary::new(3, vec![], 1));
        let bytes = encode_response(&resp).expect("encode");
        let back: CallResponse = rmp_serde::from_slice(&bytes).expect("decode");
        match back {
            CallResponse::ScanLibrary(s) => {
                assert_eq!(s.file_count, 3);
                assert_eq!(s.roots_scanned, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn emit_file_discovered_routes_to_host_emit_call_item() {
        let host = StubHost {
            emit_result: std::cell::RefCell::new(Ok(())),
            cancelled: false,
        };
        let evt = FileDiscoveredEvent::new(PathBuf::from("/test.mkv"), 1024, None);
        let result = emit_file_discovered(&host, &evt);
        assert!(result.is_ok());
    }

    #[test]
    fn emit_file_discovered_propagates_host_error() {
        let host = StubHost {
            emit_result: std::cell::RefCell::new(Err("host rejected".to_string())),
            cancelled: false,
        };
        let evt = FileDiscoveredEvent::new(PathBuf::from("/test.mkv"), 1024, None);
        let err = emit_file_discovered(&host, &evt).unwrap_err();
        assert_eq!(err, "host rejected");
    }

    #[test]
    fn is_cancelled_routes_to_host_call_is_cancelled() {
        let host = StubHost {
            emit_result: std::cell::RefCell::new(Ok(())),
            cancelled: true,
        };
        assert!(is_cancelled(&host));

        let host2 = StubHost {
            emit_result: std::cell::RefCell::new(Ok(())),
            cancelled: false,
        };
        assert!(!is_cancelled(&host2));
    }
}
