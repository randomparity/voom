use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use anyhow::{Context, Result};
use console::style;
use parking_lot::Mutex;

use tokio_util::sync::CancellationToken;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use crate::config;
use crate::paths::resolve_paths;
use crate::policy_map::{PolicyMatch, PolicyResolver};
use crate::progress::{BatchProgress, DiscoveryProgress};
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{
    Event, IntrospectCompleteEvent, JobCompletedEvent, JobProgressEvent, JobStartedEvent,
    PlanCompletedEvent, PlanCreatedEvent, PlanExecutingEvent, PlanFailedEvent, PlanSkippedEvent,
};
use voom_domain::plan::OperationType;
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
    let capabilities = Arc::new(collector.snapshot());

    let paths = resolve_paths(&args.paths)?;

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
                .and_then(|v| v.as_bool())
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

    let mut events = discover_files(&paths, &args, &kernel, quiet)?;
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

    let (pool, effective_workers) = create_worker_pool(job_queue, &args, token.clone())?;

    let reporter = build_reporter(&events, effective_workers, plan_only, quiet, kernel.clone());

    let items = build_work_items(&events, args.priority_by_date);
    let all_phase_names = resolver.all_phase_names();
    let resolver = Arc::new(resolver);
    let flag_size_increase = args.flag_size_increase;

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
        kernel_for_completion.dispatch(Event::IntrospectComplete(IntrospectCompleteEvent::new(
            pool.completed_count(),
        )));
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

        let total_errors = ctx.pool.failed_count()
            + ctx
                .counters
                .phase_stats
                .lock()
                .values()
                .map(|ps| ps.failed)
                .sum::<u64>();
        // Deduplicate: pool counts files, phase_stats counts phases. Use phase errors.
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
        // Suppress unused binding warning
        let _ = total_errors;
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

        let progress_clone = progress.clone();
        let cum_disc = cumulative_discovered.clone();
        let proc_base = processing_base.clone();
        let pre_scan_discovered = cumulative_discovered.load(std::sync::atomic::Ordering::Relaxed);

        options.on_progress = Some(Box::new(move |p| match p {
            voom_discovery::ScanProgress::Discovered { count: _, path } => {
                let cumulative = cum_disc.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress_clone.on_discovered(cumulative as usize, &path);
            }
            voom_discovery::ScanProgress::Processing {
                current,
                total,
                path,
            } => {
                let base = proc_base.load(std::sync::atomic::Ordering::Relaxed) as usize;
                let action = if hash_files { "Hashing" } else { "Scanning" };
                progress_clone.on_processing(base + current, base + total, &path, action);
            }
            voom_discovery::ScanProgress::OrphanedTempFiles { .. } => {}
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
        let results = kernel.dispatch(Event::FileDiscovered(event.clone()));
        log_plugin_errors(&results);
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

/// Log any `plugin.error` events produced during event dispatch so
/// CLI users see plugin failures rather than silent swallowing.
fn log_plugin_errors(results: &[voom_domain::events::EventResult]) {
    for result in results {
        for produced in &result.produced_events {
            if let Event::PluginError(err) = produced {
                tracing::warn!(
                    plugin = %err.plugin_name,
                    event = %err.event_type,
                    error = %err.error,
                    "plugin error during dispatch"
                );
            }
        }
    }
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
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return 200,
    };
    let modified = match metadata.modified() {
        Ok(t) => t,
        Err(_) => return 200,
    };
    let elapsed = match std::time::SystemTime::now().duration_since(modified) {
        Ok(d) => d,
        Err(_) => return 200,
    };
    const SECS_PER_DAY: u64 = 86_400;
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
) -> Result<(WorkerPool, usize)> {
    let mut config = WorkerPoolConfig::default();
    config.max_workers = args.workers;
    config.worker_prefix = "voom".to_string();
    let effective_workers = config.effective_workers();

    let pool = WorkerPool::new(queue, config, token);

    Ok((pool, effective_workers))
}

/// Extract and deserialize the job payload from a process job.
fn parse_job_payload(job: &voom_domain::job::Job) -> anyhow::Result<DiscoveredFilePayload> {
    let raw_payload = job
        .payload
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing payload"))?;
    serde_json::from_value(raw_payload.clone()).context("invalid payload")
}

#[derive(Debug, Default)]
struct PhaseStats {
    completed: u64,
    skipped: u64,
    failed: u64,
    skip_reasons: HashMap<String, u64>,
}

type PhaseStatsMap = Arc<Mutex<HashMap<String, PhaseStats>>>;

fn record_phase_stat(stats: &PhaseStatsMap, phase_name: &str, outcome: PhaseOutcomeKind) {
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

enum PhaseOutcomeKind {
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
struct RunCounters {
    modified_count: Arc<AtomicU64>,
    backup_bytes: Arc<AtomicU64>,
    phase_stats: PhaseStatsMap,
    plan_collector: Arc<Mutex<Vec<serde_json::Value>>>,
    session_id: uuid::Uuid,
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
struct ProcessContext<'a> {
    resolver: &'a PolicyResolver,
    kernel: Arc<voom_kernel::Kernel>,
    store: Arc<dyn voom_domain::storage::StorageTrait>,
    dry_run: bool,
    plan_only: bool,
    flag_size_increase: bool,
    token: &'a CancellationToken,
    ffprobe_path: Option<&'a str>,
    capabilities: &'a voom_domain::CapabilityMap,
    counters: &'a RunCounters,
}

/// Process a single file: introspect, orchestrate, and (unless dry-run) execute plans.
///
/// For dry-run/plan-only mode, all phases are evaluated up front against the
/// original file state (matching existing behavior).
///
/// For real execution, phases are evaluated one at a time. After each phase
/// executes, the file is re-introspected so the next phase sees the current
/// on-disk state (updated path, tracks, container, etc.).
async fn process_single_file(
    job: voom_domain::job::Job,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let payload = parse_job_payload(&job).map_err(|e| format!("job payload: {e}"))?;

    let path = std::path::PathBuf::from(&payload.path);

    let mut file = crate::introspect::introspect_file(
        path,
        payload.size,
        payload.content_hash,
        &ctx.kernel,
        ctx.ffprobe_path,
    )
    .await
    .map_err(|e| format!("introspect {}: {e}", payload.path))?;

    // Prior runs may have written plugin_metadata that the current
    // introspection didn't reproduce; merge so the evaluator sees both.
    if let Ok(Some(stored)) = ctx.store.file_by_path(&file.path) {
        for (k, v) in stored.plugin_metadata {
            file.plugin_metadata.entry(k).or_insert(v);
        }
    }

    apply_detected_languages(&mut file);

    // Resolve which policy applies to this file.
    let matched = ctx
        .resolver
        .resolve(&file.path)
        .map_err(|e| format!("policy resolution: {e}"))?;
    let compiled = match matched {
        PolicyMatch::Policy(compiled, _name) => compiled,
        PolicyMatch::Skip => {
            return Ok(Some(serde_json::json!({
                "path": file.path.display().to_string(),
                "skipped": true,
                "reason": "excluded by policy map",
            })));
        }
    };

    if ctx.dry_run {
        process_single_file_dry_run(&file, compiled, ctx)
    } else {
        process_single_file_execute(&file, compiled, ctx).await
    }
}

/// Dry-run / plan-only: evaluate all phases up front against the original file.
fn process_single_file_dry_run(
    file: &voom_domain::media::MediaFile,
    compiled: &voom_dsl::CompiledPolicy,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let mut result = orchestrate_plans(compiled, file, ctx.capabilities);
    annotate_disk_space_violations(&mut result, file);

    collect_safeguard_violations(file, &result, ctx);

    let needs_exec = voom_phase_orchestrator::PhaseOrchestrator::needs_execution(&result);
    if needs_exec {
        ctx.counters
            .modified_count
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    for plan in &result.plans {
        if plan.is_skipped() {
            let reason = plan
                .skip_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            record_phase_stat(
                &ctx.counters.phase_stats,
                &plan.phase_name,
                PhaseOutcomeKind::Skipped(reason),
            );
        } else if !plan.is_empty() {
            record_phase_stat(
                &ctx.counters.phase_stats,
                &plan.phase_name,
                PhaseOutcomeKind::Completed,
            );
        }
    }

    if ctx.plan_only {
        let plans_json: Vec<serde_json::Value> = result
            .plans
            .iter()
            .filter(|p| !p.is_empty() && !p.is_skipped())
            .map(|p| serde_json::to_value(p).expect("Plan implements Serialize"))
            .collect();
        if !plans_json.is_empty() {
            ctx.counters.plan_collector.lock().extend(plans_json);
        }
    }

    let plan_summaries: Vec<serde_json::Value> = result
        .plans
        .iter()
        .map(|p| {
            let mut summary = serde_json::json!({
                "phase": p.phase_name,
                "actions": p.actions.len(),
                "skipped": p.is_skipped(),
            });
            if !p.safeguard_violations.is_empty() {
                summary["safeguard_violations"] = serde_json::json!(p.safeguard_violations);
            }
            summary
        })
        .collect();

    Ok(Some(serde_json::json!({
        "path": file.path.display().to_string(),
        "needs_execution": needs_exec,
        "plans": plan_summaries,
    })))
}

/// Real execution: evaluate → execute → re-introspect per phase.
///
/// After each phase executes successfully, the file is re-introspected so
/// that subsequent phases see the updated path, container, and tracks.
async fn process_single_file_execute(
    file: &voom_domain::media::MediaFile,
    compiled: &voom_dsl::CompiledPolicy,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let file_path_str = file.path.display().to_string();
    let keep_backups = compiled.config.keep_backups;

    // Verify file hasn't changed since introspection (TOCTOU guard)
    if let Some(skip_json) = check_file_hash(file).await {
        return Ok(Some(skip_json));
    }

    let evaluator = voom_policy_evaluator::PolicyEvaluator::new();
    let mut current_file = file.clone();
    let mut phase_outcomes: HashMap<String, voom_policy_evaluator::EvaluationOutcome> =
        HashMap::new();
    let mut any_executed = false;
    let mut modified_counted = false;
    let mut plans_evaluated: usize = 0;

    for phase_name in &compiled.phase_order {
        if ctx.token.is_cancelled() {
            break;
        }

        let plan = match evaluator.evaluate_single_phase(
            phase_name,
            compiled,
            &current_file,
            &phase_outcomes,
            ctx.capabilities,
        ) {
            Some(p) => p,
            None => continue,
        };

        plans_evaluated += 1;

        dispatch_safeguard_violations(&plan, &current_file, ctx);

        // Handle skipped plans
        if let Some(reason) = &plan.skip_reason {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::Skipped,
            );
            dispatch_skipped_plan(&plan, &current_file, reason, ctx);
            continue;
        }

        // Empty plans need no execution
        if plan.is_empty() {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::Executed { modified: false },
            );
            continue;
        }

        // Pre-execution safeguard: check disk space
        // Note: check_disk_space dispatches only PlanFailed (not
        // PlanCreated, which would trigger executors) and records
        // PhaseOutcomeKind::Failed for stats. This insert updates
        // the dependency-resolution map to block downstream run_if gates.
        if check_disk_space(&plan, &current_file, ctx) {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
            );
            continue;
        }

        // Execute this plan
        let plan_clone = plan.clone();
        let file_clone = current_file.clone();
        let kernel_clone = ctx.kernel.clone();
        let start = std::time::Instant::now();
        let exec_outcome = tokio::task::spawn_blocking(move || {
            execute_single_plan(&plan_clone, &file_clone, &kernel_clone)
        })
        .await
        .map_err(|e| format!("plan execution join error: {e}"))?;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match exec_outcome {
            PlanOutcome::Success { executor } => {
                // Post-execution safeguard: check size increase
                // Note: check_size_increase dispatches PlanFailed (PlanCreated was
                // already dispatched by execute_single_plan) and records
                // PhaseOutcomeKind::Failed for stats. This insert updates the
                // dependency-resolution map.
                if check_size_increase(&plan, &current_file, ctx) {
                    phase_outcomes.insert(
                        phase_name.clone(),
                        voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
                    );
                    continue;
                }
                any_executed = true;
                if !modified_counted {
                    ctx.counters
                        .modified_count
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    modified_counted = true;
                }
                phase_outcomes.insert(
                    phase_name.clone(),
                    voom_policy_evaluator::EvaluationOutcome::Executed { modified: true },
                );
                current_file = handle_plan_success(
                    plan,
                    &current_file,
                    &executor,
                    elapsed_ms,
                    keep_backups,
                    ctx,
                )
                .await;
            }
            PlanOutcome::Failed(failed) => {
                let plan_id = plan.id;
                let policy_name = plan.policy_name.clone();
                let phase_name_owned = plan.phase_name.clone();
                let executor = failed.plugin_name.clone().unwrap_or_default();
                let error_msg = failed.error.clone();
                dispatch_plan_failure(failed, &phase_name_owned, ctx);
                record_failure_transition(
                    &current_file,
                    plan_id,
                    &executor,
                    &policy_name,
                    &phase_name_owned,
                    Some(&error_msg),
                    ctx,
                );
                phase_outcomes.insert(
                    phase_name_owned.clone(),
                    voom_policy_evaluator::EvaluationOutcome::ExecutionFailed,
                );
                // Downstream phases still evaluate; run_if gates block
                // them via ExecutionFailed in phase_outcomes.
            }
        }
    }

    Ok(Some(serde_json::json!({
        "path": file_path_str,
        "needs_execution": any_executed,
        "plans_evaluated": plans_evaluated,
    })))
}

/// Dispatch safeguard violations for a plan through the event bus.
fn dispatch_safeguard_violations(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ctx: &ProcessContext<'_>,
) {
    if plan.safeguard_violations.is_empty() {
        return;
    }
    let mut tagged = file.clone();
    tagged.plugin_metadata.insert(
        "safeguard_violations".to_string(),
        serde_json::json!(&plan.safeguard_violations),
    );
    let r = ctx.kernel.dispatch(Event::FileIntrospected(
        voom_domain::events::FileIntrospectedEvent::new(tagged),
    ));
    log_plugin_errors(&r);
}

/// Dispatch events for a skipped plan: PlanCreated then PlanSkipped.
fn dispatch_skipped_plan(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    reason: &str,
    ctx: &ProcessContext<'_>,
) {
    let r = ctx
        .kernel
        .dispatch(Event::PlanCreated(PlanCreatedEvent::new(plan.clone())));
    log_plugin_errors(&r);
    let r = ctx
        .kernel
        .dispatch(Event::PlanSkipped(PlanSkippedEvent::new(
            plan.id,
            file.path.clone(),
            plan.phase_name.clone(),
            reason.to_string(),
        )));
    log_plugin_errors(&r);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Skipped(reason.to_string()),
    );
}

/// Dispatch a PlanFailed event and record the phase stat.
fn dispatch_plan_failure(failed: PlanFailedEvent, phase_name: &str, ctx: &ProcessContext<'_>) {
    let r = ctx.kernel.dispatch(Event::PlanFailed(failed));
    log_plugin_errors(&r);
    record_phase_stat(
        &ctx.counters.phase_stats,
        phase_name,
        PhaseOutcomeKind::Failed,
    );
}

/// Record a failure transition in the store for a plan that did not succeed.
///
/// The file is unchanged on failure, so `to_size = from_size` and `to_hash =
/// from_hash`. The `executor` argument should be the executor plugin name, or
/// an empty string when no executor was involved (e.g. size-increase abort).
fn record_failure_transition(
    file: &voom_domain::media::MediaFile,
    plan_id: uuid::Uuid,
    executor: &str,
    policy_name: &str,
    phase_name: &str,
    error_message: Option<&str>,
    ctx: &ProcessContext<'_>,
) {
    let to_hash = file.content_hash.clone().unwrap_or_default();
    let mut transition = voom_domain::FileTransition::new(
        file.id,
        file.path.clone(),
        to_hash,
        file.size,
        voom_domain::TransitionSource::Voom,
    )
    .with_from(file.content_hash.clone(), Some(file.size))
    .with_detail(executor)
    .with_plan_id(plan_id)
    .with_processing(
        0,
        0,
        0,
        voom_domain::ProcessingOutcome::Failure,
        policy_name,
        phase_name,
    )
    .with_session_id(ctx.counters.session_id);

    if let Some(msg) = error_message {
        transition = transition.with_error_message(msg);
    }

    if let Err(e) = ctx.store.record_transition(&transition) {
        tracing::warn!(error = %e, "failed to record failure transition");
    }
}

/// Check if the output file grew larger than the original.
///
/// Returns `true` if the size increased and the phase should be skipped
/// (`PlanFailed` is dispatched and the failure is recorded; `PlanCreated`
/// was already dispatched by the caller). Returns `false` to proceed normally.
fn check_size_increase(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ctx: &ProcessContext<'_>,
) -> bool {
    if !ctx.flag_size_increase {
        return false;
    }
    let check_path = resolve_post_execution_path(file, std::slice::from_ref(plan));
    let Ok(meta) = std::fs::metadata(&check_path) else {
        return false;
    };
    let new_size = meta.len();
    if new_size <= file.size || file.size == 0 {
        return false;
    }
    tracing::warn!(
        path = %check_path.display(),
        before = file.size,
        after = new_size,
        "output larger than original, restoring"
    );
    if check_path != file.path {
        if let Err(e) = std::fs::remove_file(&check_path) {
            tracing::warn!(
                path = %check_path.display(),
                error = %e,
                "failed to remove converted output"
            );
        }
    }
    // Note: PlanCreated was already dispatched by execute_single_plan
    // (the caller). We only dispatch PlanFailed here — the
    // PlanCreated/PlanFailed pairing is satisfied by the earlier
    // PlanCreated.
    let r = ctx.kernel.dispatch(Event::PlanFailed(PlanFailedEvent::new(
        plan.id,
        file.path.clone(),
        plan.phase_name.clone(),
        format!("output grew from {} to {} bytes", file.size, new_size,),
    )));
    log_plugin_errors(&r);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Failed,
    );
    let err_msg = format!("output grew from {} to {} bytes", file.size, new_size);
    record_failure_transition(
        file,
        plan.id,
        "",
        &plan.policy_name,
        &plan.phase_name,
        Some(&err_msg),
        ctx,
    );
    true
}

