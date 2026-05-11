use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::app;
use crate::cli::ScanArgs;
use crate::config;
use crate::output;
use crate::paths::resolve_paths;
use crate::progress::{DiscoveryProgress, ProbeProgress};
use anyhow::{Context, Result};
use console::style;
use indicatif::HumanDuration;
use tokio_util::sync::CancellationToken;
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{Event, FileDiscoveredEvent, ScanCompleteEvent};
use voom_domain::storage::StorageTrait;
use voom_domain::verification::{VerificationMode, VerificationOutcome, VerificationRecord};
use voom_verifier::VerifierConfig;

use crate::commands::verify::{QuickVerifyTarget, run_quick_pass};

/// Run the scan command.
///
/// Discovery and introspection are driven directly for deterministic progress
/// reporting, but all events are also published through the kernel's event bus
/// so that subscribers (sqlite-store, WASM plugins) receive them.
pub async fn run(args: ScanArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    let config = config::load_config()?;
    let app::BootstrapResult { kernel, store, .. } = app::bootstrap_kernel_with_store(&config)?;
    let kernel = Arc::new(kernel);

    let primary_result: Result<()> = async {
        let paths = resolve_paths(&args.paths)?;
        let hash_files = !args.no_hash;
        let start = Instant::now();

        if !quiet {
            let path_list: Vec<_> = paths
                .iter()
                .map(|p| style(p.display()).cyan().to_string())
                .collect();
            eprintln!("{} {}", style("Scanning").bold(), path_list.join(", "));
        }

        let (mut all_events, orphans, disc_errors) =
            run_discovery(&args, &paths, hash_files, quiet, &kernel, store.clone())?;

        // Deduplicate by path: overlapping scan roots can produce the same path
        // multiple times. Doing this here keeps the dispatch, summary,
        // introspection, verification, and JSON output paths all
        // single-counted. The session API's IngestDecision::Duplicate is now
        // defense in depth for the hash path.
        {
            let mut seen = HashSet::new();
            all_events.retain(|e| seen.insert(e.path.clone()));
        }

        if all_events.is_empty() {
            handle_empty_scan(&*store, &paths, hash_files, orphans, quiet, args.format)?;
            return Ok(());
        }

        if !quiet {
            print_discovery_summary(
                all_events.len(),
                start.elapsed(),
                hash_files,
                orphans,
                disc_errors,
            );
        }

        let reconcile_outcome =
            drive_session_ingest(&*store, &all_events, &paths, hash_files, quiet, &kernel)?;
        let needs_introspection = reconcile_outcome.needs_introspection;
        let needs_introspection_refs: Vec<&FileDiscoveredEvent> =
            needs_introspection.iter().collect();
        let (introspected, errors) = run_introspection(
            &needs_introspection_refs,
            &kernel,
            config.ffprobe_path(),
            config.animation_detection_mode(),
            &token,
            quiet,
        )
        .await;

        print_scan_summary(
            &all_events,
            introspected,
            errors,
            start.elapsed(),
            token.is_cancelled(),
            quiet,
            args.format,
        );

        if token.is_cancelled() {
            return Ok(());
        }

        purge_stale_records(&*store, config.pruning.retention_days, quiet);

        // Optional quick-verification pass after introspection. Discovery itself
        // is not blocked by this — verifications run as a separate fan-out only
        // once the discovery + introspection phases have completed.
        let verifier_cfg = read_verifier_config(&config);
        let should_verify = args.verify || verifier_cfg.verify_on_scan;
        if should_verify {
            run_verify_pass(
                &store,
                &verifier_cfg,
                &all_events,
                args.workers,
                quiet,
                &token,
            );
        }

        // Protected from cancelled runs by the early return above (line 91).
        // `ScanComplete` carries both files_discovered and files_introspected and
        // is the single lifecycle event for a full scan. We deliberately do NOT
        // dispatch `IntrospectComplete` here — that event is reserved for
        // standalone re-introspection runs (see commands/process/mod.rs). Emitting
        // both would cause subscribers like the report plugin to capture two
        // back-to-back snapshots (see issue #153).
        kernel.dispatch(Event::ScanComplete(ScanCompleteEvent::new(
            all_events.len() as u64,
            introspected,
        )));

        if let Some(format) = args.format {
            output::format_scan_results(&format_results(&all_events), format);
        }

        Ok(())
    }
    .await;

    crate::retention::maybe_run_after_cli(store, &config.retention, Some(kernel));

    primary_result
}

