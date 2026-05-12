use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app;
use crate::cli::ScanArgs;
use crate::config;
use crate::output;
use crate::paths::resolve_paths;
use crate::progress::{DiscoveryProgress, ProbeProgress};
use anyhow::Result;
use console::style;
use indicatif::{HumanDuration, MultiProgress};
use tokio_util::sync::CancellationToken;
use voom_domain::events::{Event, FileDiscoveredEvent, ScanCompleteEvent};
use voom_domain::storage::StorageTrait;
use voom_domain::verification::{VerificationMode, VerificationOutcome, VerificationRecord};
use voom_verifier::VerifierConfig;

use crate::commands::verify::{QuickVerifyTarget, run_quick_pass};

mod pipeline;

/// Run the scan command.
///
/// Discovery, ingest, and introspection run as three concurrent streaming
/// stages. All events are published through the kernel's event bus so that
/// subscribers (sqlite-store, WASM plugins) receive them.
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

        let mp = MultiProgress::new();
        let discovery_progress = if quiet {
            DiscoveryProgress::hidden()
        } else {
            DiscoveryProgress::new_in(&mp)
        };
        let probe_progress = if quiet {
            ProbeProgress::hidden_dynamic()
        } else {
            ProbeProgress::new_dynamic_in(&mp)
        };

        let outcome = pipeline::run_streaming_pipeline(
            &args,
            &paths,
            hash_files,
            store.clone(),
            kernel.clone(),
            config.ffprobe_path().map(|s| s.to_owned()),
            config.animation_detection_mode(),
            discovery_progress,
            probe_progress,
            token.clone(),
        )
        .await?;

        if outcome.files_discovered == 0 {
            // Missing-file reconciliation already ran INSIDE the pipeline
            // (finish_scan_session for the hashed path, mark_missing_paths
            // for --no-hash). outcome.missing / outcome.orphans are
            // authoritative — do NOT re-run reconciliation here. A second
            // session would clobber the count from the first.
            if !quiet && outcome.missing > 0 {
                print_missing_count(outcome.missing);
            }
            if !quiet {
                if outcome.orphans > 0 {
                    eprintln!(
                        "{} ({} orphaned temp {} skipped)",
                        style("No media files found.").yellow(),
                        outcome.orphans,
                        if outcome.orphans == 1 {
                            "file"
                        } else {
                            "files"
                        },
                    );
                } else {
                    eprintln!("{}", style("No media files found.").yellow());
                }
            }
            if matches!(args.format, Some(crate::cli::OutputFormat::Json)) {
                #[allow(clippy::print_stdout)]
                {
                    println!("[]");
                }
            }
            return Ok(());
        }

        if !quiet {
            print_discovery_summary(
                outcome.files_discovered as usize,
                start.elapsed(),
                hash_files,
                outcome.orphans,
                outcome.discovery_errors,
            );
        }

        if !quiet {
            if outcome.missing > 0 {
                print_missing_count(outcome.missing);
            }
            if outcome.moved > 0 {
                eprintln!(
                    "  {} {} files moved/renamed",
                    style("Moved").dim(),
                    outcome.moved,
                );
            }
            if outcome.external_changes > 0 {
                eprintln!(
                    "  {} {} files changed externally",
                    style("Changed").dim(),
                    outcome.external_changes,
                );
            }
        }

        print_scan_summary(
            outcome.files_discovered as usize,
            outcome.files_introspected,
            outcome.errors(),
            start.elapsed(),
            token.is_cancelled(),
            quiet,
        );

        if token.is_cancelled() {
            if let Some(format) = args.format {
                output::format_scan_results(&outcome.formatted, format);
            }
            return Ok(());
        }

        purge_stale_records(&*store, config.pruning.retention_days, quiet);

        // Build FileDiscoveredEvent stubs for the verify helper.
        let all_events: Vec<FileDiscoveredEvent> = outcome
            .formatted
            .iter()
            .map(|(path, size, hash)| FileDiscoveredEvent::new(path.clone(), *size, hash.clone()))
            .collect();

        // Optional quick-verification pass after introspection.
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

        // `ScanComplete` carries both files_discovered and files_introspected and
        // is the single lifecycle event for a full scan. We deliberately do NOT
        // dispatch `IntrospectComplete` here — that event is reserved for
        // standalone re-introspection runs (see commands/process/mod.rs). Emitting
        // both would cause subscribers like the report plugin to capture two
        // back-to-back snapshots (see issue #153).
        kernel.dispatch(Event::ScanComplete(ScanCompleteEvent::new(
            outcome.files_discovered,
            outcome.files_introspected,
        )));

        if let Some(format) = args.format {
            output::format_scan_results(&outcome.formatted, format);
        }

        Ok(())
    }
    .await;

    crate::retention::maybe_run_after_cli(store, &config.retention, Some(kernel));

    primary_result
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

