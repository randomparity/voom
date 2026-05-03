mod dispatch;
mod pipeline;
mod plan_outcome;
mod safeguards;
mod transitions;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use anyhow::{Context, Result};
use console::style;
use parking_lot::Mutex;

use tokio_util::sync::CancellationToken;

use dispatch::dispatch_and_log;
use pipeline::process_single_file;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use crate::config;
use crate::paths::resolve_paths;
use crate::policy_map::PolicyResolver;
use crate::progress::{BatchProgress, DiscoveryProgress};
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{
    Event, IntrospectCompleteEvent, JobCompletedEvent, JobProgressEvent, JobStartedEvent,
};
use voom_domain::utils::format::format_size;
use voom_job_manager::progress::{CompositeReporter, ProgressReporter};
use voom_job_manager::worker::{JobErrorStrategy, WorkerPool, WorkerPoolConfig};

/// Run the process command.
///
/// Uses the event-driven + direct-call pattern throughout:
///
/// - **Discovery** — called directly for progress/filtering control, then each
///   `FileDiscovered` event is dispatched so sqlite-store records the file in
///   `discovered_files` and ffprobe-introspector enqueues introspection jobs.
/// - **Introspection** — called directly via `introspect_file()` for
///   deterministic worker-pool concurrency. The result `FileIntrospected`
///   event is dispatched for persistence.
/// - **Policy evaluation & orchestration** — called directly to produce `Plan`
///   structs. No events dispatched at this stage (avoids triggering executors
///   during dry-run).
/// - **Plan execution** — `PlanExecuting` and `PlanCreated` events ARE
///   dispatched through the kernel so that backup-manager and executor plugins
///   handle them via the event bus.
///
/// This split gives the CLI full control over ordering, concurrency, and
/// progress reporting while still letting kernel-registered plugins react to
/// the events they care about.
pub async fn run(args: ProcessArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    if args.plan_only && args.approve {
        anyhow::bail!(
            "--plan-only and --approve cannot be used together \
             (plan-only skips execution)"
        );
    }

    let plan_only = args.plan_only;
    let dry_run = args.dry_run || plan_only;

    let config = config::load_config()?;
    let app::BootstrapResult {
        kernel,
        store,
        collector,
        job_queue,
        ..
    } = app::bootstrap_kernel_with_store(&config)?;
    let kernel = Arc::new(kernel);
    let store_for_retention = store.clone();
    let kernel_for_retention = kernel.clone();
    let capabilities = Arc::new(collector.snapshot());

    let paths = resolve_paths(&args.paths)?;

    let primary_result: Result<()> = async {
        let root = if paths.len() == 1 && paths[0].is_file() {
            paths[0]
                .parent()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot determine parent directory of {}",
                        paths[0].display()
                    )
                })?
                .to_path_buf()
        } else {
            paths[0].clone()
        };

        let resolver = build_policy_resolver(&args, &config, &root)?;
        let counters = RunCounters::new();

        if !plan_only && !quiet {
            let path_list: Vec<_> = paths.iter().map(|p| p.display().to_string()).collect();
            let display_paths = path_list.join(", ");
            print_run_header(
                &resolver.summary(),
                &display_paths,
                dry_run,
                counters.session_id,
            );
        }

        // Auto-prune stale file entries under the target directories
        for path in &paths {
            match store.prune_missing_files_under(path) {
                Ok(n) if n > 0 && !plan_only && !quiet => eprintln!("Pruned {n} stale entries."),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "auto-prune failed"),
            }
        }

        // Check for orphaned backups left by a previous crashed execution
        // Extract backup-manager's global backup dir from plugin config, if set.
        let global_backup_dir: Option<std::path::PathBuf> =
            config.plugin.get("backup-manager").and_then(|t| {
                let use_global = t
                    .get("use_global_dir")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                if use_global {
                    t.get("backup_dir")
                        .and_then(|v| v.as_str())
                        .map(std::path::PathBuf::from)
                } else {
                    None
                }
            });
        match crate::recovery::check_and_recover_under(
            &config.recovery,
            &paths,
            store.as_ref(),
            global_backup_dir.as_deref(),
        ) {
            Ok(recovered) if recovered > 0 && !plan_only && !quiet => {
                eprintln!(
                    "{} {} {} from crashed execution",
                    console::style("Recovered").bold().green(),
                    recovered,
                    if recovered == 1 { "file" } else { "files" }
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "crash recovery check failed"),
        }

        if token.is_cancelled() {
            if !plan_only && !quiet {
                eprintln!("{}", style("Interrupted before discovery.").yellow());
            }
            return Ok(());
        }

        let mut events = discover_files(&paths, &args, &kernel, quiet, store.clone())?;
        if events.is_empty() {
            if plan_only {
                println!("[]");
            } else if !quiet {
                eprintln!("{}", style("No media files found.").yellow());
            }
            return Ok(());
        }

        // Filter out known-bad files unless --force-rescan is set
        if !args.force_rescan {
            filter_bad_files(&mut events, &store, plan_only, quiet)?;
        }

        if events.is_empty() {
            if plan_only {
                println!("[]");
            } else if !quiet {
                eprintln!("{}", style("No processable files found.").yellow());
            }
            return Ok(());
        }

        let file_count = events.len();
        if !plan_only && !quiet {
            eprintln!("Found {} media files.", style(file_count).bold());
        }

        let on_error = match args.on_error {
            ErrorHandling::Fail => JobErrorStrategy::Fail,
            ErrorHandling::Continue => JobErrorStrategy::Continue,
        };

        if token.is_cancelled() {
            if !quiet {
                eprintln!("{}", style("Interrupted before processing.").yellow());
            }
            return Ok(());
        }

        let (pool, effective_workers) = create_worker_pool(job_queue, &args, token.clone());

        let reporter = build_reporter(&events, effective_workers, plan_only, quiet, kernel.clone());

        let items = build_work_items(&events, args.priority_by_date);
        let all_phase_names = resolver.all_phase_names();
        let resolver = Arc::new(resolver);
        let flag_size_increase = args.flag_size_increase;
        let flag_duration_shrink = args.flag_duration_shrink;
        let force_rescan = args.force_rescan;

        let token_for_workers = token.clone();
        let ffprobe_path: Option<String> = config.ffprobe_path().map(String::from);
        let ffprobe_path = Arc::new(ffprobe_path);
        let counters_for_summary = counters.clone();
        let kernel_for_completion = kernel.clone();
        let _results = pool
            .process_batch(
                items,
                move |job| {
                    let resolver = resolver.clone();
                    let kernel = kernel.clone();
                    let store = store.clone();
                    let token = token_for_workers.clone();
                    let ffprobe_path = ffprobe_path.clone();
                    let capabilities = capabilities.clone();
                    let counters = counters.clone();
                    async move {
                        let ctx = ProcessContext {
                            resolver: &resolver,
                            kernel,
                            store,
                            dry_run,
                            plan_only,
                            flag_size_increase,
                            flag_duration_shrink,
                            force_rescan,
                            token: &token,
                            ffprobe_path: ffprobe_path.as_deref(),
                            capabilities: &capabilities,
                            counters: &counters,
                        };
                        process_single_file(job, &ctx).await
                    }
                },
                on_error,
                reporter.clone(),
            )
            .await;

        if !token.is_cancelled() {
            kernel_for_completion.dispatch(Event::IntrospectComplete(
                IntrospectCompleteEvent::new(pool.completed_count()),
            ));
        }

        print_run_results(&RunResultsContext {
            counters: &counters_for_summary,
            plan_only,
            quiet,
            cancelled: token.is_cancelled(),
            pool: &pool,
            file_count,
            effective_workers,
            dry_run,
            paths: &paths,
            all_phase_names: &all_phase_names,
        })
    }
    .await;

    crate::retention::maybe_run_after_cli(
        store_for_retention,
        &config.retention,
        Some(kernel_for_retention),
    );

    primary_result
}