/// Outcome of driving the session-based ingest flow.
struct ReconcileOutcome {
    needs_introspection: Vec<FileDiscoveredEvent>,
}

/// Drive the scan-session lifecycle (begin → ingest each → finish) over
/// discovered events, dispatching `FileDiscovered` on the bus and collecting
/// the set of events that need introspection. On error mid-loop, cancels the
/// session so no file is marked missing.
fn drive_session_ingest(
    store: &dyn voom_domain::storage::StorageTrait,
    events: &[FileDiscoveredEvent],
    paths: &[PathBuf],
    hash_files: bool,
    quiet: bool,
    kernel: &Arc<voom_kernel::Kernel>,
) -> anyhow::Result<ReconcileOutcome> {
    use voom_domain::transition::{DiscoveredFile, IngestDecision};

    let mut needs_introspection: Vec<FileDiscoveredEvent> = Vec::new();

    if !hash_files {
        // --no-hash path: dispatch events + path-only missing-mark
        let discovered: Vec<PathBuf> = events.iter().map(|e| e.path.clone()).collect();
        let missing = store
            .mark_missing_paths(&discovered, paths)
            .context("path-only reconciliation failed")?;
        if !quiet && missing > 0 {
            print_missing_count(missing);
        }
        for event in events {
            kernel.dispatch(Event::FileDiscovered(event.clone()));
        }
        needs_introspection = events.to_vec();
        return Ok(ReconcileOutcome {
            needs_introspection,
        });
    }

    let session = store
        .begin_scan_session(paths)
        .context("failed to begin scan session")?;

    let mut moved = 0u32;
    let mut external_changes = 0u32;

    // Wrap ingest + finish so any error routes through the cancel path below
    // and the session is never left in_progress.
    let combined_result: anyhow::Result<voom_domain::transition::ScanFinishOutcome> = (|| {
        for event in events {
            // Dispatch before ingest so bus subscribers see every event even if
            // ingest later errors and aborts the loop.
            kernel.dispatch(Event::FileDiscovered(event.clone()));

            let Some(hash) = event.content_hash.clone() else {
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
                needs_introspection.push(event.clone());
            }
        }
        let finish = store
            .finish_scan_session(session)
            .context("finish_scan_session failed")?;
        Ok(finish)
    })();

    let finish = match combined_result {
        Ok(f) => f,
        Err(e) => {
            if let Err(cancel_err) = store.cancel_scan_session(session) {
                tracing::warn!(
                    session = %session,
                    ingest_error = %e,
                    cancel_error = %cancel_err,
                    "failed to cancel scan session after ingest error",
                );
            }
            return Err(e);
        }
    };

    // Promoted moves were counted as New during ingestion; correct both totals.
    moved += finish.promoted_moves;
    if !quiet {
        if finish.missing > 0 {
            print_missing_count(finish.missing);
        }
        if moved > 0 {
            eprintln!("  {} {} files moved/renamed", style("Moved").dim(), moved,);
        }
        if external_changes > 0 {
            eprintln!(
                "  {} {} files changed externally",
                style("Changed").dim(),
                external_changes,
            );
        }
    }
    Ok(ReconcileOutcome {
        needs_introspection,
    })
}