/// Check whether sufficient disk space is available before executing a plan.
///
/// Returns `true` if space is insufficient and the phase should be skipped
/// (`PlanFailed` is dispatched and the failure is recorded; `PlanCreated`
/// is intentionally not dispatched to avoid triggering executors).
/// Returns `false` to proceed normally.
fn check_disk_space(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ctx: &ProcessContext<'_>,
) -> bool {
    let check_path = file.path.parent().unwrap_or(std::path::Path::new("/"));

    let available = match voom_domain::utils::disk::available_space(check_path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %file.path.display(),
                error = %e,
                "disk space check failed, proceeding anyway"
            );
            return false;
        }
    };

    let required = voom_domain::utils::disk::estimate_required_space(plan, file.size);

    if available >= required {
        return false;
    }

    let message = format!(
        "insufficient disk space: need {} but only {} available on {}",
        format_size(required),
        format_size(available),
        check_path.display(),
    );

    tracing::warn!(
        path = %file.path.display(),
        phase = %plan.phase_name,
        required,
        available,
        "{message}"
    );

    // Note: we intentionally do NOT dispatch PlanCreated here.
    // PlanCreated triggers executor plugins (mkvtoolnix, ffmpeg)
    // which would execute the plan before we can abort it.
    // sqlite-store's update_plan_status is a no-op for unknown
    // plan IDs, so the missing PlanCreated is harmless.
    let r = ctx.kernel.dispatch(Event::PlanFailed(PlanFailedEvent::new(
        plan.id,
        file.path.clone(),
        plan.phase_name.clone(),
        &message,
    )));
    log_plugin_errors(&r);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Failed,
    );
    record_failure_transition(
        file,
        plan.id,
        "",
        &plan.policy_name,
        &plan.phase_name,
        Some(&message),
        ctx,
    );
    true
}

