//! Streaming scan pipeline: discovery → ingest → bounded probe pool.

/// Default SQLite connection pool size. Mirrors
/// `voom_sqlite_store::store::SqliteStoreConfig::default().pool_size`.
/// Hard-coded here to avoid a circular dep on the store crate's config type.
const DEFAULT_SQLITE_POOL: usize = 8;

/// Slots we reserve out of the pool for non-probe work (one writer reacting
/// to events, one ad-hoc reader). Anything below this is unsafe — probes
/// would deadlock against the bus.
const RESERVED_POOL_SLOTS: usize = 2;

/// Pick the introspection worker count when `--probe-workers` is `0`.
///
/// Returns `min(num_cpus, pool_size - reserved)` with a floor of `1`.
#[must_use]
pub(crate) fn auto_probe_workers(num_cpus: usize, pool_size: usize) -> usize {
    let cap = pool_size.saturating_sub(RESERVED_POOL_SLOTS);
    num_cpus.min(cap).max(1)
}

/// Resolve the effective probe worker count from the CLI flag.
#[must_use]
pub(crate) fn resolve_probe_workers(flag: usize) -> usize {
    if flag == 0 {
        auto_probe_workers(num_cpus::get(), DEFAULT_SQLITE_POOL)
    } else {
        flag
    }
}

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{Event, FileDiscoveredEvent};
use voom_domain::storage::{StorageTrait, with_scan_session};
use voom_domain::transition::{DiscoveredFile, IngestDecision, ScanFinishOutcome};

use crate::cli::ScanArgs;
use crate::introspect;
use crate::progress::{DiscoveryProgress, ProbeProgress};

/// Output of the streaming pipeline. Fields mirror the counters that
/// `print_scan_summary`, `print_discovery_summary`, and `ScanComplete` need.
pub(crate) struct PipelineOutcome {
    pub files_discovered: u64,
    pub files_introspected: u64,
    /// Errors raised by the discovery / hashing stage (walk failures, open
    /// failures, etc.). Reported in the discovery summary line.
    pub discovery_errors: u64,
    /// Errors raised by the ffprobe stage. Reported in the final summary line.
    pub probe_errors: u64,
    pub moved: u32,
    pub external_changes: u32,
    pub missing: u32,
    /// Voom-temp files skipped during discovery. Reported in the discovery
    /// summary line (parity with the pre-streaming behaviour).
    pub orphans: u64,
    /// `(path, size, content_hash)` tuples for `--format` output.
    pub formatted: Vec<(PathBuf, u64, Option<String>)>,
}

impl PipelineOutcome {
    /// Sum of discovery + probe errors for the final scan-summary line.
    #[must_use]
    pub fn errors(&self) -> u64 {
        self.discovery_errors.saturating_add(self.probe_errors)
    }
}

/// Channel capacity multiplier — bounds peak in-memory queue depth to
/// roughly `probe_workers * CHANNEL_DEPTH_PER_WORKER`.
const CHANNEL_DEPTH_PER_WORKER: usize = 4;

