use std::path::PathBuf;

use chrono::Utc;
use criterion::{criterion_group, criterion_main, Criterion};
use tempfile::TempDir;
use uuid::Uuid;
use voom_domain::media::MediaFile;
use voom_domain::storage::{FileFilters, FileStorage, VerificationStorage};
use voom_domain::verification::{
    VerificationFilters, VerificationMode, VerificationOutcome, VerificationRecord,
};
use voom_sqlite_store::store::SqliteStore;

fn seed_store(file_count: usize) -> (TempDir, SqliteStore) {
    let dir = TempDir::new().expect("temp dir");
    let store = SqliteStore::open(&dir.path().join("bench.sqlite")).expect("sqlite store");
    let now = Utc::now();

    for index in 0..file_count {
        let mut file = MediaFile::new(PathBuf::from(format!("/media/file-{index:05}.mkv")));
        file.size = index as u64;
        file.duration = index as f64;
        store.upsert_file(&file).expect("insert file");

        if index % 2 == 0 {
            let record = VerificationRecord::new(
                Uuid::new_v4(),
                file.id.to_string(),
                now,
                VerificationMode::Quick,
                VerificationOutcome::Ok,
                0,
                0,
                None,
                None,
            );
            store
                .insert_verification(&record)
                .expect("insert verification");
        }
    }

    (dir, store)
}

fn old_n_plus_one_due_files(
    store: &SqliteStore,
    cutoff: chrono::DateTime<Utc>,
) -> Vec<(String, PathBuf, Option<f64>)> {
    let files = store
        .list_files(&FileFilters::default())
        .expect("list files");
    let mut due = Vec::new();
    for file in files {
        let mut filters = VerificationFilters::default();
        filters.file_id = Some(file.id.to_string());
        filters.limit = Some(1);
        let latest = store
            .list_verifications(&filters)
            .expect("list verifications");
        let needs_verification = latest
            .first()
            .is_none_or(|record| record.verified_at < cutoff);
        if needs_verification {
            due.push((file.id.to_string(), file.path, Some(file.duration)));
        }
    }
    due
}

fn bench_due_file_listing(c: &mut Criterion) {
    let (_dir, store) = seed_store(10_000);
    let cutoff = Utc::now() - chrono::Duration::days(30);
    let expected = old_n_plus_one_due_files(&store, cutoff).len();
    assert_eq!(
        expected,
        store
            .list_files_due_for_verification(cutoff)
            .expect("list due files")
            .len()
    );

    let mut group = c.benchmark_group("verification_due_10k");
    group.sample_size(10);
    group.bench_function("old_n_plus_one", |b| {
        b.iter(|| {
            old_n_plus_one_due_files(std::hint::black_box(&store), std::hint::black_box(cutoff))
        });
    });
    group.bench_function("list_files_due_for_verification", |b| {
        b.iter(|| {
            store
                .list_files_due_for_verification(std::hint::black_box(cutoff))
                .expect("list due files")
                .len()
        });
    });
    group.finish();
}

criterion_group!(benches, bench_due_file_listing);
criterion_main!(benches);