/// Run filesystem discovery across all paths, returning events and counters.
fn run_discovery(
    args: &ScanArgs,
    paths: &[PathBuf],
    hash_files: bool,
    quiet: bool,
    kernel: &Arc<voom_kernel::Kernel>,
    store: Arc<dyn voom_domain::storage::StorageTrait>,
) -> Result<(Vec<FileDiscoveredEvent>, u64, u64)> {
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let progress = if quiet {
        DiscoveryProgress::hidden()
    } else {
        DiscoveryProgress::new()
    };
    let orphan_count = Arc::new(AtomicU64::new(0));
    let discovery_errors = Arc::new(AtomicU64::new(0));
    let cumulative_discovered = Arc::new(AtomicU64::new(0));
    let processing_base = Arc::new(AtomicU64::new(0));
    let mut all_events = Vec::new();

    for path in paths {
        progress.reset_to_spinner();

        let progress_clone = progress.clone();
        let orphan_clone = orphan_count.clone();
        let errors_clone = discovery_errors.clone();
        let kernel_clone = kernel.clone();
        let cum_disc = cumulative_discovered.clone();
        let proc_base = processing_base.clone();
        let pre_scan = cumulative_discovered.load(Ordering::Relaxed);

        let mut options = voom_discovery::ScanOptions::new(path.clone());
        options.recursive = args.recursive;
        options.hash_files = hash_files;
        options.workers = args.workers;
        options.fingerprint_lookup = Some(crate::introspect::fingerprint_lookup(store.clone()));
        options.on_progress = Some(Box::new(move |progress| match progress {
            voom_discovery::ScanProgress::Discovered { count: _, path } => {
                let cumulative = cum_disc.fetch_add(1, Ordering::Relaxed) + 1;
                let cumulative = usize::try_from(cumulative).unwrap_or(usize::MAX);
                progress_clone.on_discovered(cumulative, &path);
            }
            voom_discovery::ScanProgress::Processing {
                current,
                total,
                path,
            } => {
                let base = proc_base.load(Ordering::Relaxed);
                let base = usize::try_from(base).unwrap_or(usize::MAX);
                let action = if hash_files { "Hashing" } else { "Processing" };
                progress_clone.on_processing(base + current, base + total, &path, action);
            }
            voom_discovery::ScanProgress::OrphanedTempFiles { count } => {
                orphan_clone.fetch_add(count as u64, Ordering::Relaxed);
            }
            voom_discovery::ScanProgress::HashReused { .. } => {}
        }));
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

        let events = discovery.scan(&options).context("filesystem scan failed")?;

        let dir_discovered = cumulative_discovered.load(Ordering::Relaxed) - pre_scan;
        processing_base.fetch_add(dir_discovered, Ordering::Relaxed);
        all_events.extend(events);
    }

    progress.finish();

    let orphans = orphan_count.load(Ordering::Relaxed);
    let errors = discovery_errors.load(Ordering::Relaxed);
    Ok((all_events, orphans, errors))
}

/// Handle the case where discovery found no files. Runs reconciliation
/// to mark previously known files as missing, then prints output.
fn handle_empty_scan(
    store: &dyn voom_domain::storage::StorageTrait,
    paths: &[PathBuf],
    hash_files: bool,
    orphans: u64,
    quiet: bool,
    format: Option<crate::cli::OutputFormat>,
) -> Result<()> {
    if hash_files {
        let result = store.reconcile_discovered_files(&[], paths)?;
        if !quiet && result.missing > 0 {
            print_missing_count(result.missing);
        }
    } else {
        let missing = store.mark_missing_paths(&[], paths)?;
        if !quiet && missing > 0 {
            print_missing_count(missing);
        }
    }

    if !quiet {
        if orphans > 0 {
            eprintln!(
                "{} ({} orphaned temp {} skipped)",
                style("No media files found.").yellow(),
                orphans,
                if orphans == 1 { "file" } else { "files" },
            );
        } else {
            eprintln!("{}", style("No media files found.").yellow());
        }
    }

    if matches!(format, Some(crate::cli::OutputFormat::Json)) {
        println!("[]");
    }
    Ok(())
}