/// Drive the full streaming pipeline. Returns the outcome counters used by
/// the caller for the final summary and `ScanComplete` dispatch.
///
/// `ffprobe_path` / `animation_mode` are passed in directly (rather than
/// pulled from `config` inside this fn) so tests can call it without
/// going through `app::bootstrap_kernel_with_store`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_streaming_pipeline(
    args: &ScanArgs,
    paths: &[PathBuf],
    hash_files: bool,
    store: Arc<dyn StorageTrait>,
    kernel: Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<String>,
    animation_mode: voom_ffprobe_introspector::parser::AnimationDetectionMode,
    discovery_progress: DiscoveryProgress,
    probe_progress: ProbeProgress,
    token: CancellationToken,
) -> Result<PipelineOutcome> {
    let probe_workers = resolve_probe_workers(args.probe_workers);
    let channel_depth = probe_workers * CHANNEL_DEPTH_PER_WORKER;

    let (tx_disc, rx_disc) = mpsc::channel::<FileDiscoveredEvent>(channel_depth);
    let (tx_probe, rx_probe) = mpsc::channel::<FileDiscoveredEvent>(channel_depth);

    // Shared counters. Discovery and probe errors stay separate so the
    // existing per-stage summary lines keep their meaning.
    let introspected = Arc::new(AtomicU64::new(0));
    let probe_errors = Arc::new(AtomicU64::new(0));
    let orphans = Arc::new(AtomicU64::new(0));

    // Pipeline-internal cancellation token. When any stage errors, we cancel
    // this token so sibling stages observe the failure and clean up promptly
    // (e.g., ingest runs cancel_scan_session instead of finish_scan_session).
    // It is a child of the external `token` so external cancellation also
    // propagates automatically.
    let pipeline_cancel = token.child_token();

    let discovery_handle = spawn_discovery_stage(
        args,
        paths,
        hash_files,
        store.clone(),
        kernel.clone(),
        discovery_progress.clone(),
        tx_disc,
        pipeline_cancel.clone(),
        orphans.clone(),
    );

    let ingest_handle = spawn_ingest_stage(
        store,
        kernel.clone(),
        paths.to_vec(),
        hash_files,
        rx_disc,
        tx_probe,
        probe_progress.clone(),
        pipeline_cancel.clone(),
    );

    let probe_handle = spawn_probe_stage(
        rx_probe,
        kernel,
        ffprobe_path,
        animation_mode,
        probe_workers,
        probe_progress.clone(),
        pipeline_cancel.clone(),
        introspected.clone(),
        probe_errors.clone(),
    );

    // Await all three stages unconditionally before propagating any error so
    // that:
    //   - the scan session always ends in a terminal state (Finished or
    //     Cancelled), never InProgress
    //   - progress bars always finish() before we return
    //   - probe tasks always drain
    //
    // After each await, if the result indicates failure (join error OR the
    // task's own Err), cancel the internal token so sibling stages observe
    // the failure and take the cancel path rather than the finish path.
    let disc_join = discovery_handle.await;
    if disc_join.as_ref().map_or(true, |r| r.is_err()) {
        pipeline_cancel.cancel();
    }

    let ingest_join = ingest_handle.await;
    if ingest_join.as_ref().map_or(true, |r| r.is_err()) {
        pipeline_cancel.cancel();
    }

    let probe_join = probe_handle.await;

    // Finish progress bars before propagating errors so they never hang.
    discovery_progress.finish();
    probe_progress.finish();

    // Propagate errors in source order: discovery → ingest → probe.
    // The `??` idiom: outer `?` propagates the JoinError (after .context()),
    // inner `?` propagates the task's own Err.
    let discovery_errors = disc_join.context("discovery task join failed")??;
    let (finish_outcome, formatted, files_discovered) =
        ingest_join.context("ingest task join failed")??;
    probe_join.context("probe task join failed")??;

    let mut moved = finish_outcome.moved_from_ingest;
    moved += finish_outcome.scan_finish.promoted_moves;

    Ok(PipelineOutcome {
        files_discovered,
        files_introspected: introspected.load(Ordering::Relaxed),
        discovery_errors,
        probe_errors: probe_errors.load(Ordering::Relaxed),
        moved,
        external_changes: finish_outcome.external_changes,
        missing: finish_outcome.scan_finish.missing,
        orphans: orphans.load(Ordering::Relaxed),
        formatted,
    })
}

/// Bundle of session-related counts returned by the ingest task.
struct IngestSummary {
    scan_finish: ScanFinishOutcome,
    moved_from_ingest: u32,
    external_changes: u32,
}