/// Remove known-bad files from the event list, logging how many were skipped.
fn filter_bad_files(
    events: &mut Vec<voom_domain::events::FileDiscoveredEvent>,
    store: &Arc<dyn voom_domain::storage::StorageTrait>,
    plan_only: bool,
    quiet: bool,
) -> Result<()> {
    let bad_files = store
        .list_bad_files(&voom_domain::storage::BadFileFilters::default())
        .context("failed to list bad files")?;
    if bad_files.is_empty() {
        return Ok(());
    }
    let bad_paths: std::collections::HashSet<_> = bad_files.iter().map(|bf| &bf.path).collect();
    let before = events.len();
    events.retain(|e| !bad_paths.contains(&e.path));
    let skipped = before - events.len();
    if skipped > 0 && !plan_only && !quiet {
        eprintln!(
            "Skipping {} known-bad files (use {} to re-attempt).",
            style(skipped).yellow(),
            style("--force-rescan").bold()
        );
    }
    Ok(())
}

/// Build the progress reporter stack for the processing batch.
fn build_reporter(
    events: &[voom_domain::events::FileDiscoveredEvent],
    effective_workers: usize,
    plan_only: bool,
    quiet: bool,
    kernel: Arc<voom_kernel::Kernel>,
) -> Arc<dyn ProgressReporter> {
    let bus_reporter: Arc<dyn ProgressReporter> = Arc::new(EventBusReporter::new(kernel));
    if plan_only {
        return bus_reporter;
    }
    let cli_reporter: Arc<dyn ProgressReporter> = if quiet {
        Arc::new(BatchProgress::hidden(events, effective_workers))
    } else {
        Arc::new(BatchProgress::new(events, effective_workers))
    };
    Arc::new(CompositeReporter::new(vec![cli_reporter, bus_reporter]))
}

/// Arguments for `print_run_results`.
#[allow(clippy::struct_excessive_bools)]
struct RunResultsContext<'a> {
    counters: &'a RunCounters,
    plan_only: bool,
    quiet: bool,
    cancelled: bool,
    pool: &'a WorkerPool,
    file_count: usize,
    effective_workers: usize,
    dry_run: bool,
    paths: &'a [std::path::PathBuf],
    all_phase_names: &'a [String],
}

/// Print final results: plan-only JSON output or execution summary.
fn print_run_results(ctx: &RunResultsContext<'_>) -> Result<()> {
    if ctx.plan_only {
        let plans = ctx.counters.plan_collector.lock();
        let output = serde_json::to_string_pretty(&*plans).context("failed to serialize plans")?;
        println!("{output}");
        return Ok(());
    }

    let modified = ctx.counters.modified_count.load(AtomicOrdering::Relaxed);
    let backup_total = ctx.counters.backup_bytes.load(AtomicOrdering::Relaxed);
    if !ctx.quiet {
        if ctx.cancelled {
            print_interrupted_summary(ctx.pool, ctx.file_count, modified);
        } else {
            print_summary(&SummaryContext {
                pool: ctx.pool,
                file_count: ctx.file_count,
                modified,
                effective_workers: ctx.effective_workers,
                dry_run: ctx.dry_run,
                backup_bytes: backup_total,
                paths: ctx.paths,
            });
        }
        print_phase_breakdown(&ctx.counters.phase_stats.lock(), ctx.all_phase_names);

        let phase_errors: u64 = ctx
            .counters
            .phase_stats
            .lock()
            .values()
            .map(|ps| ps.failed)
            .sum();
        if phase_errors > 0 && !ctx.dry_run {
            let short_session = &ctx.counters.session_id.to_string()[..8];
            eprintln!(
                "\n  {} Run `{}` to see details.",
                style(format!("{phase_errors} files had errors.")).yellow(),
                style(format!("voom report errors --session {short_session}")).bold(),
            );
        }
    }

    Ok(())
}

/// Build a `PolicyResolver` from CLI args and config.
fn build_policy_resolver(
    args: &ProcessArgs,
    config: &crate::config::AppConfig,
    root: &std::path::Path,
) -> Result<PolicyResolver> {
    if let Some(ref policy_path) = args.policy {
        let resolved = crate::config::resolve_policy_path(policy_path);
        let source = std::fs::read_to_string(&resolved)
            .with_context(|| format!("Failed to read policy: {}", resolved.display()))?;
        let compiled = voom_dsl::compile_policy(&source).context("policy compilation failed")?;
        Ok(PolicyResolver::from_single(compiled, root))
    } else if let Some(ref map_path) = args.policy_map {
        PolicyResolver::from_map_file(map_path, root)
    } else if !config.policy_mapping.is_empty() || config.default_policy.is_some() {
        PolicyResolver::from_config(config, root)
    } else {
        anyhow::bail!(
            "no policy specified; use --policy, --policy-map, \
             or configure policy_mapping in config.toml"
        );
    }
}