/// Print the final scan summary (completion or interruption).
///
/// Takes primitive counts plus elapsed time instead of borrowing the events
/// slice. This keeps tainted data from CodeQL's `rust/cleartext-logging` flow
/// analysis out of the logging sink — the print sites see only
/// `usize`/`u64`/`Duration`.
fn print_scan_summary(
    total_files: usize,
    introspected: u64,
    errors: u64,
    elapsed: Duration,
    cancelled: bool,
    quiet: bool,
) {
    let total = total_files as u64;
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
                total_files,
                introspected,
                total,
                error_suffix,
                HumanDuration(elapsed),
            );
        }
        return;
    }

    if !quiet {
        eprintln!(
            "\n{} {} files discovered, {} introspected{} ({})",
            style("Done.").bold().green(),
            total_files,
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
mod streaming_tests {
    use super::pipeline::{self, PipelineOutcome};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::storage::StorageTrait;
    use voom_domain::transition::FileStatus;

    struct Bed {
        _db_dir: TempDir,
        media_dir: TempDir,
        store: Arc<dyn StorageTrait>,
        kernel: Arc<voom_kernel::Kernel>,
    }

    fn make_bed() -> Bed {
        let db_dir = tempfile::tempdir().expect("temp db dir");
        let db_path = db_dir.path().join("voom.db");
        let store: Arc<dyn StorageTrait> =
            Arc::new(voom_sqlite_store::store::SqliteStore::open(&db_path).expect("open store"));
        let kernel = Arc::new(voom_kernel::Kernel::new());
        let media_dir = tempfile::tempdir().expect("temp media dir");
        Bed {
            _db_dir: db_dir,
            media_dir,
            store,
            kernel,
        }
    }

    fn args_with(probe_workers: usize, no_hash: bool) -> crate::cli::ScanArgs {
        crate::cli::ScanArgs {
            paths: vec![],
            recursive: true,
            workers: 0,
            probe_workers,
            no_hash,
            verify: false,
            format: None,
        }
    }

    async fn run_for(
        bed: &Bed,
        probe_workers: usize,
        hash_files: bool,
        ffprobe_path: Option<String>,
        token: CancellationToken,
    ) -> anyhow::Result<PipelineOutcome> {
        let args = args_with(probe_workers, !hash_files);
        pipeline::run_streaming_pipeline(
            &args,
            &[bed.media_dir.path().to_path_buf()],
            hash_files,
            bed.store.clone(),
            bed.kernel.clone(),
            ffprobe_path,
            voom_ffprobe_introspector::parser::AnimationDetectionMode::Off,
            crate::progress::DiscoveryProgress::hidden(),
            crate::progress::ProbeProgress::hidden_dynamic(),
            token,
        )
        .await
    }

    fn write_media(root: &std::path::Path, names: &[&str]) {
        for n in names {
            std::fs::write(root.join(n), b"fake-media-data").unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_scan_returns_zero_counts_and_no_missing() {
        let bed = make_bed();
        let outcome = run_for(&bed, 2, true, None, CancellationToken::new())
            .await
            .expect("pipeline ok");
        assert_eq!(outcome.files_discovered, 0);
        assert_eq!(outcome.files_introspected, 0);
        assert_eq!(outcome.errors(), 0);
        assert_eq!(outcome.missing, 0);
        assert_eq!(outcome.orphans, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_scan_does_not_re_run_reconciliation() {
        // Regression: previously, handle_empty_scan ran a second
        // reconcile_discovered_files after the pipeline already finished a
        // session — clobbering the missing count and double-running
        // missing-file detection.
        let bed = make_bed();
        let seeded_path = bed.media_dir.path().join("vanished.mkv");
        let mut seeded = MediaFile::new(seeded_path.clone())
            .with_container(Container::Mkv)
            .with_tracks(vec![Track::new(0, TrackType::Video, "h264".into())]);
        seeded.size = 9;
        seeded.content_hash = Some("abcdef0123456789".into());
        seeded.expected_hash = seeded.content_hash.clone();
        bed.store.upsert_file(&seeded).expect("seed upsert");

        let outcome = run_for(&bed, 2, true, None, CancellationToken::new())
            .await
            .expect("pipeline ok");
        assert_eq!(outcome.files_discovered, 0);
        assert_eq!(
            outcome.missing, 1,
            "first session must report the missing file"
        );

        // The seeded row should now be Missing in the DB.
        let row = bed
            .store
            .file_by_path(&seeded_path)
            .expect("file_by_path ok")
            .expect("row still in DB");
        assert_eq!(row.status, FileStatus::Missing);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_scan_does_not_mark_missing() {
        let bed = make_bed();
        let seeded_path = bed.media_dir.path().join("seed.mkv");
        let mut seeded = MediaFile::new(seeded_path.clone())
            .with_container(Container::Mkv)
            .with_tracks(vec![Track::new(0, TrackType::Video, "h264".into())]);
        seeded.size = 9;
        seeded.content_hash = Some("abcdef0123456789".into());
        seeded.expected_hash = seeded.content_hash.clone();
        bed.store.upsert_file(&seeded).expect("seed upsert");

        let token = CancellationToken::new();
        token.cancel();
        let outcome = run_for(&bed, 2, true, None, token)
            .await
            .expect("pipeline ok");
        assert_eq!(outcome.missing, 0, "cancelled scan must not mark missing");

        let row = bed
            .store
            .file_by_path(&seeded_path)
            .expect("file_by_path ok")
            .expect("seed still present");
        assert_eq!(row.status, FileStatus::Active);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn introspection_failures_are_counted() {
        let bed = make_bed();
        write_media(bed.media_dir.path(), &["a.mkv", "b.mkv", "c.mkv"]);

        let outcome = run_for(
            &bed,
            2,
            true,
            Some("/this/path/does/not/exist/ffprobe".to_string()),
            CancellationToken::new(),
        )
        .await
        .expect("pipeline ok");

        assert_eq!(outcome.files_discovered, 3);
        assert_eq!(outcome.files_introspected, 0);
        assert_eq!(outcome.probe_errors, 3);
        assert_eq!(outcome.discovery_errors, 0);
        assert_eq!(outcome.errors(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cache_hits_skip_probe_entirely() {
        let bed = make_bed();
        let file_path = bed.media_dir.path().join("cached.mkv");
        std::fs::write(&file_path, b"cached-content").unwrap();
        // Canonicalize so the stored path matches what the discovery scanner
        // emits (it calls normalize_path → fs::canonicalize, which on macOS
        // expands /var → /private/var).
        let canonical_path = std::fs::canonicalize(&file_path).unwrap_or(file_path);
        let on_disk_size = std::fs::metadata(&canonical_path).unwrap().len();
        let actual_hash = voom_discovery::hash_file(&canonical_path).expect("hash");

        let mut seeded = MediaFile::new(canonical_path.clone())
            .with_container(Container::Mkv)
            .with_tracks(vec![Track::new(0, TrackType::Video, "h264".into())]);
        seeded.size = on_disk_size;
        seeded.content_hash = Some(actual_hash.clone());
        seeded.expected_hash = Some(actual_hash);
        bed.store.upsert_file(&seeded).expect("seed upsert");

        // Broken ffprobe path: any probe invocation would bump probe_errors.
        let outcome = run_for(
            &bed,
            2,
            true,
            Some("/this/path/does/not/exist/ffprobe".to_string()),
            CancellationToken::new(),
        )
        .await
        .expect("pipeline ok");

        assert_eq!(outcome.files_discovered, 1);
        assert_eq!(
            outcome.files_introspected, 0,
            "cache hit should skip ffprobe"
        );
        assert_eq!(
            outcome.probe_errors, 0,
            "ffprobe must not have been invoked"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_finalisation_visible_after_pipeline_returns() {
        let bed = make_bed();
        write_media(bed.media_dir.path(), &["x.mkv", "y.mkv"]);

        let outcome = run_for(
            &bed,
            2,
            true,
            Some("/no/such/ffprobe".to_string()),
            CancellationToken::new(),
        )
        .await
        .expect("pipeline ok");
        assert_eq!(outcome.files_discovered, 2);

        // The scanner canonicalizes paths (normalize_path → fs::canonicalize),
        // so rows are stored under the real path. Canonicalize the lookup path
        // to match on macOS where /var is a symlink to /private/var.
        let canonical_dir = std::fs::canonicalize(bed.media_dir.path())
            .unwrap_or_else(|_| bed.media_dir.path().to_path_buf());
        for name in ["x.mkv", "y.mkv"] {
            let p = canonical_dir.join(name);
            let row = bed
                .store
                .file_by_path(&p)
                .expect("file_by_path ok")
                .expect("row present after drain");
            assert_eq!(row.status, FileStatus::Active);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_hash_mode_streams_through_probe() {
        let bed = make_bed();
        write_media(bed.media_dir.path(), &["a.mkv", "b.mkv"]);

        let outcome = run_for(
            &bed,
            2,
            false, // --no-hash
            Some("/no/such/ffprobe".to_string()),
            CancellationToken::new(),
        )
        .await
        .expect("pipeline ok");

        assert_eq!(outcome.files_discovered, 2);
        assert_eq!(outcome.files_introspected, 0);
        assert_eq!(outcome.probe_errors, 2);
    }
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