#[allow(clippy::too_many_arguments)]
fn spawn_discovery_stage(
    args: &ScanArgs,
    paths: &[PathBuf],
    hash_files: bool,
    store: Arc<dyn StorageTrait>,
    kernel: Arc<voom_kernel::Kernel>,
    progress: DiscoveryProgress,
    tx_disc: mpsc::Sender<FileDiscoveredEvent>,
    token: CancellationToken,
    orphans: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<Result<u64>> {
    let workers = args.workers;
    let recursive = args.recursive;
    let paths = paths.to_vec();
    tokio::task::spawn_blocking(move || -> Result<u64> {
        let discovery = voom_discovery::DiscoveryPlugin::new();
        let cumulative_discovered = Arc::new(AtomicU64::new(0));
        let processing_base = Arc::new(AtomicU64::new(0));
        // Local counter for discovery-time errors (open / hash / walk
        // failures). Returned via the JoinHandle's Ok value.
        let discovery_errors = Arc::new(AtomicU64::new(0));

        // Wrap the scan loop so we can inspect the result BEFORE dropping
        // tx_disc. On Err, we cancel the pipeline token first; that ensures
        // ingest's post-loop cancellation check observes the cancellation when
        // blocking_recv() returns None — closing the race where ingest would
        // otherwise call finish_scan_session and mark files Missing.
        let run_scan = || -> Result<u64> {
            for path in &paths {
                if token.is_cancelled() {
                    break;
                }
                progress.reset_to_spinner();
                let pre = cumulative_discovered.load(Ordering::Relaxed);

                let mut options = voom_discovery::ScanOptions::new(path.clone());
                options.recursive = recursive;
                options.hash_files = hash_files;
                options.workers = workers;
                options.fingerprint_lookup =
                    Some(crate::introspect::fingerprint_lookup(store.clone()));

                let progress_clone = progress.clone();
                let cum_disc = cumulative_discovered.clone();
                let proc_base = processing_base.clone();
                let orphans_progress = orphans.clone();
                let hash_for_progress = hash_files;
                options.on_progress = Some(Box::new(move |p| match p {
                    voom_discovery::ScanProgress::Discovered { count: _, path } => {
                        let n = cum_disc.fetch_add(1, Ordering::Relaxed) + 1;
                        let n = usize::try_from(n).unwrap_or(usize::MAX);
                        progress_clone.on_discovered(n, &path);
                    }
                    voom_discovery::ScanProgress::Processing {
                        current,
                        total,
                        path,
                    } => {
                        let base = proc_base.load(Ordering::Relaxed);
                        let base = usize::try_from(base).unwrap_or(usize::MAX);
                        let action = if hash_for_progress {
                            "Hashing"
                        } else {
                            "Processing"
                        };
                        progress_clone.on_processing(base + current, base + total, &path, action);
                    }
                    voom_discovery::ScanProgress::OrphanedTempFiles { count } => {
                        // Capture orphan-temp count so the discovery summary line
                        // can report it (parity with the pre-streaming behaviour).
                        orphans_progress.fetch_add(count as u64, Ordering::Relaxed);
                    }
                    voom_discovery::ScanProgress::HashReused { .. } => {}
                }));

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
                    // blocking_send blocks the current rayon worker until the
                    // ingest stage drains. This is the backpressure mechanism.
                    let _ = tx_inner.blocking_send(event);
                });

                discovery
                    .scan_streaming(&options, on_event)
                    .with_context(|| format!("filesystem scan failed for {}", path.display()))?;

                let dir_count = cumulative_discovered.load(Ordering::Relaxed) - pre;
                processing_base.fetch_add(dir_count, Ordering::Relaxed);
            }

            Ok(discovery_errors.load(Ordering::Relaxed))
        };
        let result = run_scan();

        // Cancel BEFORE dropping tx_disc so ingest's post-loop cancellation
        // check observes the token as cancelled when blocking_recv() returns
        // None. CancellationToken::cancel() carries Release semantics; the
        // channel close that follows establishes the happens-before edge to
        // ingest's blocking_recv returning None.
        if result.is_err() {
            token.cancel();
        }
        // Explicit drop makes the ordering visible at the call site.
        drop(tx_disc);
        result
    })
}

/// Ingest-stage output: session summary, formatted results, and discovered count.
type IngestOutput = (IngestSummary, Vec<(PathBuf, u64, Option<String>)>, u64);