/// Print the header line describing what we are about to do.
fn print_run_header(policy_name: &str, path_display: &str, dry_run: bool, session_id: uuid::Uuid) {
    let short_session = &session_id.to_string()[..8];
    eprintln!(
        "{} policy {} to {} (session {}){}",
        if dry_run {
            style("Dry-running").bold()
        } else {
            style("Applying").bold()
        },
        style(policy_name).cyan(),
        style(path_display).cyan(),
        style(short_session).dim(),
        if dry_run {
            " (no changes will be made)"
        } else {
            ""
        }
    );
}

/// Walk the filesystem and discover media files, dispatching events through the kernel.
///
/// Creates a standalone `DiscoveryPlugin` for direct API access (scan options,
/// progress callbacks) that the event-bus path does not support. `FileDiscovered`
/// events are dispatched to the kernel so that subscribers react:
/// - sqlite-store records each file in `discovered_files`
/// - ffprobe-introspector enqueues `JobType::Introspect` jobs
///
/// Introspection is still driven directly by `process_single_file` for
/// deterministic progress reporting.
fn discover_files(
    paths: &[std::path::PathBuf],
    args: &ProcessArgs,
    kernel: &voom_kernel::Kernel,
    quiet: bool,
    store: Arc<dyn voom_domain::storage::StorageTrait>,
) -> Result<Vec<voom_domain::events::FileDiscoveredEvent>> {
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let hash_files = !args.no_backup;

    let progress = if quiet {
        DiscoveryProgress::hidden()
    } else {
        DiscoveryProgress::new()
    };

    let discovery_errors: Arc<Mutex<Vec<(std::path::PathBuf, u64, String)>>> =
        Arc::new(Mutex::new(Vec::new()));

    let mut all_events: Vec<voom_domain::events::FileDiscoveredEvent> = Vec::new();

    // Cumulative counters so the progress bar shows totals across all
    // directories instead of resetting per directory.
    let cumulative_discovered = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let processing_base = Arc::new(std::sync::atomic::AtomicU64::new(0));

    for path in paths {
        // Reset bar to spinner so stale position/length from the previous
        // directory's processing phase doesn't bleed into this discovery.
        progress.reset_to_spinner();

        let mut options = voom_discovery::ScanOptions::new(path.clone());
        options.hash_files = hash_files;
        options.workers = args.workers;
        options.fingerprint_lookup = Some(crate::introspect::fingerprint_lookup(store.clone()));

        let progress_clone = progress.clone();
        let cum_disc = cumulative_discovered.clone();
        let proc_base = processing_base.clone();
        let pre_scan_discovered = cumulative_discovered.load(std::sync::atomic::Ordering::Relaxed);

        options.on_progress = Some(Box::new(move |p| match p {
            voom_discovery::ScanProgress::Discovered { count: _, path } => {
                let cumulative = cum_disc.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let cumulative = usize::try_from(cumulative).unwrap_or(usize::MAX);
                progress_clone.on_discovered(cumulative, &path);
            }
            voom_discovery::ScanProgress::Processing {
                current,
                total,
                path,
            } => {
                let base = proc_base.load(std::sync::atomic::Ordering::Relaxed);
                let base = usize::try_from(base).unwrap_or(usize::MAX);
                let action = if hash_files { "Hashing" } else { "Scanning" };
                progress_clone.on_processing(base + current, base + total, &path, action);
            }
            voom_discovery::ScanProgress::OrphanedTempFiles { .. } => {}
            voom_discovery::ScanProgress::HashReused { .. } => {}
        }));

        let errors_clone = discovery_errors.clone();
        options.on_error = Some(Box::new(move |path, size, error| {
            tracing::warn!(path = %path.display(), error = %error, "discovery error");
            errors_clone.lock().push((path, size, error));
        }));

        let events = discovery.scan(&options).context("filesystem scan failed")?;

        let dir_discovered =
            cumulative_discovered.load(std::sync::atomic::Ordering::Relaxed) - pre_scan_discovered;
        processing_base.fetch_add(dir_discovered, std::sync::atomic::Ordering::Relaxed);

        all_events.extend(events);
    }

    progress.finish();

    let mut seen = std::collections::HashSet::new();
    all_events.retain(|e| seen.insert(e.path.clone()));

    for event in &all_events {
        dispatch_and_log(kernel, Event::FileDiscovered(event.clone()));
    }

    for (path, size, error) in discovery_errors.lock().drain(..) {
        crate::introspect::dispatch_failure(
            kernel,
            path,
            size,
            None,
            &error,
            BadFileSource::Discovery,
        );
    }

    Ok(all_events)
}

use crate::introspect::DiscoveredFilePayload;

/// Compute job priority based on file modification date.
///
/// More recently modified files get higher priority (lower number).
/// - Modified within 7 days: 10
/// - Modified within 30 days: 50
/// - Modified within 1 year: 100
/// - Older or metadata unavailable: 200
fn compute_file_date_priority(path: &std::path::Path) -> i32 {
    const SECS_PER_DAY: u64 = 86_400;
    let Ok(metadata) = std::fs::metadata(path) else {
        return 200;
    };
    let Ok(modified) = metadata.modified() else {
        return 200;
    };
    let Ok(elapsed) = std::time::SystemTime::now().duration_since(modified) else {
        return 200;
    };
    let days = elapsed.as_secs() / SECS_PER_DAY;
    if days < 7 {
        10
    } else if days < 30 {
        50
    } else if days < 365 {
        100
    } else {
        200
    }
}

/// Build work items from discovery events for the worker pool.
fn build_work_items(
    events: &[voom_domain::events::FileDiscoveredEvent],
    priority_by_date: bool,
) -> Vec<voom_job_manager::worker::WorkItem<DiscoveredFilePayload>> {
    events
        .iter()
        .map(|evt| {
            let priority = if priority_by_date {
                compute_file_date_priority(&evt.path)
            } else {
                100
            };
            voom_job_manager::worker::WorkItem::new(
                voom_domain::job::JobType::Process,
                priority,
                Some(DiscoveredFilePayload {
                    path: evt.path.to_string_lossy().into_owned(),
                    size: evt.size,
                    content_hash: evt.content_hash.clone(),
                }),
            )
        })
        .collect()
}