/// Annotate plans with `DiskSpaceLow` safeguard violations for dry-run reporting.
///
/// Unlike real execution (which skips the plan entirely), dry-run mode attaches
/// the violation to the plan so it appears in `--plan-only` JSON output.
fn annotate_disk_space_violations(
    result: &mut voom_phase_orchestrator::OrchestrationResult,
    file: &voom_domain::media::MediaFile,
) {
    let check_path = file.path.parent().unwrap_or(std::path::Path::new("/"));

    let available = match voom_domain::utils::disk::available_space(check_path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %file.path.display(),
                error = %e,
                "disk space check failed during dry-run, skipping annotation"
            );
            return;
        }
    };

    for plan in &mut result.plans {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }
        let required = voom_domain::utils::disk::estimate_required_space(plan, file.size);
        if available < required {
            let message = format!(
                "insufficient disk space: need {} but only {} available on {}",
                format_size(required),
                format_size(available),
                check_path.display(),
            );
            plan.safeguard_violations
                .push(voom_domain::SafeguardViolation::new(
                    voom_domain::SafeguardKind::DiskSpaceLow,
                    message,
                    &plan.phase_name,
                ));
        }
    }
}

/// Handle a successfully executed plan: dispatch completion, re-introspect,
/// and record the file transition.
async fn handle_plan_success(
    plan: voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    executor: &str,
    elapsed_ms: u64,
    keep_backups: bool,
    ctx: &ProcessContext<'_>,
) -> voom_domain::media::MediaFile {
    if keep_backups {
        ctx.counters
            .backup_bytes
            .fetch_add(file.size, AtomicOrdering::Relaxed);
        tracing::info!(
            path = %file.path.display(),
            phase = %plan.phase_name,
            "keeping backup per policy"
        );
    }
    let r = ctx
        .kernel
        .dispatch(Event::PlanCompleted(PlanCompletedEvent::new(
            plan.id,
            file.path.clone(),
            plan.phase_name.clone(),
            plan.actions.len(),
            keep_backups,
        )));
    log_plugin_errors(&r);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Completed,
    );

    let plan_id = plan.id;
    let actions_taken = plan.actions.len() as u32;
    let tracks_modified = plan
        .actions
        .iter()
        .filter(|a| a.track_index.is_some())
        .count() as u32;
    let policy_name = plan.policy_name.clone();
    let phase_name = plan.phase_name.clone();

    let new_file = reintrospect_file(file, &[plan], ctx).await;

    record_file_transition(
        file,
        &new_file,
        executor,
        elapsed_ms,
        actions_taken,
        tracks_modified,
        &policy_name,
        &phase_name,
        plan_id,
        ctx,
    );

    new_file
}

