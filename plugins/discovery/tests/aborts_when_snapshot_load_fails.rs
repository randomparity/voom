//! Adversarial: if the `MutationSnapshotLoader` returns an error, the scan
//! must not proceed. Discovery must not silently fall through to "no
//! exclusions" because that re-introduces the rediscovery race.

use std::sync::{Arc, Mutex};

use voom_discovery::{ScanOptions, SessionMutationSnapshot, scan_directory_streaming};
use voom_domain::errors::{StorageErrorKind, VoomError};

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

    let emitted = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let emitted = emitted.clone();
        Box::new(move |fd| emitted.lock().unwrap().push(fd))
    };
    let res = scan_directory_streaming(&opts, sink);

    assert!(res.is_err(), "scan must fail when snapshot load fails");
    assert!(
        emitted.lock().unwrap().is_empty(),
        "no events must be emitted from an aborted scan"
    );
}

#[test]
fn snapshot_lookup_is_infallible() {
    // Sanity: a SessionMutationSnapshot exposes no fallible accessor.
    let s = SessionMutationSnapshot::new([std::path::PathBuf::from("/x")]);
    // This must compile: no Result, no Option.
    let _: bool = s.contains(std::path::Path::new("/x"));
}
