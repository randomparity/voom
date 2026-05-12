use std::path::PathBuf;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use voom_domain::storage::FileStorage;
use voom_domain::transition::{DiscoveredFile, IngestDecision};
use voom_sqlite_store::store::SqliteStore;

const SEED_FILES: usize = 100_000;
const SCAN_ROOT: &str = "/media";
const INGEST_OPS_PER_ITER: usize = 1000;

/// Build a template database with `file_count` active files under `/media/...`,
/// then checkpoint its WAL into the main file so the resulting `.sqlite` file
/// is self-contained and ready to be cloned per-iteration. Returns the
/// `TempDir` that owns the template file's directory — keep it alive for as
/// long as the bench needs the template.
fn build_template_db(file_count: usize) -> TempDir {
    let dir = TempDir::new().expect("template dir");
    let template_path = dir.path().join("template.sqlite");

    {
        let store = SqliteStore::open(&template_path).expect("open template");
        let roots = vec![PathBuf::from(SCAN_ROOT)];
        let session = store.begin_scan_session(&roots).expect("begin");
        for i in 0..file_count {
            let df = DiscoveredFile::new(
                PathBuf::from(format!("{SCAN_ROOT}/file-{i:06}.mkv")),
                (i as u64) + 1,
                format!("hash-{i:06}"),
            );
            store.ingest_discovered_file(session, &df).expect("ingest");
        }
        store.finish_scan_session(session).expect("finish");
        // SqliteStore (and its connection pool) drops here.
    }

    // Fold the WAL into the main DB file so a single-file copy is sufficient.
    // PRAGMA wal_checkpoint(TRUNCATE) writes all WAL frames into the main DB
    // and truncates the WAL file to zero bytes.
    {
        let conn =
            rusqlite::Connection::open(&template_path).expect("open template for checkpoint");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint template");
    }

    dir
}

/// Clone the template DB into a fresh per-iteration `TempDir` and open a
/// `SqliteStore` on it. The returned `TempDir` must outlive the `SqliteStore`
/// — Criterion's `iter_batched` keeps the setup return value alive through
/// the routine, so returning both as a tuple is sufficient.
fn fresh_clone_from(template_dir: &TempDir) -> (TempDir, SqliteStore) {
    let iter_dir = TempDir::new().expect("iter dir");
    let dest = iter_dir.path().join("iter.sqlite");
    std::fs::copy(template_dir.path().join("template.sqlite"), &dest).expect("copy template");
    let store = SqliteStore::open(&dest).expect("open iter store");
    (iter_dir, store)
}

fn bench_ingest_new(c: &mut Criterion) {
    let template = build_template_db(SEED_FILES);
    let roots = vec![PathBuf::from(SCAN_ROOT)];
    let mut group = c.benchmark_group("ingest");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));
    group.bench_function("new_1000", |b| {
        b.iter_batched(
            || {
                let (dir, store) = fresh_clone_from(&template);
                let session = store.begin_scan_session(&roots).expect("begin");
                let base = uuid::Uuid::new_v4();
                (dir, store, session, base)
            },
            |(_dir, store, session, base)| {
                for i in 0..INGEST_OPS_PER_ITER {
                    let df = DiscoveredFile::new(
                        PathBuf::from(format!("{SCAN_ROOT}/new-{base}-{i:06}.mkv")),
                        i as u64 + 1,
                        format!("new-{base}-{i:06}"),
                    );
                    let decision = store.ingest_discovered_file(session, &df).expect("ingest");
                    assert!(
                        matches!(decision, IngestDecision::New { .. }),
                        "expected IngestDecision::New, got a different variant",
                    );
                }
                // No cancel — the iteration's TempDir is dropped at the end of
                // the routine, taking the cloned DB with it.
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_ingest_unchanged(c: &mut Criterion) {
    let template = build_template_db(SEED_FILES);
    let roots = vec![PathBuf::from(SCAN_ROOT)];
    let mut group = c.benchmark_group("ingest");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));
    group.bench_function("unchanged_1000", |b| {
        b.iter_batched(
            || {
                let (dir, store) = fresh_clone_from(&template);
                let session = store.begin_scan_session(&roots).expect("begin");
                (dir, store, session)
            },
            |(_dir, store, session)| {
                for i in 0..INGEST_OPS_PER_ITER {
                    let df = DiscoveredFile::new(
                        PathBuf::from(format!("{SCAN_ROOT}/file-{i:06}.mkv")),
                        i as u64 + 1,
                        format!("hash-{i:06}"),
                    );
                    let decision = store.ingest_discovered_file(session, &df).expect("ingest");
                    assert!(
                        matches!(decision, IngestDecision::Unchanged { .. }),
                        "expected IngestDecision::Unchanged, got a different variant",
                    );
                }
                // No cancel — clone is discarded with the TempDir.
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_finish_with_moves_and_missing(c: &mut Criterion) {
    let template = build_template_db(SEED_FILES);
    let roots = vec![PathBuf::from(SCAN_ROOT)];
    let mut group = c.benchmark_group("finish");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.bench_function("100k_seed_5k_unseen_500_moves", |b| {
        b.iter_batched(
            || {
                let (dir, store) = fresh_clone_from(&template);
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
                let outcome = store.finish_scan_session(session).expect("finish");
                assert_eq!(
                    outcome.missing, 4500,
                    "expected 4500 missing, got {}",
                    outcome.missing,
                );
                assert_eq!(
                    outcome.promoted_moves, 500,
                    "expected 500 promoted_moves, got {}",
                    outcome.promoted_moves,
                );
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
