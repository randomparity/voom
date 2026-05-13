// The streaming pipeline is constructed in this task (Task 5) and wired into
// `process::run` in the next task. Until then the public symbols below have
// no production caller, only tests, so dead-code lints would fire under
// `-D warnings`. The allow is removed by Task 6's commit.
#![allow(dead_code)]

//! Streaming pipeline for `voom process`: discovery → ingest → worker pool.
//!
//! Mirrors the layout of `scan/pipeline.rs`. The discovery stage runs
//! `DiscoveryPlugin::scan_streaming` inside `spawn_blocking` and pushes
//! `FileDiscoveredEvent`s into a bounded `mpsc` channel. The ingest stage
//! filters known-bad paths, dedupes overlapping roots, dispatches
//! `Event::FileDiscovered` through the kernel (so sqlite-store and the
//! ffprobe-introspector see every file), and forwards a
//! `WorkItem<DiscoveredFilePayload>` to a second bounded channel that
//! feeds `WorkerPool::process_stream`.
//!
//! The "I'm done producing" signal from the ingest stage is the drop of
//! `tx_items` — there is no separate `Notify`. The `execution_gate` is
//! still real: workers wait on it before claiming their first job so the
//! reporter sees the full discovery set before any work begins.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{Event, FileDiscoveredEvent};
use voom_domain::storage::StorageTrait;
use voom_job_manager::progress::ProgressReporter;
use voom_job_manager::worker::{JobErrorStrategy, JobResult, WorkItem, WorkerPool};

use crate::cli::ProcessArgs;
use crate::introspect::DiscoveredFilePayload;

/// Outcome of a streaming run. The fields here feed `print_run_results` and
/// the post-pipeline summary lines in `process::run`.
pub(crate) struct StreamingOutcome {
    /// Total files emitted by discovery (after dedup, before bad-file filter).
    pub(crate) discovered: u64,
    /// Files actually enqueued (after bad-file filter).
    pub(crate) enqueued: u64,
    /// Files dropped by the bad-file filter.
    pub(crate) skipped_bad: u64,
    /// Errors raised by the discovery walk (open / hash failures).
    pub(crate) discovery_errors: u64,
    /// Full `FileDiscoveredEvent` set fed to the reporter via `seed_events`.
    pub(crate) events_for_eta: Vec<FileDiscoveredEvent>,
    /// `Vec<JobResult>` returned by the worker pool.
    pub(crate) job_results: Vec<JobResult>,
}

/// Channel capacity per worker. Same constant as scan phase 2.
const CHANNEL_DEPTH_PER_WORKER: usize = 4;