/// Set up the worker pool using the provided job queue.
fn create_worker_pool(
    queue: Arc<voom_job_manager::queue::JobQueue>,
    args: &ProcessArgs,
    token: CancellationToken,
) -> (WorkerPool, usize) {
    let mut config = WorkerPoolConfig::default();
    config.max_workers = args.workers;
    config.worker_prefix = "voom".to_string();
    let effective_workers = config.effective_workers();

    let pool = WorkerPool::new(queue, config, token);

    (pool, effective_workers)
}

#[derive(Debug, Default)]
pub(super) struct PhaseStats {
    completed: u64,
    skipped: u64,
    failed: u64,
    skip_reasons: HashMap<String, u64>,
}

pub(super) type PhaseStatsMap = Arc<Mutex<HashMap<String, PhaseStats>>>;

pub(super) fn record_phase_stat(
    stats: &PhaseStatsMap,
    phase_name: &str,
    outcome: PhaseOutcomeKind,
) {
    let mut map = stats.lock();
    let entry = map.entry(phase_name.to_string()).or_default();
    match outcome {
        PhaseOutcomeKind::Completed => entry.completed += 1,
        PhaseOutcomeKind::Skipped(reason) => {
            entry.skipped += 1;
            *entry.skip_reasons.entry(reason).or_insert(0) += 1;
        }
        PhaseOutcomeKind::Failed => entry.failed += 1,
    }
}

pub(super) enum PhaseOutcomeKind {
    Completed,
    Skipped(String),
    Failed,
}

/// Shared mutable counters accumulated during a processing run.
///
/// `parking_lot::Mutex` is safe here because the lock is never held across
/// `.await` points — `phase_stats` is only locked inside synchronous
/// closures (`record_phase_stat`) that complete before any await.
#[derive(Clone)]
pub(super) struct RunCounters {
    pub(super) modified_count: Arc<AtomicU64>,
    pub(super) backup_bytes: Arc<AtomicU64>,
    pub(super) phase_stats: PhaseStatsMap,
    pub(super) plan_collector: Arc<Mutex<Vec<serde_json::Value>>>,
    pub(super) session_id: uuid::Uuid,
}

impl RunCounters {
    fn new() -> Self {
        Self {
            modified_count: Arc::new(AtomicU64::new(0)),
            backup_bytes: Arc::new(AtomicU64::new(0)),
            phase_stats: Arc::new(Mutex::new(HashMap::new())),
            plan_collector: Arc::new(Mutex::new(Vec::new())),
            session_id: uuid::Uuid::new_v4(),
        }
    }
}

/// Shared context for processing a single file.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct ProcessContext<'a> {
    pub(super) resolver: &'a PolicyResolver,
    pub(super) kernel: Arc<voom_kernel::Kernel>,
    pub(super) store: Arc<dyn voom_domain::storage::StorageTrait>,
    pub(super) dry_run: bool,
    pub(super) plan_only: bool,
    pub(super) flag_size_increase: bool,
    pub(super) flag_duration_shrink: bool,
    /// When true, bypass the introspection cache and force a fresh ffprobe pass.
    pub(super) force_rescan: bool,
    pub(super) token: &'a CancellationToken,
    pub(super) ffprobe_path: Option<&'a str>,
    pub(super) capabilities: &'a voom_domain::CapabilityMap,
    pub(super) counters: &'a RunCounters,
}

/// Print a summary when interrupted by CTRL-C.
fn print_interrupted_summary(pool: &WorkerPool, file_count: usize, modified: u64) {
    let completed = pool.completed_count();
    let failed = pool.failed_count();
    eprintln!(
        "\n{} {}/{} processed, {} modified, {} errors",
        style("Interrupted.").bold().yellow(),
        completed,
        file_count,
        modified,
        failed,
    );
}

/// Grouped arguments for the post-processing summary.
struct SummaryContext<'a> {
    pool: &'a WorkerPool,
    file_count: usize,
    modified: u64,
    effective_workers: usize,
    dry_run: bool,
    backup_bytes: u64,
    paths: &'a [std::path::PathBuf],
}

/// Print the final summary line after processing.
fn print_summary(ctx: &SummaryContext<'_>) {
    let completed = ctx.pool.completed_count();
    let failed = ctx.pool.failed_count();
    let skipped = (ctx.file_count as u64)
        .saturating_sub(completed)
        .saturating_sub(failed);

    let modified_label = if ctx.dry_run {
        "would modify"
    } else {
        "modified"
    };

    eprintln!(
        "\n{} {} processed, {} {modified_label}, {} skipped, {} errors (workers: {})",
        style("Done.").bold().green(),
        style(completed).green(),
        style(ctx.modified).cyan(),
        style(skipped).dim(),
        if failed > 0 {
            style(failed).red().to_string()
        } else {
            failed.to_string()
        },
        ctx.effective_workers,
    );

    if ctx.backup_bytes > 0 && ctx.modified > 0 && !ctx.dry_run {
        let path_args: Vec<_> = ctx.paths.iter().map(|p| p.display().to_string()).collect();
        eprintln!(
            "{} {} backups retained ({}) \u{2014} list with: voom backup list {} \
             \u{2014} delete with: voom backup cleanup {}",
            style("Info:").bold(),
            style(ctx.modified).cyan(),
            style(format_size(ctx.backup_bytes)).cyan(),
            path_args.join(" "),
            path_args.join(" "),
        );
    }

    if ctx.dry_run {
        eprintln!(
            "\n{}",
            style("This was a dry run. No files were modified.").dim()
        );
    }
}

fn print_phase_breakdown(stats: &HashMap<String, PhaseStats>, phase_order: &[String]) {
    if stats.is_empty() {
        return;
    }

    eprintln!("\n  {}", style("Phase breakdown:").bold());
    for phase_name in phase_order {
        let Some(ps) = stats.get(phase_name) else {
            continue;
        };
        let mut parts: Vec<String> = Vec::new();
        if ps.completed > 0 {
            parts.push(format!("{} completed", ps.completed));
        }
        if ps.skipped > 0 {
            let reasons = crate::output::format_skip_reasons(&ps.skip_reasons, 3);
            if reasons.is_empty() {
                parts.push(format!("{} skipped", ps.skipped));
            } else {
                parts.push(format!("{} skipped ({reasons})", ps.skipped));
            }
        }
        if ps.failed > 0 {
            parts.push(format!("{} errors", ps.failed));
        }
        eprintln!("    {:<20} {}", format!("{phase_name}:"), parts.join(", "));
    }
}