/// Record a file transition in the store if the content hash changed.
#[allow(clippy::too_many_arguments)]
fn record_file_transition(
    old_file: &voom_domain::media::MediaFile,
    new_file: &voom_domain::media::MediaFile,
    executor: &str,
    elapsed_ms: u64,
    actions_taken: u32,
    tracks_modified: u32,
    policy_name: &str,
    phase_name: &str,
    plan_id: uuid::Uuid,
    ctx: &ProcessContext<'_>,
) {
    if new_file.content_hash == old_file.content_hash {
        return;
    }
    let transition = voom_domain::FileTransition::new(
        old_file.id,
        new_file.path.clone(),
        new_file.content_hash.clone().unwrap_or_default(),
        new_file.size,
        voom_domain::TransitionSource::Voom,
    )
    .with_from(old_file.content_hash.clone(), Some(old_file.size))
    .with_detail(executor)
    .with_plan_id(plan_id)
    .with_processing(
        elapsed_ms,
        actions_taken,
        tracks_modified,
        voom_domain::ProcessingOutcome::Success,
        policy_name,
        phase_name,
    )
    .with_metadata_snapshot(voom_domain::MetadataSnapshot::from_media_file(new_file))
    .with_session_id(ctx.counters.session_id);

    if let Err(e) = ctx.store.record_transition(&transition) {
        tracing::warn!(error = %e, "failed to record transition");
    }

    if let Some(ref hash) = new_file.content_hash {
        if let Err(e) = ctx.store.update_expected_hash(&old_file.id, hash) {
            tracing::warn!(error = %e, "failed to update expected_hash");
        }
    }
}

