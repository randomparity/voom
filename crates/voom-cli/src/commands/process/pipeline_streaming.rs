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
use console::style;
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{Event, FileDiscoveredEvent, RootWalkCompletedEvent};
use voom_domain::storage::StorageTrait;
use voom_domain::transition::ScanSessionId;
use voom_job_manager::progress::ProgressReporter;
use voom_job_manager::worker::{JobErrorStrategy, JobResult, WorkItem, WorkerPool};

use super::root_gate::{HoldingBuffer, RootGate, root_for_path};

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
    /// Surfaced on the outcome so Task 7 integration tests can assert on the
    /// exact event stream observed by the reporter without re-running discovery.
    #[allow(dead_code)]
    pub(crate) events_for_eta: Vec<FileDiscoveredEvent>,
    /// `Vec<JobResult>` returned by the worker pool. Production callers don't
    /// inspect this (results are aggregated through the kernel event bus and
    /// `RunCounters`), but Task 7 integration tests verify per-job outcomes
    /// directly off this field.
    #[allow(dead_code)]
    pub(crate) job_results: Vec<JobResult>,
    /// Deduplicated, full discovered path set (every path the ingest stage
    /// saw before any bad-file filtering). Used by the no-hash success path
    /// in `process::run` to call `mark_missing_paths` for path-only
    /// reconciliation — mirroring `voom scan`'s unhashed branch.
    pub(crate) discovered_paths: Vec<PathBuf>,
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
    quiet: bool,
    plan_only: bool,
    token: CancellationToken,
    scan_session: ScanSessionId,
) -> Result<StreamingOutcome>
where
    F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
        + Send
        + 'static,
{
    let execute_during_discovery = args.execute_during_discovery;

    // Log activation when the flag is on.
    if execute_during_discovery && !args.dry_run && !plan_only && !args.estimate {
        tracing::warn!(
            target: "voom.streaming.mode",
            flag = "execute_during_discovery",
            value = true,
            "executing mutating plans during active discovery; per-root gates active"
        );
        if paths.len() == 1 {
            tracing::info!(
                "single-root invocation: --execute-during-discovery has no effect; \
                 execution will still wait for the root's walk to complete"
            );
        }
    }

    let effective_workers = pool.config().effective_workers();
    let channel_depth = effective_workers * CHANNEL_DEPTH_PER_WORKER;

    let (tx_disc, rx_disc) = mpsc::channel::<FileDiscoveredEvent>(channel_depth);
    let (tx_items, rx_items) = mpsc::channel::<WorkItem<DiscoveredFilePayload>>(channel_depth);

    // Channel for discovery stage to signal root completion to the dispatcher.
    // Only created when the flag is on; when off, no channel exists so no
    // spurious send-on-dropped-receiver warnings can fire.
    let (tx_root_done, rx_root_done): (
        Option<mpsc::Sender<RootWalkCompletedEvent>>,
        Option<mpsc::Receiver<RootWalkCompletedEvent>>,
    ) = if execute_during_discovery {
        let (tx, rx) = mpsc::channel::<RootWalkCompletedEvent>(paths.len().max(1));
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

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
    let discovered_paths: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let enqueued = Arc::new(AtomicU64::new(0));
    let discovered = Arc::new(AtomicU64::new(0));
    let skipped_bad = Arc::new(AtomicU64::new(0));

    let discovery_handle = spawn_discovery_stage(
        args,
        paths,
        store.clone(),
        kernel.clone(),
        tx_disc,
        tx_root_done,
        pipeline_cancel.clone(),
        discovery_errors.clone(),
        scan_session,
        execute_during_discovery,
    );

    let ingest_handle = spawn_ingest_stage(
        kernel.clone(),
        store.clone(),
        scan_session,
        bad_files,
        args.force_rescan,
        args.priority_by_date,
        paths.to_vec(),
        rx_disc,
        rx_root_done,
        tx_items,
        execution_gate.clone(),
        pipeline_cancel.clone(),
        discovered.clone(),
        enqueued.clone(),
        skipped_bad.clone(),
        events_for_eta.clone(),
        discovered_paths.clone(),
        reporter.clone(),
        quiet,
        plan_only,
        execute_during_discovery,
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
    let pool_for_task = pool.clone();
    let pool_handle: JoinHandle<Vec<JobResult>> = tokio::spawn(async move {
        pool_for_task
            .process_stream(rx_items, execution_gate, processor, on_error, pool_reporter)
            .await
    });

    // Await each handle in source order. After each await, if the result
    // indicates failure (join error OR the task's own Err), cancel the
    // child token so sibling stages observe the failure rather than
    // continuing on the finish path.
    let disc_join = discovery_handle.await;
    if disc_join.as_ref().map_or(true, |r| r.is_err()) {
        pipeline_cancel.cancel();
        // Also cancel the worker pool so it exits its gate wait promptly.
        // pipeline_cancel is a CHILD of `token`, but the pool was constructed
        // with `token` — child cancellation alone won't reach the pool's
        // internal token, leaving workers hung on execution_gate.notified().
        pool.cancel();
    }

    let ingest_join = ingest_handle.await;
    if ingest_join.as_ref().map_or(true, |r| r.is_err()) {
        pipeline_cancel.cancel();
        pool.cancel();
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
    let discovered_paths = Arc::try_unwrap(discovered_paths)
        .map(parking_lot::Mutex::into_inner)
        .unwrap_or_else(|m| m.lock().clone());

    Ok(StreamingOutcome {
        discovered: discovered.load(Ordering::Relaxed),
        enqueued: enqueued.load(Ordering::Relaxed),
        skipped_bad: skipped_bad.load(Ordering::Relaxed),
        discovery_errors: discovery_errors.load(Ordering::Relaxed),
        events_for_eta,
        job_results,
        discovered_paths,
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
    tx_root_done: Option<mpsc::Sender<RootWalkCompletedEvent>>,
    token: CancellationToken,
    discovery_errors: Arc<AtomicU64>,
    scan_session: ScanSessionId,
    with_gate: bool,
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

                // When gate mode is on, load a fresh snapshot per root so that
                // mutations recorded during prior roots' execution are excluded
                // from later walks. Snapshot load failures abort the scan
                // (fail-closed).
                if with_gate {
                    let snapshot_store = store.clone();
                    options.session_mutations = Some(std::sync::Arc::new(move || {
                        load_snapshot_via_storage_trait(&snapshot_store, scan_session)
                    }));
                }

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

                let started = std::time::Instant::now();
                discovery
                    .scan_streaming(&options, on_event)
                    .with_context(|| format!("filesystem scan failed for {}", path.display()))?;

                // Emit RootWalkCompleted so the dispatcher can open the gate
                // and drain the holding buffer for this root. Only send when
                // the channel exists (i.e. execute_during_discovery is on).
                if let Some(tx) = tx_root_done.as_ref() {
                    let elapsed_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let evt = RootWalkCompletedEvent::new(path.clone(), scan_session, elapsed_ms);
                    if let Err(e) = tx.blocking_send(evt) {
                        tracing::warn!(error = %e, "tx_root_done dropped; ingest dispatcher gone");
                    }
                }
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
        drop(tx_root_done);
        result
    })
}

/// Build a `SessionMutationSnapshot` from the storage trait's
/// `voom_mutations_for_session` method. Paths are normalized via
/// `voom_discovery::normalize_path` so they match what the scanner emits.
fn load_snapshot_via_storage_trait(
    store: &Arc<dyn StorageTrait>,
    session: ScanSessionId,
) -> voom_domain::errors::Result<voom_discovery::SessionMutationSnapshot> {
    let mutations = store.voom_mutations_for_session(session)?;
    let paths: HashSet<PathBuf> = mutations
        .into_iter()
        .map(|m| voom_discovery::normalize_path(&m.path))
        .collect();
    Ok(voom_discovery::SessionMutationSnapshot::new(paths))
}

// --- Ingest stage -----------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn spawn_ingest_stage(
    kernel: Arc<voom_kernel::Kernel>,
    store: Arc<dyn StorageTrait>,
    scan_session: ScanSessionId,
    bad_files: HashSet<PathBuf>,
    force_rescan: bool,
    priority_by_date: bool,
    roots: Vec<PathBuf>,
    mut rx_disc: mpsc::Receiver<FileDiscoveredEvent>,
    rx_root_done: Option<mpsc::Receiver<RootWalkCompletedEvent>>,
    tx_items: mpsc::Sender<WorkItem<DiscoveredFilePayload>>,
    execution_gate: Arc<Notify>,
    token: CancellationToken,
    discovered: Arc<AtomicU64>,
    enqueued: Arc<AtomicU64>,
    skipped_bad: Arc<AtomicU64>,
    events_for_eta: Arc<Mutex<Vec<FileDiscoveredEvent>>>,
    discovered_paths: Arc<Mutex<Vec<PathBuf>>>,
    reporter: Arc<dyn ProgressReporter>,
    quiet: bool,
    plan_only: bool,
    execute_during_discovery: bool,
) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let gate = RootGate::new(&roots);
        let holding = Arc::new(HoldingBuffer::<WorkItem<DiscoveredFilePayload>>::new());

        // Spawn dispatcher task when gate mode is on. It listens for
        // RootWalkCompleted events, opens the gate, and drains the holding
        // buffer into tx_items. When the flag is off, rx_root_done is None
        // so no channel exists and no spurious warnings can fire.
        let dispatcher_handle: Option<JoinHandle<()>> = if let Some(rx) = rx_root_done {
            let gate_d = gate.clone();
            let holding_d = holding.clone();
            let tx_items_d = tx_items.clone();
            let enqueued_d = enqueued.clone();
            let token_d = token.clone();
            let kernel_d = kernel.clone();
            Some(tokio::spawn(async move {
                let mut rx = rx;
                loop {
                    let msg = tokio::select! {
                        biased;
                        () = token_d.cancelled() => break,
                        msg = rx.recv() => msg,
                    };
                    let Some(evt) = msg else { break };
                    gate_d.open(&evt.root);
                    // Dispatch to the event bus so plugins (sqlite-store) can
                    // log the event and tests can observe it via the timeline.
                    super::dispatch::dispatch_and_log(
                        &kernel_d,
                        Event::RootWalkCompleted(evt.clone()),
                    );
                    let drained = holding_d.drain_root(&evt.root);
                    for item in drained {
                        if tx_items_d.send(item).await.is_err() {
                            return;
                        }
                        enqueued_d.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }))
        } else {
            None
        };

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
            // Track every deduped, observed path. This is the input set for
            // `mark_missing_paths` on the no-hash (`--no-backup`) success
            // path. Collected BEFORE the bad-file filter because a known-bad
            // file is still present on disk — excluding it would falsely
            // mark it missing on the next path-only reconciliation.
            discovered_paths.lock().push(event.path.clone());

            // Session registration MUST happen BEFORE the bad-file filter:
            // a known-bad file may still be physically present on disk and
            // have an active row in `files`. If we skipped registration
            // here, `finish_scan_session` would later see the row as not
            // registered this session and mark it Missing — false-positive
            // missing of a present file. (Codex second-pass review, May
            // 2026.)
            let needs_reintrospect = if let Some(hash) = event.content_hash.clone() {
                let df = voom_domain::transition::DiscoveredFile::new(
                    event.path.clone(),
                    event.size,
                    hash,
                );
                let store_for_blocking = store.clone();
                let session_for_blocking = scan_session;
                let join_result = tokio::task::spawn_blocking(move || {
                    store_for_blocking.ingest_discovered_file(session_for_blocking, &df)
                })
                .await;

                // Ingest failure must be FAIL-CLOSED: cancel the pipeline
                // (so discovery stops walking and the pool stops claiming),
                // cancel the scan session (so no later code path can call
                // finish_scan_session and falsely mark files missing), then
                // propagate the error. Without this, discovery keeps
                // emitting events until the walk completes naturally and —
                // under `--execute-during-discovery` — workers can keep
                // mutating files after registration has already failed.
                let decision = match join_result {
                    Ok(Ok(decision)) => decision,
                    Ok(Err(e)) => {
                        token.cancel();
                        if let Err(cancel_err) = store.cancel_scan_session(scan_session) {
                            tracing::warn!(
                                error = %cancel_err,
                                "cancel_scan_session failed after ingest error"
                            );
                        }
                        return Err(anyhow::Error::new(e).context("ingest_discovered_file failed"));
                    }
                    Err(e) => {
                        token.cancel();
                        if let Err(cancel_err) = store.cancel_scan_session(scan_session) {
                            tracing::warn!(
                                error = %cancel_err,
                                "cancel_scan_session failed after ingest join error"
                            );
                        }
                        return Err(anyhow::anyhow!("ingest_discovered_file join failed: {e}"));
                    }
                };
                // `Moved` and `ExternallyChanged` indicate the existing row
                // is stale; the worker must bypass the matches_discovery
                // cache and re-introspect. `Unchanged` / `Duplicate` are
                // safe cache hits. `New` requires introspection by
                // definition; surface that too so the bit is meaningful
                // regardless of which decision came back.
                decision.needs_introspection_path(&event.path).is_some()
            } else {
                // No hash → no session registration possible. Treat as
                // needing introspection (the worker will introspect; the
                // success path will do path-only reconciliation).
                true
            };

            // Bad-file filter runs AFTER session registration so the row
            // counts as "seen this session" for `finish_scan_session`.
            // Skipping the work item still saves the worker pool the load,
            // but the file isn't falsely missing.
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
                needs_reintrospect,
            };
            let item = WorkItem::new(voom_domain::job::JobType::Process, priority, Some(payload));

            if execute_during_discovery {
                let item_path =
                    PathBuf::from(item.payload.as_ref().map_or("", |p| p.path.as_str()));
                if let Some(root) = root_for_path(&roots, &item_path) {
                    if gate.is_open(root) {
                        if tx_items.send(item).await.is_err() {
                            break;
                        }
                        enqueued.fetch_add(1, Ordering::Relaxed);
                    } else {
                        holding.push(root, item);
                        // Not yet eligible; dispatcher will bump enqueued when draining.
                    }
                } else {
                    // No matching root — pass through.
                    if tx_items.send(item).await.is_err() {
                        break;
                    }
                    enqueued.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                if tx_items.send(item).await.is_err() {
                    // The pool's enqueuer dropped its receiver; bail out.
                    break;
                }
                enqueued.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Dropping tx_items signals "no more work" to the pool's enqueuer
        // (after the dispatcher is also done).
        if let Some(handle) = dispatcher_handle {
            // Wait for the dispatcher to drain all buffered items before
            // dropping tx_items. If cancelled, the holding buffer will still
            // have items — just discard them (no SQL rows were created).
            let _ = handle.await;
            let _ = holding.drain_all(); // discard any leftover held items
        }
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

            // Print the "Found N media files." line BEFORE workers start so the
            // user sees discovery's final count between the spinner and the
            // per-file progress bar, matching the pre-streaming UX.
            if !plan_only && !quiet && !token.is_cancelled() {
                eprintln!("Found {} media files.", style(events.len()).bold());
            }

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
            execute_during_discovery: false,
            format: crate::cli::OutputFormat::Table,
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
            true,
            token,
            voom_domain::transition::ScanSessionId::new(),
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
            true,
            token,
            voom_domain::transition::ScanSessionId::new(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.discovered, 2);
        assert_eq!(outcome.enqueued, 1);
        assert_eq!(outcome.skipped_bad, 1);
    }

    #[tokio::test]
    async fn streaming_enqueues_during_discovery() {
        let tmp = TempDir::new().unwrap();
        for i in 0..20 {
            write_fixture(tmp.path(), &format!("f{i:02}.mkv"), b"x");
        }

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
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    Ok(None)
                }
            },
            true,
            true,
            token,
            voom_domain::transition::ScanSessionId::new(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.discovered, 20);
        assert_eq!(outcome.enqueued, 20);
        assert!(invocations.load(Ordering::SeqCst) >= 20);
    }

    #[tokio::test]
    async fn cancellation_during_discovery_leaves_no_pending_jobs() {
        let tmp = TempDir::new().unwrap();
        for i in 0..50 {
            write_fixture(tmp.path(), &format!("f{i:02}.mkv"), b"x");
        }
        let token = CancellationToken::new();
        let (pool, store, kernel) = make_pool(token.clone());
        let args = make_test_args(vec![tmp.path().to_path_buf()]);

        let token_for_cancel = token.clone();
        let processor = move |_job: voom_domain::job::Job| {
            let token_for_cancel = token_for_cancel.clone();
            async move {
                token_for_cancel.cancel();
                Err::<Option<serde_json::Value>, String>("cancelled mid-run".into())
            }
        };

        let pool_clone = pool.clone();
        let _outcome = run_streaming_pipeline(
            &args,
            &args.paths,
            kernel,
            store,
            pool,
            Arc::new(NoopReporter),
            JobErrorStrategy::Continue,
            HashSet::new(),
            processor,
            true,
            true,
            token,
            voom_domain::transition::ScanSessionId::new(),
        )
        .await
        .unwrap();

        let mut filters = voom_domain::storage::JobFilters::default();
        filters.status = Some(voom_domain::job::JobStatus::Pending);
        let pending = pool_clone.queue().list_jobs(&filters).unwrap();
        assert!(
            pending.is_empty(),
            "no jobs should remain in Pending after cancel, got {}",
            pending.len()
        );
    }

    #[tokio::test]
    async fn fail_fast_with_pending_streamed_jobs() {
        let tmp = TempDir::new().unwrap();
        for i in 0..10 {
            write_fixture(tmp.path(), &format!("f{i:02}.mkv"), b"x");
        }
        let token = CancellationToken::new();
        let (pool, store, kernel) = make_pool(token.clone());
        let args = make_test_args(vec![tmp.path().to_path_buf()]);

        let invocations = Arc::new(AtomicU64::new(0));
        let invocations_clone = invocations.clone();
        let processor = move |_job: voom_domain::job::Job| {
            let c = invocations_clone.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n == 2 {
                    Err::<Option<serde_json::Value>, String>("boom".into())
                } else {
                    Ok(None)
                }
            }
        };

        let outcome = run_streaming_pipeline(
            &args,
            &args.paths,
            kernel,
            store,
            pool,
            Arc::new(NoopReporter),
            JobErrorStrategy::Fail,
            HashSet::new(),
            processor,
            true,
            true,
            token,
            voom_domain::transition::ScanSessionId::new(),
        )
        .await
        .unwrap();

        let failures = outcome
            .job_results
            .iter()
            .filter(|r| !r.is_success())
            .count();
        let successes = outcome
            .job_results
            .iter()
            .filter(|r| r.is_success())
            .count();
        assert!(failures >= 1, "expected at least one failure, got 0");
        assert!(
            successes <= 4,
            "fail-fast should stop short of all 10 files, got {successes} successes"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discovery_error_does_not_hang_pool() {
        // Discovery is given a non-existent root so scan_streaming returns
        // Err on the first path. The pipeline must propagate that error
        // promptly and the pool must not be left waiting on the gate.
        let token = CancellationToken::new();
        let (pool, store, kernel) = make_pool(token.clone());

        let missing = std::path::PathBuf::from("/does/not/exist/for/streaming/test");
        let args = make_test_args(vec![missing.clone()]);

        // Race with a 5-second hard timeout — without the fix this hangs
        // forever.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_streaming_pipeline(
                &args,
                &args.paths,
                kernel,
                store,
                pool.clone(),
                Arc::new(NoopReporter),
                JobErrorStrategy::Continue,
                HashSet::new(),
                |_job| async { Ok(None) },
                true,
                true,
                token,
                voom_domain::transition::ScanSessionId::new(),
            ),
        )
        .await;

        match result {
            Ok(Err(_)) => { /* pipeline returned an error — good */ }
            Ok(Ok(_)) => panic!("pipeline returned Ok despite discovery error"),
            Err(_) => panic!("pipeline hung instead of propagating the discovery error"),
        }
    }

    #[tokio::test]
    async fn bounded_buffering_under_large_input() {
        let tmp = TempDir::new().unwrap();
        for i in 0..200 {
            write_fixture(tmp.path(), &format!("f{i:03}.mkv"), b"x");
        }
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
            HashSet::new(),
            |_job| async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                Ok(None)
            },
            true,
            true,
            token,
            voom_domain::transition::ScanSessionId::new(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.enqueued, 200);
        let successes = outcome
            .job_results
            .iter()
            .filter(|r| r.is_success())
            .count();
        assert_eq!(successes, 200, "expected all 200 jobs to succeed");
    }
}
