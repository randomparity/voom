//! End-to-end test for Phase 2 WASM streaming dispatch (issue #378).
//!
//! Loads the fixture plugin at
//! `tests/fixtures/wasm-streaming-test-plugin/` through `WasmPluginLoader`
//! and exercises the full `Kernel::dispatch_to_capability` →
//! `with_streaming_call` → `on-call` → `emit-call-item` round-trip.
//!
//! The fixture must be built before running these tests:
//!
//!     ./crates/voom-kernel/tests/fixtures/wasm-streaming-test-plugin/build.sh
//!
//! Then:
//!
//!     cargo test -p voom-kernel --test wasm_streaming_dispatch --features wasm
//!
//! The fixture-dependent tests skip gracefully (printing a notice to
//! stderr and returning early) when the `.wasm` is missing, so a fresh
//! clone without a built fixture still has a green test run. The
//! size-cap helper tests (`check_call_payload_size_*`) do not depend on
//! the fixture and always run when the `wasm` feature is enabled.

#![cfg(feature = "wasm")]

use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use voom_domain::call::{Call, CallResponse};
use voom_domain::capabilities::CapabilityQuery;
use voom_domain::events::{FileDiscoveredEvent, RootWalkCompletedEvent};
use voom_domain::scan::ScanOptions;
use voom_domain::transition::ScanSessionId;
use voom_kernel::Kernel;
use voom_kernel::loader::wasm::{
    MAX_WASM_EVENT_PAYLOAD, WasmPluginLoader, check_call_payload_size,
};

/// Resolve the path to the built test-fixture `.wasm`.
fn fixture_wasm_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join(
        "tests/fixtures/wasm-streaming-test-plugin/\
         target/wasm32-wasip2/release/wasm_streaming_test_plugin.wasm",
    )
}

/// Build a kernel that has the fixture WASM plugin loaded and registered.
///
/// Returns `None` (after printing a skip notice with the build command
/// to stderr) when the fixture `.wasm` hasn't been built. Callers
/// should early-return on `None` so the test passes as a skip rather
/// than failing on first clones / dev workflows that haven't run the
/// fixture's `build.sh` yet.
fn bootstrap_kernel_with_fixture() -> Option<Kernel> {
    let wasm_path = fixture_wasm_path();
    if !wasm_path.exists() {
        eprintln!(
            "WASM test fixture not built. Run:\n  \
             ./crates/voom-kernel/tests/fixtures/wasm-streaming-test-plugin/build.sh\n\
             (expected: {})\nSkipping this test.",
            wasm_path.display()
        );
        return None;
    }

    // Leak the loader: its background epoch thread holds an Arc<Engine>, and
    // letting it live for the duration of the test process keeps WASM plugin
    // execution from being interrupted unpredictably between assertions.
    let loader: &'static WasmPluginLoader =
        Box::leak(Box::new(WasmPluginLoader::new().expect("create loader")));

    let plugin = loader.load(&wasm_path).expect("load fixture WASM plugin");

    let mut kernel = Kernel::new();
    // The manifest declares priority 50; we pass it explicitly here.
    kernel
        .register_plugin(plugin, 50)
        .expect("register fixture plugin");
    Some(kernel)
}

#[test]
fn wasm_plugin_streams_files_via_emit_call_item() {
    let Some(kernel) = bootstrap_kernel_with_fixture() else {
        return;
    };
    let (sink, mut rx) = mpsc::channel::<FileDiscoveredEvent>(16);
    let cancel = CancellationToken::new();

    let call = Call::ScanLibrary {
        uri: "mock-fs:///synthetic".into(),
        options: ScanOptions::new("/synthetic"),
        scan_session: ScanSessionId::new(),
        sink,
        root_done: None,
        cancel,
    };

    let response = kernel
        .dispatch_to_capability(
            CapabilityQuery::Sharded {
                kind: "discover".into(),
                key: "mock-fs".into(),
            },
            call,
        )
        .expect("dispatch should succeed");

    // Drain the receiver: the WASM plugin should have emitted exactly 5
    // FileDiscoveredEvents via the host's emit-call-item bridge.
    let mut received = Vec::new();
    while let Ok(event) = rx.try_recv() {
        received.push(event);
    }
    assert_eq!(
        received.len(),
        5,
        "fixture should stream exactly 5 files via emit-call-item"
    );

    for (i, ev) in received.iter().enumerate() {
        let expected = format!("file-{i}.mkv");
        assert!(
            ev.path.to_string_lossy().ends_with(&expected),
            "file {i}: path {:?} should end with {expected:?}",
            ev.path,
        );
        assert!(ev.size >= 1024, "file {i}: size should be >= 1024");
    }

    match response {
        CallResponse::ScanLibrary(summary) => {
            assert_eq!(summary.file_count, 5);
            assert_eq!(summary.roots_scanned, 1);
            assert!(summary.errors.is_empty());
        }
        other => panic!("expected CallResponse::ScanLibrary, got {other:?}"),
    }
}