/// Check file hash for TOCTOU guard. Returns Some(json) if file should be skipped.
async fn check_file_hash(file: &voom_domain::media::MediaFile) -> Option<serde_json::Value> {
    let file_path_str = file.path.display().to_string();
    let Some(ref stored_hash) = file.content_hash else {
        tracing::debug!(path = %file.path.display(),
            "no content_hash available, skipping TOCTOU check");
        return None;
    };

    let hash_path = file.path.clone();
    let hash_result =
        tokio::task::spawn_blocking(move || voom_discovery::hash_file(&hash_path)).await;
    match hash_result {
        Ok(Ok(current_hash)) if &current_hash != stored_hash => {
            tracing::warn!(path = %file.path.display(), "file changed since introspection, skipping");
            Some(serde_json::json!({
                "path": file_path_str,
                "skipped": true,
                "reason": "file changed since introspection",
            }))
        }
        Ok(Err(e)) => {
            tracing::warn!(path = %file.path.display(), error = %e, "hash check failed, skipping");
            Some(serde_json::json!({
                "path": file_path_str,
                "skipped": true,
                "reason": format!("hash check failed: {e}"),
            }))
        }
        Err(e) => {
            tracing::warn!(path = %file.path.display(), error = %e, "hash task panicked, skipping");
            Some(serde_json::json!({
                "path": file_path_str,
                "skipped": true,
                "reason": format!("hash task panicked: {e}"),
            }))
        }
        _ => None, // hash matches, proceed
    }
}

