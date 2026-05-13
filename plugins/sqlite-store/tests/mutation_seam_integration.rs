//! Verifies that the public `ScanSessionMutationStorage` trait is reachable
//! from outside the crate and that the full session lifecycle correctly
//! skips VOOM-touched paths at finish.
//!
//! This is a structural test for the public seam — if `voom-ffmpeg-executor`
//! and `voom-mkvtoolnix-executor` can call `record_voom_mutation` exactly
//! the way this file does, the production wiring will work.

use std::path::PathBuf;

use voom_domain::media::MediaFile;
use voom_domain::scan_session_mutations::{MutationKind, VoomOriginatedMutation};
use voom_domain::storage::FileStorage;
use voom_sqlite_store::ScanSessionMutationStorage;
use voom_sqlite_store::store::SqliteStore;

fn store() -> SqliteStore {
    SqliteStore::in_memory().expect("open in-memory store")
}

fn active_file(path: &str) -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from(path));
    file.content_hash = Some("abc123".to_string());
    file.expected_hash = Some("abc123".to_string());
    file
}

#[test]
fn rename_through_public_seam_protects_both_paths_at_finish() {
    let store = store();
    let roots = vec![PathBuf::from("/m")];

    // Seed the source as active.
    store.upsert_file(&active_file("/m/foo.mkv")).unwrap();

    // Begin a session via the public scan-session API.
    let session = store.begin_scan_session(&roots).unwrap();

    // Record a rename via the public mutation trait. This is the call site
    // the future executor will use, verbatim.
    store
        .record_voom_mutation(&VoomOriginatedMutation::new(
            session,
            PathBuf::from("/m/foo.mp4"),
            Some(PathBuf::from("/m/foo.mkv")),
            MutationKind::Rename,
        ))
        .unwrap();

    // Finish: source must NOT be marked missing.
    let finish = store.finish_scan_session(session).unwrap();
    assert_eq!(
        finish.missing, 0,
        "VOOM rename through the public seam must protect both paths"
    );
}

#[test]
fn reentrant_destination_through_public_seam_keeps_source_protected() {
    let store = store();
    let roots = vec![PathBuf::from("/m")];

    store.upsert_file(&active_file("/m/A.mkv")).unwrap();
    let session = store.begin_scan_session(&roots).unwrap();

    // Rename A -> B.
    store
        .record_voom_mutation(&VoomOriginatedMutation::new(
            session,
            "/m/B.mp4".into(),
            Some("/m/A.mkv".into()),
            MutationKind::Rename,
        ))
        .unwrap();
    // Then overwrite B again in the same session.
    store
        .record_voom_mutation(&VoomOriginatedMutation::new(
            session,
            "/m/B.mp4".into(),
            None,
            MutationKind::Overwrite,
        ))
        .unwrap();

    let finish = store.finish_scan_session(session).unwrap();
    assert_eq!(
        finish.missing, 0,
        "rename source must survive a same-destination follow-up write"
    );
}
