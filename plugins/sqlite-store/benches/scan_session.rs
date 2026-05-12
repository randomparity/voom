use std::path::PathBuf;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use voom_domain::storage::FileStorage;
use voom_domain::transition::DiscoveredFile;
use voom_sqlite_store::store::SqliteStore;

const SEED_FILES: usize = 100_000;
const SCAN_ROOT: &str = "/media";
const INGEST_OPS_PER_ITER: usize = 1000;

fn fresh_store() -> (TempDir, SqliteStore) {
    let dir = TempDir::new().expect("temp dir");
    let store =
        SqliteStore::open(&dir.path().join("scan_bench.sqlite")).expect("open sqlite store");
    (dir, store)
}

/// Seed `count` active files under `/media/...` via a completed scan session,
/// so the rows have `last_seen_session_id` set and `expected_hash` populated.
fn seed_active(store: &SqliteStore, count: usize) {
    let roots = vec![PathBuf::from(SCAN_ROOT)];
    let session = store.begin_scan_session(&roots).expect("begin");
    for i in 0..count {
        let df = DiscoveredFile::new(
            PathBuf::from(format!("{SCAN_ROOT}/file-{i:06}.mkv")),
            (i as u64) + 1,
            format!("hash-{i:06}"),
        );
        store.ingest_discovered_file(session, &df).expect("ingest");
    }
    store.finish_scan_session(session).expect("finish");
}

fn bench_ingest_new(c: &mut Criterion) {
    let (_dir, store) = fresh_store();
    seed_active(&store, SEED_FILES);
    let roots = vec![PathBuf::from(SCAN_ROOT)];
    let mut group = c.benchmark_group("ingest");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));
    group.bench_function("new_1000", |b| {
        b.iter_batched(
            || {
                let session = store.begin_scan_session(&roots).expect("begin");
                let base = uuid::Uuid::new_v4();
                (session, base)
            },
            |(session, base)| {
                for i in 0..INGEST_OPS_PER_ITER {
                    let df = DiscoveredFile::new(
                        PathBuf::from(format!("{SCAN_ROOT}/new-{base}-{i:06}.mkv")),
                        i as u64 + 1,
                        format!("new-{base}-{i:06}"),
                    );
                    store.ingest_discovered_file(session, &df).expect("ingest");
                }
                store.cancel_scan_session(session).expect("cancel");
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_ingest_unchanged(c: &mut Criterion) {
    let (_dir, store) = fresh_store();
    seed_active(&store, SEED_FILES);
    let roots = vec![PathBuf::from(SCAN_ROOT)];
    let mut group = c.benchmark_group("ingest");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));
    group.bench_function("unchanged_1000", |b| {
        b.iter_batched(
            || store.begin_scan_session(&roots).expect("begin"),
            |session| {
                for i in 0..INGEST_OPS_PER_ITER {
                    let df = DiscoveredFile::new(
                        PathBuf::from(format!("{SCAN_ROOT}/file-{i:06}.mkv")),
                        i as u64 + 1,
                        format!("hash-{i:06}"),
                    );
                    store.ingest_discovered_file(session, &df).expect("ingest");
                }
                store.cancel_scan_session(session).expect("cancel");
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_finish_with_moves_and_missing(c: &mut Criterion) {
    let mut group = c.benchmark_group("finish");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.bench_function("100k_seed_5k_unseen_500_moves", |b| {
        b.iter_batched(
            || {
                let (dir, store) = fresh_store();
                seed_active(&store, SEED_FILES);
                let roots = vec![PathBuf::from(SCAN_ROOT)];
                let session = store.begin_scan_session(&roots).expect("begin");
                // Re-ingest 95k as unchanged (skip last 5k for missing pass).
                for i in 0..(SEED_FILES - 5_000) {
                    let df = DiscoveredFile::new(
                        PathBuf::from(format!("{SCAN_ROOT}/file-{i:06}.mkv")),
                        i as u64 + 1,
                        format!("hash-{i:06}"),
                    );
                    store.ingest_discovered_file(session, &df).expect("ingest");
                }
                // Ingest 500 "moves": same hash as missing rows in the last 5k,
                // but at a new path under the same root.
                for i in 0..500 {
                    let src_index = SEED_FILES - 5_000 + i;
                    let df = DiscoveredFile::new(
                        PathBuf::from(format!("{SCAN_ROOT}/moved-{i:06}.mkv")),
                        src_index as u64 + 1,
                        format!("hash-{src_index:06}"),
                    );
                    store.ingest_discovered_file(session, &df).expect("ingest");
                }
                (dir, store, session)
            },
            |(_dir, store, session)| {
                store.finish_scan_session(session).expect("finish");
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_ingest_new,
    bench_ingest_unchanged,
    bench_finish_with_moves_and_missing,
);
criterion_main!(benches);