/// Re-introspect the file after a phase executes, returning the updated
/// `MediaFile` with the current on-disk path, tracks, and metadata.
async fn reintrospect_file(
    file: &voom_domain::media::MediaFile,
    plans: &[voom_domain::plan::Plan],
    ctx: &ProcessContext<'_>,
) -> voom_domain::media::MediaFile {
    let current_path = resolve_post_execution_path(file, plans);
    if !current_path.exists() {
        tracing::warn!(
            path = %current_path.display(),
            "file not found after execution, using previous state"
        );
        return file.clone();
    }

    let p = current_path.clone();
    let file_size = file.size;
    let (size, hash) = tokio::task::spawn_blocking(move || {
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(file_size);
        let hash = match voom_discovery::hash_file(&p) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e,
                    "could not hash file after execution");
                None
            }
        };
        (size, hash)
    })
    .await
    .unwrap_or((file.size, None));

    let ffp = ctx.ffprobe_path.map(String::from);
    let kernel_clone = ctx.kernel.clone();
    match crate::introspect::introspect_file(
        current_path,
        size,
        hash,
        &kernel_clone,
        ffp.as_deref(),
    )
    .await
    {
        Ok(mut new_file) => {
            // Preserve plugin_metadata from prior phases
            for (k, v) in &file.plugin_metadata {
                new_file
                    .plugin_metadata
                    .entry(k.clone())
                    .or_insert(v.clone());
            }
            // Reapply detector-derived languages so subsequent
            // phases see normalized values, not raw ffprobe tags.
            apply_detected_languages(&mut new_file);
            new_file
        }
        Err(e) => {
            tracing::warn!(error = %e, "re-introspection failed, using previous state");
            file.clone()
        }
    }
}