/// Print the discovery/hashing summary line.
fn print_discovery_summary(
    file_count: usize,
    elapsed: Duration,
    hash_files: bool,
    orphans: u64,
    disc_errors: u64,
) {
    let orphan_suffix = if orphans > 0 {
        format!(
            " ({} orphaned temp {} skipped)",
            orphans,
            if orphans == 1 { "file" } else { "files" }
        )
    } else {
        String::new()
    };
    let error_suffix = if disc_errors > 0 {
        format!(
            ", {} discovery {}",
            disc_errors,
            if disc_errors == 1 { "error" } else { "errors" }
        )
    } else {
        String::new()
    };

    if hash_files {
        let elapsed_str = if elapsed.as_millis() < 1000 {
            format!("{}ms", elapsed.as_millis())
        } else {
            format!("{}", HumanDuration(elapsed))
        };
        eprintln!(
            "  {} {} files, hashed in {}{}{}",
            style("Discovered").dim(),
            file_count,
            elapsed_str,
            orphan_suffix,
            error_suffix,
        );
    } else {
        eprintln!(
            "  {} {} files (hashing skipped){}{}",
            style("Discovered").dim(),
            file_count,
            orphan_suffix,
            error_suffix,
        );
    }
}

/// Run ffprobe introspection on files. Returns (introspected, errors) counts.
async fn run_introspection(
    events: &[&FileDiscoveredEvent],
    kernel: &Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<&str>,
    animation_mode: voom_ffprobe_introspector::parser::AnimationDetectionMode,
    token: &CancellationToken,
    quiet: bool,
) -> (u64, u64) {
    let probe = if quiet {
        ProbeProgress::hidden(events.len())
    } else {
        ProbeProgress::new(events.len())
    };
    let mut introspected = 0u64;
    let mut errors = 0u64;

    for (i, event) in events.iter().enumerate() {
        if token.is_cancelled() {
            break;
        }
        probe.on_file(i + 1, &event.path);

        match crate::introspect::introspect_file(
            event.path.clone(),
            event.size,
            event.content_hash.clone(),
            kernel,
            ffprobe_path,
            animation_mode,
        )
        .await
        {
            Ok(_file) => introspected += 1,
            Err(e) => {
                tracing::warn!(
                    path = %event.path.display(),
                    error = %e,
                    "introspection failed"
                );
                errors += 1;
            }
        }
        probe.inc();
    }

    probe.finish();
    (introspected, errors)
}

/// Print the final scan summary (completion or interruption).
fn print_scan_summary(
    events: &[FileDiscoveredEvent],
    introspected: u64,
    errors: u64,
    elapsed: Duration,
    cancelled: bool,
    quiet: bool,
    format: Option<crate::cli::OutputFormat>,
) {
    let total = events.len() as u64;
    let error_suffix = if errors > 0 {
        format!(", {} {}", errors, style("errors").red())
    } else {
        String::new()
    };

    if cancelled {
        if !quiet {
            eprintln!(
                "\n{} {} files discovered, {}/{} introspected{} ({})",
                style("Interrupted.").bold().yellow(),
                events.len(),
                introspected,
                total,
                error_suffix,
                HumanDuration(elapsed),
            );
        }
        if let Some(format) = format {
            output::format_scan_results(&format_results(events), format);
        }
        return;
    }

    if !quiet {
        eprintln!(
            "\n{} {} files discovered, {} introspected{} ({})",
            style("Done.").bold().green(),
            events.len(),
            introspected,
            error_suffix,
            HumanDuration(elapsed),
        );
    }
}