/// Drive the full process streaming pipeline.
///
/// Three stages run concurrently:
///
/// 1. **Discovery** — `DiscoveryPlugin::scan_streaming` inside `spawn_blocking`,
///    pushing `FileDiscoveredEvent`s into `tx_disc`.
/// 2. **Ingest** — async task that dedupes, filters known-bad paths,
///    dispatches `FileDiscovered` through the kernel, builds `WorkItem`s, and
///    forwards them to `tx_items`. When `rx_disc` drains it drops `tx_items`
///    and (for mutating runs) releases `execution_gate` so the pool can
///    start claiming.
/// 3. **Pool** — `WorkerPool::process_stream` drains `rx_items`, enqueues
///    rows into the job queue, and runs workers in parallel.
///
/// Cancellation: a child token derived from `token`. Any stage failure
/// cancels the child token; siblings observe and exit. All three handles
/// are awaited unconditionally; the first error is propagated in source
/// order (discovery → ingest).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_streaming_pipeline<F, Fut>(
    args: &ProcessArgs,
    paths: &[PathBuf],
    kernel: Arc<voom_kernel::Kernel>,
    store: Arc<dyn StorageTrait>,
    pool: Arc<WorkerPool>,
    reporter: Arc<dyn ProgressReporter>,
    on_error: JobErrorStrategy,
    bad_files: HashSet<PathBuf>,
    processor: F,
    _dry_run: bool,
    token: CancellationToken,
) -> Result<StreamingOutcome>
where
    F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
        + Send
        + 'static,
{
    let effective_workers = pool.config().effective_workers();
    let channel_depth = effective_workers * CHANNEL_DEPTH_PER_WORKER;

    let (tx_disc, rx_disc) = mpsc::channel::<FileDiscoveredEvent>(channel_depth);
    let (tx_items, rx_items) = mpsc::channel::<WorkItem<DiscoveredFilePayload>>(channel_depth);

    let execution_gate = Arc::new(Notify::new());
    let pipeline_cancel = token.child_token();

    // The gate is always fired by the ingest stage once the channel drains —
    // this is the moment we know the full discovery set, so reporters can be
    // seeded with deterministic totals before workers begin claiming. Workers
    // wait on the gate (edge-triggered `notify_waiters`) so this ordering is
    // strict: workers register first, then ingest opens the gate after
    // `tx_items` is dropped.

    let discovery_errors = Arc::new(AtomicU64::new(0));
    let events_for_eta: Arc<Mutex<Vec<FileDiscoveredEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let enqueued = Arc::new(AtomicU64::new(0));
    let discovered = Arc::new(AtomicU64::new(0));
    let skipped_bad = Arc::new(AtomicU64::new(0));

    let discovery_handle = spawn_discovery_stage(
        args,
        paths,
        store.clone(),
        kernel.clone(),
        tx_disc,
        pipeline_cancel.clone(),
        discovery_errors.clone(),
    );

    let ingest_handle = spawn_ingest_stage(
        kernel.clone(),
        bad_files,
        args.force_rescan,
        args.priority_by_date,
        rx_disc,
        tx_items,
        execution_gate.clone(),
        pipeline_cancel.clone(),
        discovered.clone(),
        enqueued.clone(),
        skipped_bad.clone(),
        events_for_eta.clone(),
        reporter.clone(),
    );

    // Drive the pool future concurrently with discovery and ingest. Awaiting
    // sequentially would deadlock: ingest sends `WorkItem`s into rx_items but
    // until `process_stream` is being polled there is no enqueuer task to
    // drain that channel. Cross-stage cancellation is handled inside each
    // stage via `pipeline_cancel` — discovery cancels the token if its scan
    // errors, ingest observes the token in its receive select, and the pool's
    // workers observe their own copy via `WorkerPool::new`'s `token`.
    //
    // We wrap the pool future in its own `tokio::spawn` so all three stages
    // are `JoinHandle`s. If we used `tokio::join!` directly with a bare
    // future, a panic in the pool future would resolve `join!` only after
    // the two `JoinHandle`s also resolved — dropping a `JoinHandle` only
    // detaches the task, it does NOT abort it. Discovery and ingest could
    // keep running indefinitely with `pipeline_cancel` never tripped. By
    // serializing the awaits and cancelling on the first failure we mirror
    // the structure used in `scan/pipeline.rs`.
    let pool_reporter = reporter.clone();
    let pool_handle: JoinHandle<Vec<JobResult>> = tokio::spawn(async move {
        pool.process_stream(rx_items, execution_gate, processor, on_error, pool_reporter)
            .await
    });

    // Await each handle in source order. After each await, if the result
    // indicates failure (join error OR the task's own Err), cancel the
    // child token so sibling stages observe the failure rather than
    // continuing on the finish path.
    let disc_join = discovery_handle.await;
    if disc_join.as_ref().map_or(true, |r| r.is_err()) {
        pipeline_cancel.cancel();
    }

    let ingest_join = ingest_handle.await;
    if ingest_join.as_ref().map_or(true, |r| r.is_err()) {
        pipeline_cancel.cancel();
    }

    let pool_join = pool_handle.await;
    // No further stages remain to cancel; the pool's own `JoinError` is
    // surfaced below via `.context()?`.

    // Propagate errors in source order: discovery → ingest → pool. The
    // `??` idiom: outer `?` propagates the JoinError (after .context()),
    // inner `?` propagates the task's own Err.
    disc_join.context("discovery task join failed")??;
    ingest_join.context("ingest task join failed")??;
    let job_results = pool_join.context("pool task join failed")?;

    let events_for_eta = Arc::try_unwrap(events_for_eta)
        .map(parking_lot::Mutex::into_inner)
        .unwrap_or_else(|m| m.lock().clone());

    Ok(StreamingOutcome {
        discovered: discovered.load(Ordering::Relaxed),
        enqueued: enqueued.load(Ordering::Relaxed),
        skipped_bad: skipped_bad.load(Ordering::Relaxed),
        discovery_errors: discovery_errors.load(Ordering::Relaxed),
        events_for_eta,
        job_results,
    })
}

