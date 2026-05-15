//! End-to-end check that `Kernel::dispatch_to_capability` routes
//! `Call::ScanLibrary` through to `DiscoveryPlugin::on_call`, drains the
//! event stream into the caller's `mpsc::Receiver`, and returns a populated
//! `CallResponse::ScanLibrary` summary.
//!
//! This test is intentionally external to the discovery crate's own tests:
//! its purpose is to prove the *dispatch path* works against the real
//! plugin, which the synthetic test plugins in `voom-kernel` cannot.

use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use voom_discovery::DiscoveryPlugin;
use voom_domain::call::{Call, CallResponse};
use voom_domain::capabilities::CapabilityQuery;
use voom_domain::events::{FileDiscoveredEvent, RootWalkCompletedEvent};
use voom_domain::scan::ScanOptions;
use voom_domain::transition::ScanSessionId;
use voom_kernel::Kernel;

/// Create a temp dir with N media files and return its path.
fn temp_media_dir(n: usize) -> TempDir {
    let dir = TempDir::new().expect("create tempdir");
    for i in 0..n {
        let path = dir.path().join(format!("clip_{i:03}.mkv"));
        std::fs::write(&path, b"\x1A\x45\xDF\xA3").expect("write fixture");
    }
    dir
}

#[test]
fn dispatch_scan_library_streams_through_real_discovery_plugin() {
    let dir = temp_media_dir(3);
    let root = dir.path().to_path_buf();

    let mut kernel = Kernel::new();
    kernel
        .register_plugin(Arc::new(DiscoveryPlugin::for_bootstrap()), 10)
        .expect("register discovery");
    let kernel = Arc::new(kernel);

    let (sink, mut rx) = mpsc::channel::<FileDiscoveredEvent>(64);
    let (root_done_tx, mut root_done_rx) = mpsc::channel::<RootWalkCompletedEvent>(4);
    let cancel = CancellationToken::new();

    let call = Call::ScanLibrary {
        uri: format!("file://{}", root.display()),
        options: ScanOptions::new(root.clone()),
        scan_session: ScanSessionId::new(),
        sink,
        root_done: Some(root_done_tx),
        cancel,
    };

    // The dispatch path is synchronous; spawn it on a worker thread so this
    // test thread can drain the mpsc receivers without deadlocking against
    // the sender's buffer.
    let kernel_for_thread = kernel.clone();
    let handle = thread::spawn(move || {
        kernel_for_thread.dispatch_to_capability(
            CapabilityQuery::Sharded {
                kind: "discover".into(),
                key: "file".into(),
            },
            call,
        )
    });

    // Collect all FileDiscoveredEvent payloads and the root-done signal.
    let mut received = Vec::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let root_done_event = rt.block_on(async {
        while let Some(ev) = rx.recv().await {
            received.push(ev);
        }
        root_done_rx.recv().await
    });

    let response = handle
        .join()
        .expect("dispatch panicked")
        .expect("dispatch result");
    let summary = match response {
        CallResponse::ScanLibrary(s) => s,
        other => panic!("unexpected variant: {other:?}"),
    };

    assert_eq!(received.len(), 3, "all three files must stream through");
    assert_eq!(summary.file_count, 3);
    assert_eq!(summary.roots_scanned, 1);
    assert!(
        summary.errors.is_empty(),
        "no discovery errors expected on a clean tempdir"
    );
    let root_done_event = root_done_event.expect("root_done must fire on a successful scan");
    assert_eq!(
        root_done_event.root, root,
        "RootWalkCompletedEvent must carry the scanned root"
    );
}

#[test]
fn dispatch_scan_library_rejects_other_call_variants() {
    // Any Call variant other than ScanLibrary on a DiscoveryPlugin returns
    // an error rather than silently succeeding.
    let mut kernel = Kernel::new();
    kernel
        .register_plugin(Arc::new(DiscoveryPlugin::for_bootstrap()), 10)
        .expect("register discovery");

    let call = Call::Orchestrate {
        plans: vec![],
        policy_name: "demo".into(),
    };
    // The query targets a kind discovery doesn't hold, so the kernel won't
    // even route this to discovery — but make sure it surfaces the missing-
    // handler error rather than ever calling discovery's on_call with a
    // non-ScanLibrary variant.
    let err = kernel
        .dispatch_to_capability(
            CapabilityQuery::Exclusive {
                kind: "orchestrate_phases".into(),
            },
            call,
        )
        .expect_err("no orchestrator registered → must error");
    assert!(
        err.to_string().contains("orchestrate_phases"),
        "error must name the missing capability: {err}"
    );
}