/// Collect safeguard violations across plans and tag the file.
fn collect_safeguard_violations(
    file: &voom_domain::media::MediaFile,
    result: &voom_phase_orchestrator::OrchestrationResult,
    ctx: &ProcessContext<'_>,
) {
    let violations: Vec<&voom_domain::SafeguardViolation> = result
        .plans
        .iter()
        .flat_map(|p| &p.safeguard_violations)
        .collect();
    if !violations.is_empty() {
        let mut tagged_file = file.clone();
        tagged_file.plugin_metadata.insert(
            "safeguard_violations".to_string(),
            serde_json::json!(violations),
        );
        let r = ctx.kernel.dispatch(Event::FileIntrospected(
            voom_domain::events::FileIntrospectedEvent::new(tagged_file),
        ));
        log_plugin_errors(&r);
    }
}

const AUDIO_LANGUAGE_DETECTOR_PLUGIN: &str = "audio-language-detector";

/// Apply audio language detection results to track language fields.
///
/// If the `audio-language-detector` plugin has produced metadata, update
/// each track's language to match the detected value. This runs before
/// policy evaluation so that policies can filter on detected languages
/// (e.g. `remove audio where lang == zxx` for silent tracks).
fn apply_detected_languages(file: &mut voom_domain::media::MediaFile) {
    let metadata = match file.plugin_metadata.get(AUDIO_LANGUAGE_DETECTOR_PLUGIN) {
        Some(m) => m,
        None => return,
    };

    let detections = match metadata.get("detections").and_then(|d| d.as_array()) {
        Some(d) => d,
        None => return,
    };

    for det in detections {
        let track_index = match det.get("track_index").and_then(|v| v.as_u64()) {
            Some(i) => i as u32,
            None => continue,
        };
        let detected = match det.get("detected_language").and_then(|v| v.as_str()) {
            Some(l) => l,
            None => continue,
        };

        let Some(track) = file.tracks.iter_mut().find(|t| t.index == track_index) else {
            continue;
        };

        let normalized = match voom_domain::utils::language::normalize_language(detected) {
            Some(code) => code,
            None => {
                tracing::warn!(
                    path = %file.path.display(),
                    track = track_index,
                    detected = %detected,
                    "unrecognized language code from detector, skipping"
                );
                continue;
            }
        };

        if track.language == normalized {
            continue;
        }

        if track.language != "und" {
            tracing::warn!(
                path = %file.path.display(),
                track = track_index,
                existing = %track.language,
                detected = %normalized,
                "overwriting track language with detected value"
            );
        }

        track.language = normalized.to_string();
    }
}

/// Run the phase orchestrator to produce plans (used for dry-run mode).
///
/// NOTE: This function does NOT dispatch `PlanCreated` events. Dispatching
/// here would trigger executor plugins during dry-run mode.
fn orchestrate_plans(
    compiled: &voom_dsl::CompiledPolicy,
    file: &voom_domain::media::MediaFile,
    capabilities: &voom_domain::CapabilityMap,
) -> voom_phase_orchestrator::OrchestrationResult {
    let plans = voom_policy_evaluator::PolicyEvaluator::new()
        .evaluate_with_capabilities(compiled, file, capabilities)
        .plans;
    let orchestrator = voom_phase_orchestrator::PhaseOrchestrator::new();
    orchestrator.orchestrate(plans)
}

/// Determine the file path after plan execution.
///
/// If a `ConvertContainer` action changed the container, the file extension
/// will have changed on disk (e.g. `.mp4` → `.mkv`). Derive the new path
/// from the plan actions; fall back to the original path if unchanged.
fn resolve_post_execution_path(
    file: &voom_domain::media::MediaFile,
    plans: &[voom_domain::plan::Plan],
) -> std::path::PathBuf {
    if let Some(container) = find_last_container_action(plans) {
        let new_path = file.path.with_extension(container.as_str());
        if new_path.exists() {
            return new_path;
        }
    }
    file.path.clone()
}