/// Reporter that dispatches job lifecycle events through the kernel event bus.
///
/// `kernel.dispatch()` is synchronous. While most handlers are fast in-memory
/// operations, `sqlite-store` performs a blocking `SQLite` write on every dispatch.
/// The overhead is acceptable for job lifecycle events (low frequency), but this
/// should be revisited if dispatch latency becomes a concern.
struct EventBusReporter {
    kernel: Arc<voom_kernel::Kernel>,
}

impl EventBusReporter {
    fn new(kernel: Arc<voom_kernel::Kernel>) -> Self {
        Self { kernel }
    }
}

impl ProgressReporter for EventBusReporter {
    fn on_batch_start(&self, _total: usize) {}

    fn on_job_start(&self, job: &voom_domain::job::Job) {
        self.kernel.dispatch(Event::JobStarted(JobStartedEvent::new(
            job.id,
            job.job_type.to_string(),
        )));
    }

    fn on_job_progress(&self, job_id: uuid::Uuid, progress: f64, message: Option<&str>) {
        let mut event = JobProgressEvent::new(job_id, progress);
        event.message = message.map(String::from);
        self.kernel.dispatch(Event::JobProgress(event));
    }

    fn on_job_complete(&self, job_id: uuid::Uuid, success: bool, error: Option<&str>) {
        let mut event = JobCompletedEvent::new(job_id, success);
        event.message = error.map(String::from);
        self.kernel.dispatch(Event::JobCompleted(event));
    }

    fn on_batch_complete(&self, _completed: u64, _failed: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{
        EventResult, FileDiscoveredEvent, FileIntrospectedEvent, PlanCreatedEvent, PlanSkippedEvent,
    };
    use voom_domain::media::MediaFile;
    use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

    use super::pipeline::{
        apply_detected_languages, execute_single_plan, AUDIO_LANGUAGE_DETECTOR_PLUGIN,
    };
    use super::plan_outcome::PlanOutcome;
    use super::safeguards::{check_disk_space, check_duration_shrink, check_size_increase};

    /// A test plugin that counts received plan lifecycle events.
    #[allow(clippy::struct_field_names)]
    struct PlanRecordingPlugin {
        discovered_count: AtomicUsize,
        introspected_count: AtomicUsize,
        plan_created_count: AtomicUsize,
        plan_executing_count: AtomicUsize,
        plan_completed_count: AtomicUsize,
        plan_skipped_count: AtomicUsize,
        plan_failed_count: AtomicUsize,
    }

    impl PlanRecordingPlugin {
        fn new() -> Self {
            Self {
                discovered_count: AtomicUsize::new(0),
                introspected_count: AtomicUsize::new(0),
                plan_created_count: AtomicUsize::new(0),
                plan_executing_count: AtomicUsize::new(0),
                plan_completed_count: AtomicUsize::new(0),
                plan_skipped_count: AtomicUsize::new(0),
                plan_failed_count: AtomicUsize::new(0),
            }
        }
    }

    impl voom_kernel::Plugin for PlanRecordingPlugin {
        fn name(&self) -> &'static str {
            "plan-recorder"
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
                    | Event::PLAN_CREATED
                    | Event::PLAN_EXECUTING
                    | Event::PLAN_COMPLETED
                    | Event::PLAN_SKIPPED
                    | Event::PLAN_FAILED
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
                Event::PlanCreated(_) => {
                    self.plan_created_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::PlanExecuting(_) => {
                    self.plan_executing_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::PlanCompleted(_) => {
                    self.plan_completed_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::PlanSkipped(_) => {
                    self.plan_skipped_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::PlanFailed(_) => {
                    self.plan_failed_count.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(None)
        }
    }

    pub(super) fn test_plan(phase: &str, skipped: bool) -> Plan {
        let mut plan = Plan::new(
            MediaFile::new(PathBuf::from("/tmp/test.mkv")),
            "test-policy",
            phase,
        );
        plan.actions = vec![PlannedAction::track_op(
            OperationType::SetDefault,
            0,
            ActionParams::Empty,
            "test action",
        )];
        if skipped {
            plan.skip_reason = Some("skipped".into());
        }
        plan
    }

    /// Bundle of long-lived test fixtures shared across `ProcessContext`
    /// construction sites. Owns the `TempDir` so the resolver's working path
    /// stays valid for the test's lifetime.
    #[allow(dead_code)] // populated by Tasks 2-6 of issue #159
    pub(super) struct TestFixture {
        pub(super) capabilities: voom_domain::CapabilityMap,
        pub(super) counters: RunCounters,
        pub(super) token: CancellationToken,
        pub(super) resolver: PolicyResolver,
        // Held for lifetime; the resolver borrows `dir.path()`.
        dir: tempfile::TempDir,
    }

    #[allow(dead_code)] // populated by Tasks 2-6 of issue #159
    impl TestFixture {
        /// Default fixture with the canonical `phase normalize { container mkv }` policy.
        pub(super) fn new() -> Self {
            Self::with_policy(r#"policy "test" { phase normalize { container mkv } }"#)
        }

        /// Fixture parameterised by an arbitrary policy DSL source.
        pub(super) fn with_policy(dsl: &str) -> Self {
            let dir =
                tempfile::tempdir().expect("TestFixture: failed to create tempdir for resolver");
            let resolver = PolicyResolver::from_single(
                voom_dsl::compile_policy(dsl)
                    .expect("TestFixture: fixture policy DSL must compile"),
                dir.path(),
            );
            Self {
                capabilities: voom_domain::CapabilityMap::new(),
                counters: RunCounters::new(),
                token: CancellationToken::new(),
                resolver,
                dir,
            }
        }

        /// Path to the held `TempDir`. Use for placing fixture media files
        /// alongside the resolver's working directory.
        pub(super) fn dir_path(&self) -> &std::path::Path {
            self.dir.path()
        }

        /// Pre-cancel the fixture token. Used by tests that exercise
        /// cancellation-aware code paths.
        pub(super) fn cancel(&self) {
            self.token.cancel();
        }

        /// Build a `ProcessContext` with all flags defaulted to `false` and
        /// `ffprobe_path = None`. Override per-test using struct-update syntax:
        ///
        /// ```ignore
        /// let ctx = ProcessContext {
        ///     flag_size_increase: true,
        ///     ..fixture.make_ctx(Arc::new(kernel), store)
        /// };
        /// ```
        pub(super) fn make_ctx<'a>(
            &'a self,
            kernel: Arc<voom_kernel::Kernel>,
            store: Arc<dyn voom_domain::storage::StorageTrait>,
        ) -> ProcessContext<'a> {
            ProcessContext {
                resolver: &self.resolver,
                kernel,
                store,
                dry_run: false,
                plan_only: false,
                flag_size_increase: false,
                flag_duration_shrink: false,
                force_rescan: false,
                token: &self.token,
                ffprobe_path: None,
                capabilities: &self.capabilities,
                counters: &self.counters,
            }
        }
    }

    #[tokio::test]
    async fn test_execute_single_plan_dispatches_lifecycle_events() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let plan = test_plan("normalize", false);

        // Call the actual production function — returns outcome, caller
        // dispatches PlanCompleted/PlanFailed.
        let outcome = execute_single_plan(&plan, &file, &kernel);

        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 1);
        // No executor registered — outcome is Failed (unclaimed)
        assert!(matches!(outcome, PlanOutcome::Failed(_)));
        // PlanCompleted is NOT dispatched by execute_single_plan
        assert_eq!(recorder.plan_completed_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_skipped_plan_dispatches_created_and_skipped_events() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let skipped_plan = test_plan("normalize", true);
        assert!(skipped_plan.is_skipped());

        // Simulate the skipped-plan dispatch sequence from
        // process_single_file_execute: PlanCreated then PlanSkipped.
        dispatch_and_log(
            &kernel,
            Event::PlanCreated(PlanCreatedEvent::new(skipped_plan.clone())),
        );
        dispatch_and_log(
            &kernel,
            Event::PlanSkipped(PlanSkippedEvent::new(
                skipped_plan.id,
                file.path.clone(),
                skipped_plan.phase_name.clone(),
                skipped_plan.skip_reason.clone().unwrap(),
            )),
        );

        // Skipped plans should NOT trigger execution events
        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_completed_count.load(Ordering::SeqCst), 0);
        // PlanCreated IS dispatched (so sqlite-store can persist the row)
        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 1);
        // PlanSkipped IS dispatched (to update status to skipped)
        assert_eq!(recorder.plan_skipped_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_discovery_and_introspection_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        // Simulate discovery events
        let discovered =
            FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 1024, Some("abc".into()));
        kernel.dispatch(Event::FileDiscovered(discovered));

        // Simulate introspection event
        let file = MediaFile::new(PathBuf::from("/tmp/a.mkv"));
        kernel.dispatch(Event::FileIntrospected(FileIntrospectedEvent::new(file)));

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.introspected_count.load(Ordering::SeqCst), 1);
    }