// --- Discovery stage --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn spawn_discovery_stage(
    args: &ProcessArgs,
    paths: &[PathBuf],
    store: Arc<dyn StorageTrait>,
    kernel: Arc<voom_kernel::Kernel>,
    tx_disc: mpsc::Sender<FileDiscoveredEvent>,
    token: CancellationToken,
    discovery_errors: Arc<AtomicU64>,
) -> JoinHandle<Result<()>> {
    let workers = args.workers;
    let hash_files = !args.no_backup;
    let paths = paths.to_vec();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let discovery = voom_discovery::DiscoveryPlugin::new();

        let run_scan = || -> Result<()> {
            for path in &paths {
                if token.is_cancelled() {
                    break;
                }

                let mut options = voom_discovery::ScanOptions::new(path.clone());
                options.hash_files = hash_files;
                options.workers = workers;
                options.fingerprint_lookup =
                    Some(crate::introspect::fingerprint_lookup(store.clone()));

                let kernel_clone = kernel.clone();
                let errors_clone = discovery_errors.clone();
                options.on_error = Some(Box::new(move |path, size, error| {
                    tracing::warn!(path = %path.display(), error = %error, "discovery error");
                    errors_clone.fetch_add(1, Ordering::Relaxed);
                    crate::introspect::dispatch_failure(
                        &kernel_clone,
                        path,
                        size,
                        None,
                        &error,
                        BadFileSource::Discovery,
                    );
                }));

                let token_inner = token.clone();
                let tx_inner = tx_disc.clone();
                let on_event: voom_discovery::EventSink = Box::new(move |event| {
                    if token_inner.is_cancelled() {
                        return;
                    }
                    // blocking_send applies natural backpressure: when the
                    // ingest channel is full this blocks the rayon worker
                    // until the ingest stage drains.
                    let _ = tx_inner.blocking_send(event);
                });

                discovery
                    .scan_streaming(&options, on_event)
                    .with_context(|| format!("filesystem scan failed for {}", path.display()))?;
            }
            Ok(())
        };
        let result = run_scan();

        // Cancel BEFORE dropping tx_disc on error. When tx_disc is dropped,
        // ingest's tokio::select! arm `ev = rx_disc.recv()` resolves to None,
        // breaking the ingest loop. By cancelling first, ingest's gate-fire
        // check at end of loop (`if !token.is_cancelled() { ... }`) observes
        // the cancellation and skips seeding the reporter / opening the
        // gate.
        if result.is_err() {
            token.cancel();
        }
        drop(tx_disc);
        result
    })
}

