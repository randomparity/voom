//! Verify that `transitions_for_file` returns transitions across multiple paths.

use std::path::PathBuf;

use voom_domain::media::MediaFile;
use voom_domain::storage::{FileStorage, FileTransitionStorage};
use voom_domain::transition::{FileTransition, TransitionSource};
use voom_sqlite_store::store::SqliteStore;

#[test]
fn transitions_for_file_spans_multiple_paths() {
    let store = SqliteStore::in_memory().unwrap();

    // Insert the file so the file_id exists in the database.
    let file = MediaFile::new(PathBuf::from("/library/movies/old-name.mkv"));
    store.upsert_file(&file).unwrap();

    let file_id = store
        .file_by_path(std::path::Path::new("/library/movies/old-name.mkv"))
        .unwrap()
        .expect("file must exist after upsert")
        .id;

    // Record a transition under the original path.
    let t1 = FileTransition::new(
        file_id,
        PathBuf::from("/library/movies/old-name.mkv"),
        "hash1".into(),
        1000,
        TransitionSource::Voom,
    );
    store.record_transition(&t1).unwrap();

    // Record a second transition under the renamed path.
    // The file_transitions table records provenance and has no FK back to files,
    // so path and file_id are stored as-is.
    let t2 = FileTransition::new(
        file_id,
        PathBuf::from("/library/movies/new-name.mkv"),
        "hash2".into(),
        1000,
        TransitionSource::Voom,
    );
    store.record_transition(&t2).unwrap();

    // transitions_for_file should return both transitions regardless of path.
    let all = store.transitions_for_file(&file_id).unwrap();
    assert_eq!(all.len(), 2, "transitions_for_file must span both paths");

    // transitions_for_path should return only the matching path's transition.
    let old_only = store
        .transitions_for_path(std::path::Path::new("/library/movies/old-name.mkv"))
        .unwrap();
    assert_eq!(
        old_only.len(),
        1,
        "only one transition recorded at old path"
    );

    let new_only = store
        .transitions_for_path(std::path::Path::new("/library/movies/new-name.mkv"))
        .unwrap();
    assert_eq!(
        new_only.len(),
        1,
        "only one transition recorded at new path"
    );
}