/// Purge stale missing records based on retention config.
fn purge_stale_records(
    store: &dyn voom_domain::storage::StorageTrait,
    retention_days: u32,
    quiet: bool,
) {
    if retention_days == 0 {
        return;
    }
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days));
    match store.purge_missing(cutoff) {
        Ok(n) if n > 0 && !quiet => {
            eprintln!(
                "  {} {} stale records (missing >{} days)",
                style("Purged").dim(),
                n,
                retention_days
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge failed"),
    }
}

fn print_missing_count(count: u32) {
    eprintln!(
        "  {} {} files no longer on disk",
        style("Missing").dim(),
        count
    );
}

fn format_results(events: &[FileDiscoveredEvent]) -> Vec<(PathBuf, u64, Option<String>)> {
    events
        .iter()
        .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
        .collect()
}

/// Read `[plugin.verifier]` from the loaded `AppConfig`, falling back to defaults.
fn read_verifier_config(cfg: &crate::config::AppConfig) -> VerifierConfig {
    cfg.plugin
        .get("verifier")
        .and_then(|t| serde_json::to_value(t).ok())
        .and_then(|v| serde_json::from_value::<VerifierConfig>(v).ok())
        .unwrap_or_default()
}

/// Build the freshness cutoff: skip files whose latest quick verification is
/// newer than this timestamp. `0` means always re-verify.
fn freshness_cutoff(days: u64) -> Option<chrono::DateTime<chrono::Utc>> {
    if days == 0 {
        return None;
    }
    let dur = chrono::Duration::days(i64::try_from(days).unwrap_or(i64::MAX));
    Some(chrono::Utc::now() - dur)
}

/// Build verify targets from the discovery events: look up each file via
/// `lookup`, drop any whose `latest_quick` timestamp is at or after the
/// freshness cutoff. The two-closure signature keeps this unit-testable
/// without a full storage mock.
fn build_verify_targets<L, V>(
    lookup: L,
    latest_quick: V,
    events: &[FileDiscoveredEvent],
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
) -> (Vec<QuickVerifyTarget>, u64)
where
    L: Fn(&std::path::Path) -> Option<voom_domain::media::MediaFile>,
    V: Fn(&str) -> Option<chrono::DateTime<chrono::Utc>>,
{
    let mut targets = Vec::new();
    let mut skipped_fresh = 0u64;
    for ev in events {
        let Some(file) = lookup(&ev.path) else {
            continue;
        };
        let file_id = file.id.to_string();
        if let Some(cutoff_ts) = cutoff {
            if let Some(when) = latest_quick(&file_id) {
                if when >= cutoff_ts {
                    skipped_fresh += 1;
                    continue;
                }
            }
        }
        targets.push(QuickVerifyTarget {
            file_id,
            path: file.path.clone(),
        });
    }
    (targets, skipped_fresh)
}

/// Run a quick-verification fan-out after scan completes. This is invoked
/// only when `--verify` is set or `[plugin.verifier] verify_on_scan = true`.
fn run_verify_pass(
    store: &Arc<dyn StorageTrait>,
    cfg: &VerifierConfig,
    events: &[FileDiscoveredEvent],
    workers: usize,
    quiet: bool,
    token: &CancellationToken,
) {
    if token.is_cancelled() {
        return;
    }
    let cutoff = freshness_cutoff(cfg.verify_freshness_days);
    let store_lookup = store.clone();
    let store_latest = store.clone();
    let (targets, skipped_fresh) = build_verify_targets(
        |p| match store_lookup.file_by_path(p) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "file_by_path failed");
                None
            }
        },
        |file_id| match store_latest.latest_verification(file_id, VerificationMode::Quick) {
            Ok(Some(rec)) => Some(rec.verified_at),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    file_id = %file_id,
                    error = %e,
                    "latest_verification failed; will re-verify"
                );
                None
            }
        },
        events,
        cutoff,
    );

    if targets.is_empty() {
        if !quiet && skipped_fresh > 0 {
            eprintln!(
                "  {} verification (all {} files verified within last {} days)",
                style("Skipped").dim(),
                skipped_fresh,
                cfg.verify_freshness_days,
            );
        }
        return;
    }

    if !quiet {
        eprintln!(
            "  {} {} files (quick mode){}",
            style("Verifying").dim(),
            targets.len(),
            if skipped_fresh > 0 {
                format!(", {skipped_fresh} fresh skipped")
            } else {
                String::new()
            },
        );
    }

    let records = match run_quick_pass(store, cfg, &targets, workers) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "verify pass failed to run");
            return;
        }
    };

    if !quiet {
        print_verify_summary(&records);
    }
}

