//! Adversarial: if the `MutationSnapshotLoader` returns an error, the scan
//! must not proceed. Discovery must not silently fall through to "no
//! exclusions" because that re-introduces the rediscovery race.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use voom_discovery::{DiscoveryPlugin, ScanOptions, SessionMutationSnapshot};
use voom_domain::call::Call;
use voom_domain::errors::{StorageErrorKind, VoomError};
use voom_domain::events::{FileDiscoveredEvent, RootWalkCompletedEvent};
use voom_domain::transition::ScanSessionId;
use voom_kernel::Plugin;

#[test]
fn scanner_aborts_when_snapshot_loader_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.mkv"), b"x").unwrap();

    let mut opts = ScanOptions::new(tmp.path());
    opts.hash_files = false; // keep the test fast
    opts.session_mutations = Some(Arc::new(|| {
        Err(VoomError::Storage {
            kind: StorageErrorKind::Other,
            message: "injected".into(),
        })
    }));

    let (sink, mut rx) = mpsc::channel::<FileDiscoveredEvent>(16);
    let (root_done_tx, mut root_done_rx) = mpsc::channel::<RootWalkCompletedEvent>(1);
    let cancel = CancellationToken::new();

    let call = Call::ScanLibrary {
        uri: format!("file://{}", tmp.path().display()),
        options: opts,
        scan_session: ScanSessionId::new(),
        sink,
        root_done: Some(root_done_tx),
        cancel,
    };

    let plugin = DiscoveryPlugin::for_bootstrap();
    let res = plugin.on_call(&call);

    assert!(res.is_err(), "scan must fail when snapshot load fails");

    // No events must have been emitted from an aborted scan.
    let mut emitted = 0;
    while rx.try_recv().is_ok() {
        emitted += 1;
    }
    assert_eq!(emitted, 0, "no events must be emitted from an aborted scan");

    // root_done must NOT fire on failure.
    assert!(
        root_done_rx.try_recv().is_err(),
        "RootWalkCompletedEvent must NOT be emitted on scan failure",
    );
}

#[test]
fn snapshot_lookup_is_infallible() {
    // Sanity: a SessionMutationSnapshot exposes no fallible accessor.
    let s = SessionMutationSnapshot::new([std::path::PathBuf::from("/x")]);
    // This must compile: no Result, no Option.
    let _: bool = s.contains(std::path::Path::new("/x"));
}