    /// A test plugin that records job lifecycle events.
    struct JobEventRecorder {
        started: AtomicUsize,
        progress: AtomicUsize,
        completed: AtomicUsize,
    }

    impl JobEventRecorder {
        fn new() -> Self {
            Self {
                started: AtomicUsize::new(0),
                progress: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
            }
        }
    }

    impl voom_kernel::Plugin for JobEventRecorder {
        fn name(&self) -> &'static str {
            "job-recorder"
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
                Event::JOB_STARTED | Event::JOB_PROGRESS | Event::JOB_COMPLETED
            )
        }
        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            match event {
                Event::JobStarted(_) => {
                    self.started.fetch_add(1, Ordering::SeqCst);
                }
                Event::JobProgress(_) => {
                    self.progress.fetch_add(1, Ordering::SeqCst);
                }
                Event::JobCompleted(_) => {
                    self.completed.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(None)
        }
    }

    #[test]
    fn test_event_bus_reporter_dispatches_job_events() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(JobEventRecorder::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();
        let kernel = Arc::new(kernel);

        let reporter = EventBusReporter::new(kernel);

        let job = voom_domain::job::Job::new(voom_domain::job::JobType::Process);
        let job_id = job.id;

        reporter.on_job_start(&job);
        assert_eq!(recorder.started.load(Ordering::SeqCst), 1);

        reporter.on_job_progress(job_id, 0.5, Some("halfway"));
        assert_eq!(recorder.progress.load(Ordering::SeqCst), 1);

        reporter.on_job_complete(job_id, true, None);
        assert_eq!(recorder.completed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_compute_file_date_priority_recent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent.mkv");
        std::fs::write(&path, "test").unwrap();
        // Just created -> within 7 days -> priority 10
        assert_eq!(compute_file_date_priority(&path), 10);
    }

    #[test]
    fn test_compute_file_date_priority_nonexistent() {
        let path = std::path::PathBuf::from("/nonexistent/file.mkv");
        assert_eq!(compute_file_date_priority(&path), 200);
    }

    #[test]
    fn test_event_bus_reporter_batch_methods_are_noop() {
        let kernel = Arc::new(voom_kernel::Kernel::new());
        let reporter = EventBusReporter::new(kernel);
        // These should not panic
        reporter.on_batch_start(10);
        reporter.on_batch_complete(5, 0);
    }

    // --- apply_detected_languages tests ---

    fn make_file_with_audio_tracks() -> MediaFile {
        use voom_domain::media::{Track, TrackType};
        let mut file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut t0 = Track::new(0, TrackType::AudioMain, "aac".into());
        t0.language = "und".to_string();
        let mut t1 = Track::new(1, TrackType::AudioAlternate, "ac3".into());
        t1.language = "fre".to_string();
        file.tracks = vec![t0, t1];
        file
    }

    #[test]
    fn test_apply_detected_und_to_eng() {
        let mut file = make_file_with_audio_tracks();
        file.plugin_metadata.insert(
            AUDIO_LANGUAGE_DETECTOR_PLUGIN.to_string(),
            serde_json::json!({
                "detections": [{
                    "track_index": 0,
                    "detected_language": "eng",
                    "confidence": 0.95,
                }]
            }),
        );
        apply_detected_languages(&mut file);
        assert_eq!(file.tracks[0].language, "eng");
        // Track 1 unchanged (no detection for it).
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn test_apply_detected_overwrite_mismatch() {
        let mut file = make_file_with_audio_tracks();
        file.plugin_metadata.insert(
            AUDIO_LANGUAGE_DETECTOR_PLUGIN.to_string(),
            serde_json::json!({
                "detections": [{
                    "track_index": 1,
                    "detected_language": "eng",
                    "confidence": 0.92,
                }]
            }),
        );
        apply_detected_languages(&mut file);
        // Track 1 was "fre" but detection says "eng" — overwritten.
        assert_eq!(file.tracks[1].language, "eng");
    }

    #[test]
    fn test_apply_detected_zxx() {
        let mut file = make_file_with_audio_tracks();
        file.plugin_metadata.insert(
            AUDIO_LANGUAGE_DETECTOR_PLUGIN.to_string(),
            serde_json::json!({
                "detections": [{
                    "track_index": 0,
                    "detected_language": "zxx",
                    "confidence": 0.98,
                }]
            }),
        );
        apply_detected_languages(&mut file);
        assert_eq!(file.tracks[0].language, "zxx");
    }

    #[test]
    fn test_apply_detected_mul() {
        let mut file = make_file_with_audio_tracks();
        file.plugin_metadata.insert(
            AUDIO_LANGUAGE_DETECTOR_PLUGIN.to_string(),
            serde_json::json!({
                "detections": [{
                    "track_index": 0,
                    "detected_language": "mul",
                    "confidence": 0.6,
                }]
            }),
        );
        apply_detected_languages(&mut file);
        assert_eq!(file.tracks[0].language, "mul");
    }

    #[test]
    fn test_apply_detected_no_metadata() {
        let mut file = make_file_with_audio_tracks();
        apply_detected_languages(&mut file);
        // No crash, tracks unchanged.
        assert_eq!(file.tracks[0].language, "und");
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn test_apply_detected_nonexistent_track() {
        let mut file = make_file_with_audio_tracks();
        file.plugin_metadata.insert(
            AUDIO_LANGUAGE_DETECTOR_PLUGIN.to_string(),
            serde_json::json!({
                "detections": [{
                    "track_index": 99,
                    "detected_language": "eng",
                    "confidence": 0.95,
                }]
            }),
        );
        // No panic.
        apply_detected_languages(&mut file);
        assert_eq!(file.tracks[0].language, "und");
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn test_check_disk_space_passes_with_enough_space() {
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;

        let plan = test_plan("normalize", false);

        let kernel = Arc::new(voom_kernel::Kernel::new());
        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let ctx = fixture.make_ctx(kernel, store);

        // Should return false (enough space)
        assert!(!check_disk_space(&plan, &file, &ctx));
    }

    #[test]
    fn test_check_disk_space_dispatches_plan_failed_without_plan_created() {
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        // Set size to u64::MAX / 2 so estimated required space exceeds any real disk.
        file.size = u64::MAX / 2;

        let plan = test_plan("normalize", false);

        let recorder = Arc::new(PlanRecordingPlugin::new());
        let mut kernel = voom_kernel::Kernel::new();
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let ctx = fixture.make_ctx(Arc::new(kernel), store);

        // Should return true (insufficient space).
        assert!(check_disk_space(&plan, &file, &ctx));
        assert_eq!(
            recorder.plan_created_count.load(Ordering::SeqCst),
            0,
            "PlanCreated must NOT be dispatched by check_disk_space"
        );
        assert_eq!(
            recorder.plan_failed_count.load(Ordering::SeqCst),
            1,
            "PlanFailed must fire"
        );
    }

    #[tokio::test]
    async fn handle_plan_success_preserves_lineage_on_container_conversion() {
        use voom_domain::media::{Container, MediaFile};
        use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

        let fixture =
            TestFixture::with_policy(r#"policy "test" { phase convert { container mkv } }"#);
        let mp4_path = fixture.dir_path().join("movie.mp4");
        let mkv_path = fixture.dir_path().join("movie.mkv");
        // The executor renames source-to-target; pre-write the target so
        // `resolve_post_execution_path` accepts the new extension.
        std::fs::write(&mkv_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(mp4_path);
        file.container = Container::Mp4;
        file.size = 1024;
        file.content_hash = Some("oldhash".to_string());
        let original_id = file.id;

        let mut plan = Plan::new(file.clone(), "containerize", "convert");
        plan.actions = vec![PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container {
                container: Container::Mkv,
            },
            "Convert to mkv",
        )];

        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_sqlite_store::store::SqliteStore::in_memory().unwrap());
        store.upsert_file(&file).unwrap();
        assert_eq!(store.count_files(&Default::default()).unwrap(), 1);

        let kernel = Arc::new(voom_kernel::Kernel::new());
        let ctx = fixture.make_ctx(kernel, store.clone());

        let _ =
            pipeline::handle_plan_success(plan, &file, "mkvtoolnix-executor", 0, false, &ctx).await;

        assert_eq!(
            store.count_files(&Default::default()).unwrap(),
            1,
            "ConvertContainer must not introduce a duplicate files row"
        );
        let surviving = store
            .file_by_path(&mkv_path)
            .unwrap()
            .expect("row must follow the file to its post-conversion path");
        assert_eq!(
            surviving.id, original_id,
            "lineage UUID must be preserved across container conversion"
        );
        assert!(
            store
                .file_by_path(&fixture.dir_path().join("movie.mp4"))
                .unwrap()
                .is_none(),
            "the original .mp4 path must no longer resolve to a row",
        );
    }

    #[test]
    fn record_file_transition_makes_history_lookup_work_for_old_and_new_paths() {
        // Bypass re-introspection (depends on ffprobe being on PATH) and
        // exercise `record_file_transition` directly.
        use std::path::PathBuf;
        use voom_domain::media::{Container, MediaFile};

        let mut old_file = MediaFile::new(PathBuf::from("/lib/movie.mp4"));
        old_file.container = Container::Mp4;
        old_file.size = 1024;
        old_file.content_hash = Some("old".to_string());

        let mut new_file = MediaFile::new(PathBuf::from("/lib/movie.mkv"));
        new_file.container = Container::Mkv;
        new_file.size = 900;
        new_file.content_hash = Some("new".to_string());

        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_sqlite_store::store::SqliteStore::in_memory().unwrap());
        let kernel = Arc::new(voom_kernel::Kernel::new());
        let fixture = TestFixture::with_policy(r#"policy "p" { phase convert { container mkv } }"#);
        let ctx = fixture.make_ctx(kernel, store.clone());

        super::transitions::record_file_transition(&super::transitions::FileTransitionContext {
            old_file: &old_file,
            new_file: &new_file,
            executor: "mkvtoolnix-executor",
            elapsed_ms: 0,
            actions_taken: 1,
            tracks_modified: 0,
            policy_name: "containerize",
            phase_name: "convert",
            plan_id: uuid::Uuid::new_v4(),
            ctx: &ctx,
        });

        let by_new = store.transitions_for_path(&new_file.path).unwrap();
        assert_eq!(
            by_new.len(),
            1,
            "new .mkv path must locate the conversion transition"
        );
        let by_old = store.transitions_for_path(&old_file.path).unwrap();
        assert_eq!(
            by_old.len(),
            1,
            "old .mp4 path must locate the conversion transition via from_path"
        );
        assert_eq!(by_old[0].id, by_new[0].id);
        assert_eq!(
            by_old[0].from_path.as_deref(),
            Some(old_file.path.as_path())
        );
    }

    #[test]
    fn test_check_size_increase_dispatches_plan_failed_without_plan_created() {
        // Write a file with 2048 bytes so the size-increase check fires
        // when the MediaFile reports size = 1024 (smaller than actual).
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 2048]).unwrap();

        let mut file = MediaFile::new(file_path);
        // Report size smaller than actual so the safeguard triggers.
        file.size = 1024;

        let plan = test_plan("normalize", false);

        let recorder = Arc::new(PlanRecordingPlugin::new());
        let mut kernel = voom_kernel::Kernel::new();
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let capabilities = voom_domain::CapabilityMap::new();
        let counters = RunCounters::new();
        let token = CancellationToken::new();
        let resolver = PolicyResolver::from_single(
            voom_dsl::compile_policy(r#"policy "test" { phase normalize { container mkv } }"#)
                .unwrap(),
            dir.path(),
        );
        let ctx = ProcessContext {
            resolver: &resolver,
            kernel: Arc::new(kernel),
            store,
            dry_run: false,
            plan_only: false,
            flag_size_increase: true,
            flag_duration_shrink: false,
            force_rescan: false,
            token: &token,
            ffprobe_path: None,
            capabilities: &capabilities,
            counters: &counters,
        };

        // Should return true (size increased).
        assert!(check_size_increase(&plan, &file, &ctx));
        // PlanCreated is NOT dispatched here — execute_single_plan (the caller)
        // is responsible for that dispatch. The PlanCreated/PlanFailed pairing
        // is satisfied by the earlier PlanCreated from execute_single_plan.
        assert_eq!(
            recorder.plan_created_count.load(Ordering::SeqCst),
            0,
            "PlanCreated must NOT be dispatched by check_size_increase"
        );
        assert_eq!(
            recorder.plan_failed_count.load(Ordering::SeqCst),
            1,
            "PlanFailed must fire"
        );
    }

    #[tokio::test]
    async fn test_check_duration_shrink_flag_disabled_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;
        file.duration = 100.0;

        let plan = test_plan("normalize", false);

        let kernel = voom_kernel::Kernel::new();
        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let capabilities = voom_domain::CapabilityMap::new();
        let counters = RunCounters::new();
        let token = CancellationToken::new();
        let resolver = PolicyResolver::from_single(
            voom_dsl::compile_policy(r#"policy "test" { phase normalize { container mkv } }"#)
                .unwrap(),
            dir.path(),
        );
        let ctx = ProcessContext {
            resolver: &resolver,
            kernel: Arc::new(kernel),
            store,
            dry_run: false,
            plan_only: false,
            flag_size_increase: false,
            flag_duration_shrink: false,
            force_rescan: false,
            token: &token,
            ffprobe_path: None,
            capabilities: &capabilities,
            counters: &counters,
        };

        // Flag disabled — must early-return false without invoking ffprobe.
        assert!(!check_duration_shrink(&plan, &file, &ctx).await);
    }

    #[tokio::test]
    async fn test_check_duration_shrink_zero_input_duration_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;
        file.duration = 0.0;

        let plan = test_plan("normalize", false);

        let kernel = voom_kernel::Kernel::new();
        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let capabilities = voom_domain::CapabilityMap::new();
        let counters = RunCounters::new();
        let token = CancellationToken::new();
        let resolver = PolicyResolver::from_single(
            voom_dsl::compile_policy(r#"policy "test" { phase normalize { container mkv } }"#)
                .unwrap(),
            dir.path(),
        );
        let ctx = ProcessContext {
            resolver: &resolver,
            kernel: Arc::new(kernel),
            store,
            dry_run: false,
            plan_only: false,
            flag_size_increase: false,
            flag_duration_shrink: true,
            force_rescan: false,
            token: &token,
            ffprobe_path: None,
            capabilities: &capabilities,
            counters: &counters,
        };

        // Input duration is 0.0 — can't compute a percentage; must early-return false.
        assert!(!check_duration_shrink(&plan, &file, &ctx).await);
    }

    #[tokio::test]
    async fn test_check_duration_shrink_cancelled_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;
        file.duration = 100.0;

        let plan = test_plan("normalize", false);

        let kernel = voom_kernel::Kernel::new();
        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_domain::test_support::InMemoryStore::new());
        let capabilities = voom_domain::CapabilityMap::new();
        let counters = RunCounters::new();
        let token = CancellationToken::new();
        token.cancel();
        let resolver = PolicyResolver::from_single(
            voom_dsl::compile_policy(r#"policy "test" { phase normalize { container mkv } }"#)
                .unwrap(),
            dir.path(),
        );
        let ctx = ProcessContext {
            resolver: &resolver,
            kernel: Arc::new(kernel),
            store,
            dry_run: false,
            plan_only: false,
            flag_size_increase: false,
            flag_duration_shrink: true,
            force_rescan: false,
            token: &token,
            ffprobe_path: None,
            capabilities: &capabilities,
            counters: &counters,
        };

        // Token cancelled — must early-return false without launching ffprobe.
        assert!(!check_duration_shrink(&plan, &file, &ctx).await);
    }
}