/// Print a one-line summary of a quick-verify pass.
fn print_verify_summary(records: &[VerificationRecord]) {
    let ok = records
        .iter()
        .filter(|r| r.outcome == VerificationOutcome::Ok)
        .count();
    let warn = records
        .iter()
        .filter(|r| r.outcome == VerificationOutcome::Warning)
        .count();
    let err = records
        .iter()
        .filter(|r| r.outcome == VerificationOutcome::Error)
        .count();
    let summary = format!("{ok} ok, {warn} warning, {err} error");
    eprintln!(
        "  {} {} ({})",
        style("Verified").dim(),
        records.len(),
        if err > 0 {
            style(summary).red().to_string()
        } else if warn > 0 {
            style(summary).yellow().to_string()
        } else {
            style(summary).green().to_string()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{
        EventResult, FileDiscoveredEvent, FileIntrospectedEvent, IntrospectSessionCompletedEvent,
    };
    use voom_domain::media::MediaFile;

    /// A test plugin that counts received events.
    struct RecordingPlugin {
        discovered_count: AtomicUsize,
        introspected_count: AtomicUsize,
        introspect_session_completed_count: AtomicUsize,
        introspect_session_completed_files: AtomicU64,
    }

    impl RecordingPlugin {
        fn new() -> Self {
            Self {
                discovered_count: AtomicUsize::new(0),
                introspected_count: AtomicUsize::new(0),
                introspect_session_completed_count: AtomicUsize::new(0),
                introspect_session_completed_files: AtomicU64::new(0),
            }
        }
    }

    impl voom_kernel::Plugin for RecordingPlugin {
        fn name(&self) -> &'static str {
            "test-recorder"
        }
        fn version(&self) -> &'static str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, event_type: &str) -> bool {
            matches!(
                event_type,
                Event::FILE_DISCOVERED
                    | Event::FILE_INTROSPECTED
                    | Event::INTROSPECT_SESSION_COMPLETED
            )
        }
        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            match event {
                Event::FileDiscovered(_) => {
                    self.discovered_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::FileIntrospected(_) => {
                    self.introspected_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::IntrospectSessionCompleted(e) => {
                    self.introspect_session_completed_count
                        .fetch_add(1, Ordering::SeqCst);
                    self.introspect_session_completed_files
                        .store(e.files_introspected, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(None)
        }
    }

    fn test_media_file(name: &str) -> MediaFile {
        let mut f = MediaFile::new(PathBuf::from(name));
        f.size = 1024;
        f.content_hash = Some("abc123".into());
        f
    }

    #[tokio::test]
    async fn test_events_dispatched_through_kernel() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        // Simulate discovery event
        let discovered =
            FileDiscoveredEvent::new(PathBuf::from("/tmp/test.mkv"), 1024, Some("abc123".into()));
        kernel.dispatch(Event::FileDiscovered(discovered));

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 1);

        // Simulate introspection event
        let file = test_media_file("/tmp/test.mkv");
        kernel.dispatch(Event::FileIntrospected(FileIntrospectedEvent::new(file)));

        assert_eq!(recorder.introspected_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multiple_discovery_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        let events = vec![
            FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 100, Some("aaa".into())),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/b.mp4"), 200, Some("bbb".into())),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/c.avi"), 300, Some("ccc".into())),
        ];

        for event in &events {
            kernel.dispatch(Event::FileDiscovered(event.clone()));
        }

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn freshness_cutoff_zero_returns_none() {
        assert!(freshness_cutoff(0).is_none());
    }

    #[test]
    fn freshness_cutoff_positive_returns_past_timestamp() {
        let cutoff = freshness_cutoff(7).expect("cutoff for 7 days");
        let now = chrono::Utc::now();
        let delta = now - cutoff;
        // ~7 days, allow generous slack for test scheduling
        assert!(delta.num_hours() >= 24 * 7 - 1);
        assert!(delta.num_hours() <= 24 * 7 + 1);
    }

    #[test]
    fn build_verify_targets_collects_files_with_known_paths() {
        let mut f1 = MediaFile::new(PathBuf::from("/m/a.mkv"));
        f1.size = 100;
        let mut f2 = MediaFile::new(PathBuf::from("/m/b.mkv"));
        f2.size = 200;
        let f1_clone = f1.clone();
        let f2_clone = f2.clone();

        let events = vec![
            FileDiscoveredEvent::new(f1.path.clone(), 100, Some("h1".into())),
            FileDiscoveredEvent::new(f2.path.clone(), 200, Some("h2".into())),
        ];

        let lookup = |p: &std::path::Path| {
            if p == f1_clone.path {
                Some(f1_clone.clone())
            } else if p == f2_clone.path {
                Some(f2_clone.clone())
            } else {
                None
            }
        };
        let no_records = |_: &str| None;

        let (targets, skipped) = build_verify_targets(lookup, no_records, &events, None);
        assert_eq!(targets.len(), 2);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn build_verify_targets_skips_unknown_paths() {
        let events = vec![FileDiscoveredEvent::new(
            PathBuf::from("/m/never-introspected.mkv"),
            100,
            Some("h".into()),
        )];
        let (targets, skipped) = build_verify_targets(|_| None, |_: &str| None, &events, None);
        assert!(targets.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn build_verify_targets_skips_files_verified_within_freshness() {
        let mut f = MediaFile::new(PathBuf::from("/m/fresh.mkv"));
        f.size = 100;
        let f_clone = f.clone();
        let events = vec![FileDiscoveredEvent::new(
            f.path.clone(),
            100,
            Some("h".into()),
        )];

        let lookup = |p: &std::path::Path| {
            if p == f_clone.path {
                Some(f_clone.clone())
            } else {
                None
            }
        };
        // Verified 1 day ago — well inside a 7-day cutoff.
        let recent = chrono::Utc::now() - chrono::Duration::days(1);
        let latest = move |_: &str| Some(recent);

        let cutoff = freshness_cutoff(7);
        let (targets, skipped) = build_verify_targets(lookup, latest, &events, cutoff);
        assert!(targets.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn build_verify_targets_includes_stale_records() {
        let mut f = MediaFile::new(PathBuf::from("/m/stale.mkv"));
        f.size = 100;
        let f_clone = f.clone();
        let events = vec![FileDiscoveredEvent::new(
            f.path.clone(),
            100,
            Some("h".into()),
        )];
        let lookup = |p: &std::path::Path| {
            if p == f_clone.path {
                Some(f_clone.clone())
            } else {
                None
            }
        };
        // Verified 30 days ago — past a 7-day cutoff.
        let stale = chrono::Utc::now() - chrono::Duration::days(30);
        let latest = move |_: &str| Some(stale);

        let cutoff = freshness_cutoff(7);
        let (targets, skipped) = build_verify_targets(lookup, latest, &events, cutoff);
        assert_eq!(targets.len(), 1);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn build_verify_targets_no_cutoff_includes_all_known() {
        let mut f = MediaFile::new(PathBuf::from("/m/x.mkv"));
        f.size = 100;
        let f_clone = f.clone();
        let events = vec![FileDiscoveredEvent::new(f.path.clone(), 100, None)];
        let lookup = move |_: &std::path::Path| Some(f_clone.clone());
        // Cutoff disabled → freshness check skipped entirely.
        let latest = |_: &str| Some(chrono::Utc::now());
        let (targets, skipped) = build_verify_targets(lookup, latest, &events, None);
        assert_eq!(targets.len(), 1);
        assert_eq!(skipped, 0);
    }

    #[tokio::test]
    async fn test_introspect_session_completed_kernel_roundtrip() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        kernel.dispatch(Event::IntrospectSessionCompleted(
            IntrospectSessionCompletedEvent::new(42),
        ));

        assert_eq!(
            recorder
                .introspect_session_completed_count
                .load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            recorder
                .introspect_session_completed_files
                .load(Ordering::SeqCst),
            42
        );
    }
}