#[test]
fn wasm_plugin_respects_cancellation() {
    let Some(kernel) = bootstrap_kernel_with_fixture() else {
        return;
    };
    let (sink, mut rx) = mpsc::channel::<FileDiscoveredEvent>(16);
    let cancel = CancellationToken::new();

    // Pre-cancel: the plugin's first `call_is_cancelled()` poll inside the
    // emit loop will see `true` and break before any file is emitted.
    cancel.cancel();

    let call = Call::ScanLibrary {
        uri: "mock-fs:///synthetic".into(),
        options: ScanOptions::new("/synthetic"),
        scan_session: ScanSessionId::new(),
        sink,
        root_done: None,
        cancel,
    };

    let response = kernel
        .dispatch_to_capability(
            CapabilityQuery::Sharded {
                kind: "discover".into(),
                key: "mock-fs".into(),
            },
            call,
        )
        .expect("dispatch should succeed even when pre-cancelled");

    // Receiver should be empty.
    let mut received = Vec::new();
    while let Ok(event) = rx.try_recv() {
        received.push(event);
    }
    assert!(
        received.is_empty(),
        "no files should be emitted when cancelled before first poll, got {} events",
        received.len()
    );

    match response {
        CallResponse::ScanLibrary(summary) => {
            assert_eq!(
                summary.file_count, 0,
                "summary should reflect zero files emitted after pre-cancel"
            );
            assert_eq!(summary.roots_scanned, 1);
        }
        other => panic!("expected CallResponse::ScanLibrary, got {other:?}"),
    }
}

#[test]
fn wasm_plugin_emits_root_walk_completed() {
    let Some(kernel) = bootstrap_kernel_with_fixture() else {
        return;
    };
    let (sink, _rx) = mpsc::channel::<FileDiscoveredEvent>(16);
    let (root_done, mut rx_root) = mpsc::channel::<RootWalkCompletedEvent>(8);
    let cancel = CancellationToken::new();

    let call = Call::ScanLibrary {
        uri: "mock-fs:///synthetic".into(),
        options: ScanOptions::new("/synthetic"),
        scan_session: ScanSessionId::new(),
        sink,
        root_done: Some(root_done),
        cancel,
    };

    let _response = kernel
        .dispatch_to_capability(
            CapabilityQuery::Sharded {
                kind: "discover".into(),
                key: "mock-fs".into(),
            },
            call,
        )
        .expect("dispatch should succeed");

    // The fixture emits exactly one RootWalkCompletedEvent per scan after the
    // file loop completes (see fixture's `run_scan_library`).
    let event = rx_root
        .try_recv()
        .expect("root_done sender should receive one RootWalkCompletedEvent");
    assert_eq!(event.root, std::path::PathBuf::from("/synthetic"));
    assert!(
        rx_root.try_recv().is_err(),
        "exactly one root event for a single-root mock scan",
    );
}

// --- check_call_payload_size unit tests --------------------------------------
//
// These do not depend on the WASM fixture; they exercise the rev-2 size-cap
// helper in `voom_kernel::loader::wasm`. Kept here (rather than in the
// loader's `#[cfg(test)]` module) so the integration test crate has explicit
// coverage of the public boundary helper.

#[test]
fn check_call_payload_size_accepts_within_limit() {
    let ok = check_call_payload_size("test-plugin", MAX_WASM_EVENT_PAYLOAD, "response");
    assert!(ok.is_ok(), "exactly at the limit should be accepted");

    let ok_small = check_call_payload_size("test-plugin", 0, "request");
    assert!(ok_small.is_ok());
}

#[test]
fn check_call_payload_size_rejects_oversized() {
    let err = check_call_payload_size("test-plugin", MAX_WASM_EVENT_PAYLOAD + 1, "response")
        .expect_err("over-limit payload must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("test-plugin"),
        "error should name the plugin, got: {msg}"
    );
    assert!(
        msg.contains("response"),
        "error should name the direction, got: {msg}"
    );
    assert!(
        msg.contains("exceeds"),
        "error should explain the limit was exceeded, got: {msg}"
    );
}