#[allow(clippy::too_many_arguments)]
fn spawn_ingest_stage(
    store: Arc<dyn StorageTrait>,
    kernel: Arc<voom_kernel::Kernel>,
    paths: Vec<PathBuf>,
    hash_files: bool,
    mut rx_disc: mpsc::Receiver<FileDiscoveredEvent>,
    tx_probe: mpsc::Sender<FileDiscoveredEvent>,
    probe_progress: ProbeProgress,
    token: CancellationToken,
) -> tokio::task::JoinHandle<Result<IngestOutput>> {
    tokio::task::spawn_blocking(move || -> Result<_> {
        use std::collections::HashSet;

        let mut formatted: Vec<(PathBuf, u64, Option<String>)> = Vec::new();
        let mut files_discovered: u64 = 0;

        if !hash_files {
            // --no-hash path: no session, collect paths for missing-mark.
            let mut seen: HashSet<PathBuf> = HashSet::new();
            while let Some(event) = rx_disc.blocking_recv() {
                if token.is_cancelled() {
                    break;
                }
                if !seen.insert(event.path.clone()) {
                    continue;
                }
                files_discovered += 1;
                formatted.push((event.path.clone(), event.size, event.content_hash.clone()));
                kernel.dispatch(Event::FileDiscovered(event.clone()));
                probe_progress.add_pending(1);
                if tx_probe.blocking_send(event).is_err() {
                    break;
                }
            }
            drop(tx_probe);

            let discovered_paths: Vec<PathBuf> =
                formatted.iter().map(|(p, _, _)| p.clone()).collect();
            let missing = if token.is_cancelled() {
                0
            } else {
                store
                    .mark_missing_paths(&discovered_paths, &paths)
                    .context("path-only reconciliation failed")?
            };
            let scan_finish = ScanFinishOutcome::new(missing, 0);
            return Ok((
                IngestSummary {
                    scan_finish,
                    moved_from_ingest: 0,
                    external_changes: 0,
                },
                formatted,
                files_discovered,
            ));
        }

        // Hashed path: session-based ingest.
        let mut moved: u32 = 0;
        let mut external_changes: u32 = 0;
        let mut seen: HashSet<PathBuf> = HashSet::new();

        // Wrap with_scan_session in a named closure so we can inspect the
        // result BEFORE tx_probe's implicit drop at closure end. On Err,
        // cancel the pipeline token first so that the probe stage's post-recv
        // check observes the cancellation when rx_probe returns None.
        let run_ingest = || -> Result<ScanFinishOutcome> {
            with_scan_session(
                store.as_ref(),
                &paths,
                |session| -> Result<ScanFinishOutcome> {
                    while let Some(event) = rx_disc.blocking_recv() {
                        if token.is_cancelled() {
                            // Explicitly cancel the session to leave it in
                            // Cancelled state (no missing-file marking), then
                            // return an Ok sentinel so the outer with_scan_session
                            // doesn't double-cancel.
                            store
                                .cancel_scan_session(session)
                                .context("cancel_scan_session failed")?;
                            return Ok(ScanFinishOutcome::default());
                        }
                        if !seen.insert(event.path.clone()) {
                            continue;
                        }
                        files_discovered += 1;
                        formatted.push((
                            event.path.clone(),
                            event.size,
                            event.content_hash.clone(),
                        ));
                        kernel.dispatch(Event::FileDiscovered(event.clone()));

                        let Some(hash) = event.content_hash.clone() else {
                            // No hash → can't ingest into the session; forward to
                            // probe so the file still gets introspected.
                            probe_progress.add_pending(1);
                            if tx_probe.blocking_send(event).is_err() {
                                break;
                            }
                            continue;
                        };
                        let df = DiscoveredFile::new(event.path.clone(), event.size, hash);
                        let decision = store
                            .ingest_discovered_file(session, &df)
                            .context("ingest_discovered_file failed")?;
                        match &decision {
                            IngestDecision::Moved { .. } => moved += 1,
                            IngestDecision::ExternallyChanged { .. } => external_changes += 1,
                            _ => {}
                        }
                        if decision.needs_introspection_path(&event.path).is_some() {
                            probe_progress.add_pending(1);
                            if tx_probe.blocking_send(event).is_err() {
                                break;
                            }
                        }
                    }
                    drop(tx_probe);
                    // Re-check cancellation after the receive loop drains. If
                    // discovery cancelled the token BEFORE dropping tx_disc
                    // (the per-stage cancel-on-err fix), this check will see
                    // is_cancelled() == true and avoid finish_scan_session.
                    if token.is_cancelled() {
                        store
                            .cancel_scan_session(session)
                            .context("cancel_scan_session (post-loop) failed")?;
                        return Ok(ScanFinishOutcome::default());
                    }
                    let finish = store
                        .finish_scan_session(session)
                        .context("finish_scan_session failed")?;
                    Ok(finish)
                },
            )
        };
        let ingest_result = run_ingest();

        // Cancel BEFORE the closure's captured locals (including any remaining
        // tx_probe handle) are dropped, so the probe stage observes the
        // cancellation when rx_probe returns None.
        if ingest_result.is_err() {
            token.cancel();
        }
        let session_outcome = ingest_result?;

        Ok((
            IngestSummary {
                scan_finish: session_outcome,
                moved_from_ingest: moved,
                external_changes,
            },
            formatted,
            files_discovered,
        ))
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_probe_stage(
    mut rx_probe: mpsc::Receiver<FileDiscoveredEvent>,
    kernel: Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<String>,
    animation_mode: voom_ffprobe_introspector::parser::AnimationDetectionMode,
    probe_workers: usize,
    progress: ProbeProgress,
    token: CancellationToken,
    introspected: Arc<AtomicU64>,
    probe_errors: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let sem = Arc::new(Semaphore::new(probe_workers));
        let mut set: JoinSet<()> = JoinSet::new();

        while let Some(event) = rx_probe.recv().await {
            if token.is_cancelled() {
                break;
            }
            let permit = sem
                .clone()
                .acquire_owned()
                .await
                .context("probe semaphore closed unexpectedly")?;
            // Drain any tasks that completed since the last poll. This is what
            // keeps the JoinSet bounded by `probe_workers` rather than by the
            // total number of files scanned. The Semaphore prevents
            // oversubscription; the opportunistic drain prevents JoinHandle
            // accumulation.
            while let Some(joined) = set.try_join_next() {
                handle_probe_join_result(&joined, &probe_errors, &progress);
            }
            let kernel_inner = kernel.clone();
            let introspected = introspected.clone();
            let probe_errors = probe_errors.clone();
            let progress_inner = progress.clone();
            let ffprobe_path = ffprobe_path.clone();
            let token_inner = token.clone();
            set.spawn(async move {
                let _permit = permit;
                if token_inner.is_cancelled() {
                    progress_inner.inc();
                    return;
                }
                let path_for_progress = event.path.clone();
                progress_inner.on_file(0, &path_for_progress);
                match introspect::introspect_file(
                    event.path.clone(),
                    event.size,
                    event.content_hash.clone(),
                    &kernel_inner,
                    ffprobe_path.as_deref(),
                    animation_mode,
                )
                .await
                {
                    Ok(_) => {
                        introspected.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %event.path.display(),
                            error = %e,
                            "introspection failed"
                        );
                        probe_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                progress_inner.inc();
            });
        }

        // Drain remaining workers. A panicked probe task would otherwise be
        // silently lost — we'd miss both the error count AND the progress
        // bar increment, leaving the total stuck below the discovered count.
        while let Some(joined) = set.join_next().await {
            handle_probe_join_result(&joined, &probe_errors, &progress);
        }
        Ok(())
    })
}

/// Account for a probe-task `JoinResult`. Non-panicked completions
/// already updated counters and the progress bar inside the spawned task;
/// only panics need attention here. Cancellation is treated as a warning for
/// accounting purposes (rare — JoinSet tasks are not normally cancelled
/// individually).
fn handle_probe_join_result(
    joined: &Result<(), tokio::task::JoinError>,
    probe_errors: &Arc<AtomicU64>,
    progress: &crate::progress::ProbeProgress,
) {
    if let Err(e) = joined {
        if e.is_panic() {
            tracing::error!(error = %e, "probe task panicked");
            probe_errors.fetch_add(1, Ordering::Relaxed);
            progress.inc();
        } else {
            tracing::warn!(error = %e, "probe task cancelled");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_probe_workers_caps_at_pool_minus_reserved() {
        assert_eq!(auto_probe_workers(16, 8), 6);
    }

    #[test]
    fn auto_probe_workers_uses_cpus_when_pool_is_generous() {
        assert_eq!(auto_probe_workers(4, 32), 4);
    }

    #[test]
    fn auto_probe_workers_floor_is_one() {
        assert_eq!(auto_probe_workers(0, 0), 1);
        assert_eq!(auto_probe_workers(1, 2), 1);
        assert_eq!(auto_probe_workers(1, 1), 1);
    }

    #[test]
    fn resolve_probe_workers_explicit_flag_wins() {
        assert_eq!(resolve_probe_workers(3), 3);
        assert_eq!(resolve_probe_workers(1), 1);
    }
}