/// Search plans (last to first) for the most recent ConvertContainer action.
fn find_last_container_action(
    plans: &[voom_domain::plan::Plan],
) -> Option<voom_domain::media::Container> {
    for plan in plans.iter().rev() {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }
        for action in &plan.actions {
            if action.operation == OperationType::ConvertContainer {
                if let voom_domain::plan::ActionParams::Container { container } = &action.parameters
                {
                    return Some(*container);
                }
            }
        }
    }
    None
}

/// Result of executing a single plan via the event bus.
enum PlanOutcome {
    /// An executor claimed and completed the plan.
    Success { executor: String },
    /// Execution failed (executor error or unclaimed).
    Failed(PlanFailedEvent),
}

/// Dispatch `PlanExecuting` + `PlanCreated` for a single plan.
///
/// Returns the outcome without dispatching `PlanCompleted` or `PlanFailed`
/// — the caller decides when to commit the result (e.g. after size checks).
///
/// `PlanExecuting` is dispatched first so the backup-manager backs up the file
/// BEFORE any executor modifies it.  `PlanCreated` then lets executor plugins
/// claim and run the plan.
fn execute_single_plan(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    kernel: &voom_kernel::Kernel,
) -> PlanOutcome {
    let r = kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent::new(
        plan.id,
        file.path.clone(),
        plan.phase_name.clone(),
        plan.actions.len(),
    )));
    log_plugin_errors(&r);

    let results = kernel.dispatch(Event::PlanCreated(PlanCreatedEvent::new(plan.clone())));
    log_plugin_errors(&results);

    let claimed = results.iter().any(|r| r.claimed);
    let exec_error = results.iter().find_map(|r| r.execution_error.clone());

    if claimed && exec_error.is_none() {
        let executor = results
            .iter()
            .find(|r| r.claimed)
            .map(|r| r.plugin_name.clone())
            .unwrap_or_else(|| "unknown".to_string());
        PlanOutcome::Success { executor }
    } else if let Some(error) = exec_error {
        let mut failed =
            PlanFailedEvent::new(plan.id, file.path.clone(), plan.phase_name.clone(), error);
        failed.plugin_name = results
            .iter()
            .find(|r| r.claimed)
            .map(|r| r.plugin_name.clone());
        PlanOutcome::Failed(failed)
    } else {
        PlanOutcome::Failed(PlanFailedEvent::new(
            plan.id,
            file.path.clone(),
            plan.phase_name.clone(),
            "no executor available for plan",
        ))
    }
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
            "{} {} backups retained ({}) \u{2014} delete with: find {} -name '*.vbak' -delete",
            style("Info:").bold(),
            style(ctx.modified).cyan(),
            style(format_size(ctx.backup_bytes)).cyan(),
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
/// operations, `sqlite-store` performs a blocking SQLite write on every dispatch.
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
    use voom_domain::events::{EventResult, FileDiscoveredEvent, FileIntrospectedEvent};
    use voom_domain::media::MediaFile;
    use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

    /// A test plugin that counts received plan lifecycle events.
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
        fn name(&self) -> &str {
            "plan-recorder"
        }
        fn version(&self) -> &str {
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

    fn test_plan(phase: &str, skipped: bool) -> Plan {
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
        let r = kernel.dispatch(Event::PlanCreated(PlanCreatedEvent::new(
            skipped_plan.clone(),
        )));
        log_plugin_errors(&r);
        let r = kernel.dispatch(Event::PlanSkipped(PlanSkippedEvent::new(
            skipped_plan.id,
            file.path.clone(),
            skipped_plan.phase_name.clone(),
            skipped_plan.skip_reason.clone().unwrap(),
        )));
        log_plugin_errors(&r);

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
        fn name(&self) -> &str {
            "job-recorder"
        }
        fn version(&self) -> &str {
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
        // Use a tempdir — the local filesystem should have plenty of space.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;

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
            token: &token,
            ffprobe_path: None,
            capabilities: &capabilities,
            counters: &counters,
        };

        // Should return false (enough space)
        assert!(!check_disk_space(&plan, &file, &ctx));
    }

    #[test]
    fn test_check_disk_space_dispatches_plan_failed_without_plan_created() {
        // Use a tempdir so we get a valid path for disk-space checks.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mkv");
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
            token: &token,
            ffprobe_path: None,
            capabilities: &capabilities,
            counters: &counters,
        };

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
}