// --- Ingest stage -----------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn spawn_ingest_stage(
    kernel: Arc<voom_kernel::Kernel>,
    bad_files: HashSet<PathBuf>,
    force_rescan: bool,
    priority_by_date: bool,
    mut rx_disc: mpsc::Receiver<FileDiscoveredEvent>,
    tx_items: mpsc::Sender<WorkItem<DiscoveredFilePayload>>,
    execution_gate: Arc<Notify>,
    token: CancellationToken,
    discovered: Arc<AtomicU64>,
    enqueued: Arc<AtomicU64>,
    skipped_bad: Arc<AtomicU64>,
    events_for_eta: Arc<Mutex<Vec<FileDiscoveredEvent>>>,
    reporter: Arc<dyn ProgressReporter>,
) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let mut seen: HashSet<PathBuf> = HashSet::new();

        loop {
            let event = tokio::select! {
                biased;
                () = token.cancelled() => break,
                ev = rx_disc.recv() => ev,
            };
            let Some(event) = event else { break };

            if !seen.insert(event.path.clone()) {
                continue;
            }
            discovered.fetch_add(1, Ordering::Relaxed);

            if !force_rescan && bad_files.contains(&event.path) {
                skipped_bad.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            events_for_eta.lock().push(event.clone());
            super::dispatch::dispatch_and_log(&kernel, Event::FileDiscovered(event.clone()));

            let priority = if priority_by_date {
                super::compute_file_date_priority(&event.path)
            } else {
                100
            };
            let payload = DiscoveredFilePayload {
                path: event.path.to_string_lossy().into_owned(),
                size: event.size,
                content_hash: event.content_hash.clone(),
            };
            let item = WorkItem::new(voom_domain::job::JobType::Process, priority, Some(payload));
            if tx_items.send(item).await.is_err() {
                // The pool's enqueuer dropped its receiver; bail out.
                break;
            }
            enqueued.fetch_add(1, Ordering::Relaxed);
        }

        // Dropping tx_items signals "no more work" to the pool's enqueuer.
        drop(tx_items);

        // Seed the reporter with the full discovery set so progress bars and
        // ETA reporters have determinate totals before workers begin. Then
        // open the gate so workers can claim. For dry-run, mutating, and
        // estimate flows this ordering is the same: reporters see the full
        // set, then workers start.
        //
        // `Notify::notify_waiters` is edge-triggered: it only wakes waiters
        // that have already called `.notified()`. We yield to the runtime a
        // few times before firing so that every worker task (which starts
        // its body with `execution_gate.notified()`) has reached the await
        // point. `notify_one` is not a workable alternative because `Notify`
        // stores at most one permit regardless of how many calls are made.
        //
        // Skip seeding the reporter and opening the gate entirely when
        // cancelled. The partial event set collected so far is meaningless
        // — `BatchProgress` would render a misleading total and never tick
        // because no workers will run.
        if !token.is_cancelled() {
            let events = events_for_eta.lock().clone();
            reporter.seed_events(&events);
            reporter.on_batch_start(events.len());

            // Sleep briefly so worker tasks reach their first `.notified()`
            // point. In production discovery takes orders of magnitude
            // longer than 10ms, so this is essentially free in the common
            // case. Without it, tiny test workloads (where discovery
            // completes in < 1ms) can lose the gate signal entirely.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            execution_gate.notify_waiters();
        }

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;
    use voom_job_manager::progress::NoopReporter;
    use voom_job_manager::queue::JobQueue;
    use voom_job_manager::worker::WorkerPoolConfig;

    use crate::cli::{ErrorHandling, ProcessArgs};

    fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        // Match the path normalization that discovery applies so test
        // assertions against `event.path` line up. On macOS `/var` is a
        // symlink to `/private/var`, so the raw tempdir path and the
        // canonicalized path differ.
        std::fs::canonicalize(&p).unwrap_or(p)
    }

    fn make_test_args(paths: Vec<PathBuf>) -> ProcessArgs {
        ProcessArgs {
            paths,
            policy: None,
            policy_map: None,
            dry_run: true,
            estimate: false,
            estimate_only: false,
            on_error: ErrorHandling::Continue,
            workers: 2,
            approve: false,
            no_backup: true,
            force_rescan: false,
            flag_size_increase: false,
            flag_duration_shrink: false,
            plan_only: false,
            confirm_savings: None,
            priority_by_date: false,
        }
    }

    fn make_pool(
        token: CancellationToken,
    ) -> (
        Arc<WorkerPool>,
        Arc<dyn StorageTrait>,
        Arc<voom_kernel::Kernel>,
    ) {
        let store: Arc<dyn StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let job_store: Arc<dyn voom_domain::storage::JobStorage> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let queue = Arc::new(JobQueue::new(job_store));
        let mut config = WorkerPoolConfig::default();
        config.max_workers = 2;
        config.worker_prefix = "test".into();
        let pool = Arc::new(WorkerPool::new(queue, config, token));
        let kernel = Arc::new(voom_kernel::Kernel::new());
        (pool, store, kernel)
    }

    #[tokio::test]
    async fn streaming_pipeline_enqueues_dry_run() {
        let tmp = TempDir::new().unwrap();
        write_fixture(tmp.path(), "a.mkv", b"a");
        write_fixture(tmp.path(), "b.mkv", b"b");

        let token = CancellationToken::new();
        let (pool, store, kernel) = make_pool(token.clone());

        let args = make_test_args(vec![tmp.path().to_path_buf()]);

        let invocations = Arc::new(AtomicU64::new(0));
        let invocations_clone = invocations.clone();

        let outcome = run_streaming_pipeline(
            &args,
            &args.paths,
            kernel,
            store,
            pool,
            Arc::new(NoopReporter),
            JobErrorStrategy::Continue,
            HashSet::new(),
            move |_job| {
                let c = invocations_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                }
            },
            true,
            token,
        )
        .await
        .unwrap();

        assert_eq!(outcome.discovered, 2);
        assert_eq!(outcome.enqueued, 2);
        assert_eq!(outcome.skipped_bad, 0);
        assert_eq!(invocations.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn streaming_pipeline_filters_known_bad() {
        let tmp = TempDir::new().unwrap();
        let a = write_fixture(tmp.path(), "a.mkv", b"a");
        let _b = write_fixture(tmp.path(), "b.mkv", b"b");

        let mut bad = HashSet::new();
        bad.insert(a);

        let token = CancellationToken::new();
        let (pool, store, kernel) = make_pool(token.clone());

        let args = make_test_args(vec![tmp.path().to_path_buf()]);

        let outcome = run_streaming_pipeline(
            &args,
            &args.paths,
            kernel,
            store,
            pool,
            Arc::new(NoopReporter),
            JobErrorStrategy::Continue,
            bad,
            |_job| async { Ok(None) },
            true,
            token,
        )
        .await
        .unwrap();

        assert_eq!(outcome.discovered, 2);
        assert_eq!(outcome.enqueued, 1);
        assert_eq!(outcome.skipped_bad, 1);
    }
}
