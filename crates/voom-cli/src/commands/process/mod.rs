mod audio_language;
mod dispatch;
mod pipeline;
mod pipeline_streaming;
mod plan_outcome;
mod root_gate;
mod safeguards;
mod transitions;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use anyhow::{Context, Result};
use console::style;
use parking_lot::Mutex;

use tokio_util::sync::CancellationToken;

use pipeline::process_single_file;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use crate::config;
use crate::paths::resolve_paths;
use crate::policy_map::PolicyResolver;
use crate::progress::BatchProgress;
use voom_domain::events::{
    Event, IntrospectSessionCompletedEvent, JobCompletedEvent, JobProgressEvent, JobStartedEvent,
};
use voom_domain::storage::CostModelSampleFilters;
use voom_domain::utils::format::{format_duration, format_size};
use voom_job_manager::progress::{CompositeReporter, ProgressReporter};
use voom_job_manager::worker::{JobErrorStrategy, WorkerPool, WorkerPoolConfig};

fn hw_resource_for_backend(backend: &str) -> Option<&'static str> {
    match backend {
        "nvenc" => Some("hw:nvenc"),
        "qsv" => Some("hw:qsv"),
        "vaapi" => Some("hw:vaapi"),
        "videotoolbox" => Some("hw:videotoolbox"),
        _ => None,
    }
}

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

    let estimate_mode = args.estimate || args.estimate_only;
    let plan_only = args.plan_only;
    let dry_run = args.dry_run || plan_only || estimate_mode;

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
    let limited_hw_resources = ["hw:nvenc", "hw:qsv", "hw:vaapi", "hw:videotoolbox"]
        .into_iter()
        .filter_map(|resource| {
            capabilities
                .parallel_limit(resource)
                .map(|limit| (resource.to_string(), limit))
        })
        .collect::<Vec<_>>();
    let default_hw_resource =
        hw_resource_for_backend(capabilities.best_hwaccel()).map(str::to_string);
    let plan_limiter = Arc::new(
        voom_job_manager::worker::PlanExecutionLimiter::from_limits_with_default(
            limited_hw_resources,
            default_hw_resource,
        ),
    );

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
            let prune_result = store.prune_missing_files_under(path);
            match prune_result {
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

        // Pre-load the bad-file set so the streaming ingest stage can filter
        // inline without per-file DB lookups.
        let bad_files: std::collections::HashSet<std::path::PathBuf> = if args.force_rescan {
            std::collections::HashSet::new()
        } else {
            store
                .list_bad_files(&voom_domain::storage::BadFileFilters::default())
                .context("failed to list bad files")?
                .into_iter()
                .map(|bf| bf.path)
                .collect()
        };

        if token.is_cancelled() {
            if !plan_only && !quiet {
                eprintln!("{}", style("Interrupted before discovery.").yellow());
            }
            return Ok(());
        }

        let scan_session = store
            .begin_scan_session(&paths)
            .context("failed to begin scan session")?;

        let on_error = match args.on_error {
            ErrorHandling::Fail => JobErrorStrategy::Fail,
            ErrorHandling::Continue => JobErrorStrategy::Continue,
        };

        let (pool, effective_workers) = create_worker_pool(job_queue, &args, token.clone());
        let pool = Arc::new(pool);

        // Build reporter with an empty event list; the pipeline calls
        // reporter.seed_events(&events) before on_batch_start once discovery
        // finishes.
        let reporter: Arc<dyn ProgressReporter> =
            build_reporter(&[], effective_workers, plan_only, quiet, kernel.clone());

        let all_phase_names = resolver.all_phase_names();
        let resolver = Arc::new(resolver);
        let flag_size_increase = args.flag_size_increase;
        let flag_duration_shrink = args.flag_duration_shrink;
        let confirm_savings = args.confirm_savings;
        let force_rescan = args.force_rescan;
        let estimate_samples = store
            .list_cost_model_samples(&CostModelSampleFilters::default())
            .context("failed to load estimate cost model samples")?;
        let estimate_model = Arc::new(voom_domain::EstimateModel::from_samples(estimate_samples));

        let token_for_workers = token.clone();
        let ffprobe_path: Option<String> = config.ffprobe_path().map(String::from);
        let ffprobe_path = Arc::new(ffprobe_path);
        let animation_detection_mode = config.animation_detection_mode();
        let kernel_for_workers = kernel.clone();
        let store_for_workers = store.clone();
        let capabilities_for_workers = capabilities.clone();
        let plan_limiter_for_workers = plan_limiter.clone();
        let counters_for_workers = counters.clone();

        let heartbeat_store = store.clone();
        let heartbeat_token = token.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            interval.tick().await; // skip the first immediate tick
            loop {
                tokio::select! {
                    () = heartbeat_token.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(e) = heartbeat_store.heartbeat_scan_session(scan_session) {
                            tracing::warn!(error = %e, "scan session heartbeat failed");
                        }
                    }
                }
            }
        });

        let processor = move |job: voom_domain::job::Job| {
            let resolver = resolver.clone();
            let kernel = kernel_for_workers.clone();
            let store = store_for_workers.clone();
            let token = token_for_workers.clone();
            let ffprobe_path = ffprobe_path.clone();
            let capabilities = capabilities_for_workers.clone();
            let plan_limiter = plan_limiter_for_workers.clone();
            let estimate_model = estimate_model.clone();
            let counters = counters_for_workers.clone();
            async move {
                let ctx = ProcessContext {
                    resolver: &resolver,
                    kernel,
                    store,
                    dry_run,
                    plan_only,
                    estimate_mode,
                    flag_size_increase,
                    flag_duration_shrink,
                    force_rescan,
                    token: &token,
                    ffprobe_path: ffprobe_path.as_deref(),
                    animation_detection_mode,
                    capabilities: &capabilities,
                    plan_limiter,
                    confirm_savings,
                    estimate_model,
                    counters: &counters,
                    scan_session,
                };
                process_single_file(job, &ctx).await
            }
        };

        let outcome = match pipeline_streaming::run_streaming_pipeline(
            &args,
            &paths,
            kernel.clone(),
            store.clone(),
            pool.clone(),
            reporter.clone(),
            on_error,
            bad_files,
            processor,
            quiet,
            plan_only,
            token.clone(),
            scan_session,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                heartbeat_handle.abort();
                if let Err(cancel_err) = store.cancel_scan_session(scan_session) {
                    tracing::warn!(error = %cancel_err, "failed to cancel scan session on error");
                }
                return Err(e);
            }
        };

        heartbeat_handle.abort();

        // Mark the session terminated. We intentionally use cancel rather than
        // finish: finish_scan_session computes a "missing files" set as the
        // difference between the files table and files ingested via
        // ingest_discovered_file(session, ...) during the session. The streaming
        // pipeline does not currently call ingest_discovered_file per file — it
        // emits FileDiscovered events via the kernel bus, and sqlite-store's
        // handler uses upsert_discovered_file which does NOT register the file
        // against the active session. As a result, finish_scan_session would
        // mark every pre-existing file as missing.
        //
        // The fail-closed safety properties this scan session enables
        // (record_voom_mutation at the executor, SessionMutationSnapshot at
        // the scanner) do not depend on finish_scan_session — they engage on
        // session begin and during executor/scanner work. The "skip missing
        // files" behavior of finish is left for a follow-up that wires
        // ingest_discovered_file into the streaming FileDiscovered path.
        //
        // FOLLOWUP(#361): wire ingest_discovered_file into the streaming
        // FileDiscovered handler (or thread an active session id into
        // FileDiscoveredEvent and have sqlite-store dispatch on it), then
        // reinstate finish_scan_session on the success path.
        if let Err(e) = store.cancel_scan_session(scan_session) {
            tracing::warn!(error = %e, "failed to cancel scan session");
        }

        if outcome.discovery_errors > 0 {
            tracing::warn!(
                count = outcome.discovery_errors,
                "discovery reported errors during streaming pipeline"
            );
        }

        if outcome.discovered == 0 {
            if token.is_cancelled() {
                if !plan_only && !quiet {
                    eprintln!("{}", style("Interrupted before processing.").yellow());
                }
            } else if plan_only {
                println!("[]");
            } else if !quiet {
                eprintln!("{}", style("No media files found.").yellow());
            }
            return Ok(());
        }

        if outcome.skipped_bad > 0 && !plan_only && !quiet {
            eprintln!(
                "Skipping {} known-bad files (use {} to re-attempt).",
                style(outcome.skipped_bad).yellow(),
                style("--force-rescan").bold()
            );
        }

        let file_count = outcome.enqueued as usize;

        if !token.is_cancelled() {
            kernel.dispatch(Event::IntrospectSessionCompleted(
                IntrospectSessionCompletedEvent::new(pool.completed_count()),
            ));
        }

        print_run_results(&RunResultsContext {
            counters: &counters,
            store: store.as_ref(),
            plan_only,
            estimate_mode,
            quiet,
            cancelled: token.is_cancelled(),
            pool: pool.as_ref(),
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
    store: &'a dyn voom_domain::storage::StorageTrait,
    plan_only: bool,
    estimate_mode: bool,
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

    if ctx.estimate_mode {
        let plans = ctx.counters.estimate_plans.lock().clone();
        let samples = ctx
            .store
            .list_cost_model_samples(&CostModelSampleFilters::default())
            .context("failed to load estimate cost model samples")?;
        let model = voom_domain::EstimateModel::from_samples(samples);
        let estimate = voom_domain::estimate_plans(
            voom_domain::EstimateInput::new(plans, ctx.effective_workers, chrono::Utc::now()),
            &model,
        );
        ctx.store
            .insert_estimate_run(&estimate)
            .context("failed to persist estimate run")?;
        print_estimate(&estimate);
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

fn print_estimate(estimate: &voom_domain::EstimateRun) {
    println!(
        "Estimating cost for {} planned file phases...",
        estimate.file_count
    );
    println!();
    print_estimate_breakdowns(estimate);
    println!("Total wall time:     ~{}", format_ms(estimate.wall_time_ms));
    println!(
        "Total compute time:  ~{}",
        format_ms(estimate.compute_time_ms)
    );
    println!("Bytes in:            {}", format_size(estimate.bytes_in));
    println!(
        "Bytes out:           {} (estimated)",
        format_size(estimate.bytes_out)
    );
    println!(
        "Bytes saved:         {}",
        format_signed_size(estimate.bytes_saved)
    );
    println!();
    println!(
        "Files where transcoding net loses bytes: {}",
        estimate.net_loss_files
    );
    println!(
        "Files where estimate uncertainty is high: {}",
        estimate.high_uncertainty_files
    );
    if estimate.high_uncertainty_files > 0 {
        println!(
            "High-uncertainty range: bytes saved {} to {}, compute time {} to {}",
            format_signed_size(scale_i64(estimate.bytes_saved, 0.5)),
            format_signed_size(scale_i64(estimate.bytes_saved, 1.5)),
            format_ms(scale_u64(estimate.compute_time_ms, 0.5)),
            format_ms(scale_u64(estimate.compute_time_ms, 1.5)),
        );
    }
}

fn print_estimate_breakdowns(estimate: &voom_domain::EstimateRun) {
    let mut phases: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    let mut transcodes: BTreeMap<(String, String), (usize, u64)> = BTreeMap::new();
    for file in &estimate.files {
        let phase = phases.entry(&file.phase_name).or_default();
        phase.0 += 1;
        phase.1 = phase.1.saturating_add(file.compute_time_ms);

        for action in &file.actions {
            let Some(codec) = action.codec.as_ref() else {
                continue;
            };
            let backend = action.backend.as_deref().unwrap_or("software").to_string();
            let bucket = transcodes.entry((codec.clone(), backend)).or_default();
            bucket.0 += 1;
            bucket.1 = bucket.1.saturating_add(action.compute_time_ms);
        }
    }

    if !phases.is_empty() {
        println!("Phase breakdown:");
        for (phase, (count, compute_ms)) in phases {
            println!(
                "  {phase:<18} {count:>5} plans     ~{}",
                format_ms(compute_ms)
            );
        }
        println!();
    }

    if !transcodes.is_empty() {
        println!("Transcode breakdown:");
        for ((codec, backend), (count, compute_ms)) in transcodes {
            println!(
                "  {codec:<8} {backend:<12} {count:>5} plans     ~{}",
                format_ms(compute_ms)
            );
        }
        println!();
    }
}

fn format_ms(ms: u64) -> String {
    format_duration(ms as f64 / 1_000.0)
}

fn scale_u64(value: u64, multiplier: f64) -> u64 {
    (value as f64 * multiplier).round() as u64
}

fn scale_i64(value: i64, multiplier: f64) -> i64 {
    (value as f64 * multiplier).round() as i64
}

fn format_signed_size(bytes: i64) -> String {
    if bytes >= 0 {
        format_size(bytes.unsigned_abs())
    } else {
        format!("-{}", format_size(bytes.unsigned_abs()))
    }
}

/// Build a `PolicyResolver` from CLI args and config.
fn build_policy_resolver(
    args: &ProcessArgs,
    config: &crate::config::AppConfig,
    root: &std::path::Path,
) -> Result<PolicyResolver> {
    if let Some(ref policy_path) = args.policy {
        let resolved = crate::config::resolve_policy_path(policy_path);
        let compiled = voom_dsl::compile_policy_file(&resolved)
            .with_context(|| format!("failed to compile policy: {}", resolved.display()))?;
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

/// Compute job priority based on file modification date.
///
/// More recently modified files get higher priority (lower number).
/// - Modified within 7 days: 10
/// - Modified within 30 days: 50
/// - Modified within 1 year: 100
/// - Older or metadata unavailable: 200
pub(super) fn compute_file_date_priority(path: &std::path::Path) -> i32 {
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
    pub(super) estimate_plans: Arc<Mutex<Vec<voom_domain::Plan>>>,
    pub(super) session_id: uuid::Uuid,
}

impl RunCounters {
    fn new() -> Self {
        Self {
            modified_count: Arc::new(AtomicU64::new(0)),
            backup_bytes: Arc::new(AtomicU64::new(0)),
            phase_stats: Arc::new(Mutex::new(HashMap::new())),
            plan_collector: Arc::new(Mutex::new(Vec::new())),
            estimate_plans: Arc::new(Mutex::new(Vec::new())),
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
    pub(super) estimate_mode: bool,
    pub(super) flag_size_increase: bool,
    pub(super) flag_duration_shrink: bool,
    /// When true, bypass the introspection cache and force a fresh ffprobe pass.
    pub(super) force_rescan: bool,
    pub(super) token: &'a CancellationToken,
    pub(super) ffprobe_path: Option<&'a str>,
    pub(super) animation_detection_mode: voom_ffprobe_introspector::parser::AnimationDetectionMode,
    pub(super) capabilities: &'a voom_domain::CapabilityMap,
    pub(super) plan_limiter: Arc<voom_job_manager::worker::PlanExecutionLimiter>,
    pub(super) confirm_savings: Option<u64>,
    pub(super) estimate_model: Arc<voom_domain::EstimateModel>,
    pub(super) counters: &'a RunCounters,
    pub(super) scan_session: voom_domain::transition::ScanSessionId,
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
    use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction, TranscodeSettings};

    use super::dispatch::dispatch_and_log;
    use super::pipeline::execute_single_plan;
    use super::plan_outcome::PlanOutcome;
    use super::safeguards::{check_disk_space, check_duration_shrink, check_size_increase};

    #[test]
    fn test_hw_resource_for_backend() {
        assert_eq!(hw_resource_for_backend("nvenc"), Some("hw:nvenc"));
        assert_eq!(hw_resource_for_backend("qsv"), Some("hw:qsv"));
        assert_eq!(hw_resource_for_backend("vaapi"), Some("hw:vaapi"));
        assert_eq!(
            hw_resource_for_backend("videotoolbox"),
            Some("hw:videotoolbox")
        );
        assert_eq!(hw_resource_for_backend("none"), None);
        assert_eq!(hw_resource_for_backend("unknown"), None);
    }

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

    pub(super) fn test_plan_with_transcode_hw(phase: &str, hw: &str) -> Plan {
        test_plan_with_optional_transcode_hw(phase, Some(hw))
    }

    pub(super) fn test_plan_with_optional_transcode_hw(phase: &str, hw: Option<&str>) -> Plan {
        let mut plan = test_plan(phase, false);
        plan.actions = vec![PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hw(hw.map(str::to_string)),
            },
            "Transcode video",
        )];
        plan
    }

    /// Bundle of long-lived test fixtures shared across `ProcessContext`
    /// construction sites. Owns the `TempDir` so the resolver's working path
    /// stays valid for the test's lifetime.
    pub(super) struct TestFixture {
        capabilities: voom_domain::CapabilityMap,
        plan_limiter: Arc<voom_job_manager::worker::PlanExecutionLimiter>,
        counters: RunCounters,
        token: CancellationToken,
        resolver: PolicyResolver,
        pub(super) scan_session: voom_domain::transition::ScanSessionId,
        // Held for lifetime; the resolver borrows `dir.path()`.
        dir: tempfile::TempDir,
    }

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
                plan_limiter: Arc::new(voom_job_manager::worker::PlanExecutionLimiter::default()),
                counters: RunCounters::new(),
                token: CancellationToken::new(),
                resolver,
                scan_session: voom_domain::transition::ScanSessionId::new(),
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
        pub(super) fn make_ctx(
            &self,
            kernel: Arc<voom_kernel::Kernel>,
            store: Arc<dyn voom_domain::storage::StorageTrait>,
        ) -> ProcessContext<'_> {
            ProcessContext {
                resolver: &self.resolver,
                kernel,
                store,
                dry_run: false,
                plan_only: false,
                estimate_mode: false,
                flag_size_increase: false,
                flag_duration_shrink: false,
                force_rescan: false,
                token: &self.token,
                ffprobe_path: None,
                animation_detection_mode: Default::default(),
                capabilities: &self.capabilities,
                plan_limiter: self.plan_limiter.clone(),
                confirm_savings: None,
                estimate_model: Arc::new(voom_domain::EstimateModel::default()),
                counters: &self.counters,
                scan_session: self.scan_session,
            }
        }

        /// Convenience: builds a `ProcessContext` with a fresh empty `Kernel`
        /// and an `InMemoryStore`. Use for tests that don't need to register
        /// plugins or use a SQLite store.
        pub(super) fn make_default_ctx(&self) -> ProcessContext<'_> {
            self.make_ctx(
                Arc::new(voom_kernel::Kernel::new()),
                Arc::new(voom_domain::test_support::InMemoryStore::new()),
            )
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

    #[test]
    fn test_check_disk_space_passes_with_enough_space() {
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;

        let plan = test_plan("normalize", false);

        let ctx = fixture.make_default_ctx();

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

        let _ = pipeline::handle_plan_success(plan, &file, "mkvtoolnix-executor", 0, false, &ctx)
            .await
            .expect("finalize successful plan");

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

    #[tokio::test]
    async fn handle_plan_success_reports_transition_record_failure_with_context() {
        use voom_domain::media::{Container, MediaFile};
        use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

        let fixture =
            TestFixture::with_policy(r#"policy "test" { phase convert { container mkv } }"#);
        let mp4_path = fixture.dir_path().join("movie.mp4");
        let mkv_path = fixture.dir_path().join("movie.mkv");
        std::fs::write(&mkv_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(mp4_path.clone());
        file.container = Container::Mp4;
        file.size = 1024;
        file.content_hash = Some("oldhash".to_string());

        let mut plan = Plan::new(file.clone(), "containerize", "convert");
        let plan_id = plan.id;
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
        let mut conflicting_file = MediaFile::new(mkv_path);
        conflicting_file.container = Container::Mkv;
        conflicting_file.size = 512;
        conflicting_file.content_hash = Some("conflict".to_string());
        store.upsert_file(&conflicting_file).unwrap();

        let kernel = Arc::new(voom_kernel::Kernel::new());
        let ctx = ProcessContext {
            ffprobe_path: Some("/nonexistent/ffprobe"),
            ..fixture.make_ctx(kernel, store)
        };

        let error =
            pipeline::handle_plan_success(plan, &file, "mkvtoolnix-executor", 0, false, &ctx)
                .await
                .expect_err(
                    "conflicting post-execution path should make transition recording fail",
                );
        let message = error.to_string();

        assert!(message.contains("failed to record post-execution transition"));
        assert!(message.contains(&mp4_path.display().to_string()));
        assert!(message.contains("phase 'convert'"));
        assert!(message.contains(&plan_id.to_string()));
        assert!(message.contains("storage error:"));
    }

    #[tokio::test]
    async fn handle_plan_success_clears_orphan_bad_files_row_at_post_execution_path() {
        use voom_domain::bad_file::{BadFile, BadFileSource};
        use voom_domain::media::{Container, MediaFile};
        use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

        let fixture =
            TestFixture::with_policy(r#"policy "test" { phase convert { container mkv } }"#);
        let mp4_path = fixture.dir_path().join("movie.mp4");
        let mkv_path = fixture.dir_path().join("movie.mkv");
        // Pre-write the target so `resolve_post_execution_path` accepts the new extension.
        std::fs::write(&mkv_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(mp4_path);
        file.container = Container::Mp4;
        file.size = 1024;
        file.content_hash = Some("oldhash".to_string());

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

        // Pre-seed a bad_files row at the post-execution path, simulating the
        // FileIntrospectionFailed event that fires when reintrospection fails.
        // The bundled rename must clear it.
        let orphan = BadFile::new(
            mkv_path.clone(),
            2048,
            Some("post_exec_hash".into()),
            "ffprobe failed: process exited with code 1".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&orphan).unwrap();
        assert!(
            store.bad_file_by_path(&mkv_path).unwrap().is_some(),
            "precondition: orphan bad_files row must exist before handle_plan_success"
        );

        let kernel = Arc::new(voom_kernel::Kernel::new());
        // Nonexistent ffprobe forces reintrospection onto its failure-fallback
        // branch deterministically, regardless of $PATH.
        let ctx = ProcessContext {
            ffprobe_path: Some("/nonexistent/ffprobe"),
            ..fixture.make_ctx(kernel, store.clone())
        };

        let new_file =
            pipeline::handle_plan_success(plan, &file, "mkvtoolnix-executor", 0, false, &ctx)
                .await
                .expect("finalize successful plan");

        assert_eq!(
            new_file.path, mkv_path,
            "handle_plan_success must return a MediaFile reflecting the post-execution path"
        );
        assert!(
            store.bad_file_by_path(&mkv_path).unwrap().is_none(),
            "handle_plan_success must clear orphan bad_files row at post-execution path"
        );
    }

    #[tokio::test]
    async fn handle_plan_success_clears_bad_files_row_when_plan_is_no_op_and_reintrospection_fails()
    {
        // Issue #180: when a plan executes "successfully" but produces no
        // observable change (same path AND same hash), `record_file_transition`
        // short-circuits (hash_changed=false, path_changed=false) before the
        // bundled `record_post_execution` call can clear any `bad_files` row.
        //
        // This test simulates the orphan row that exists at the start of
        // `handle_plan_success` (e.g. left by a previous failed scan or a
        // re-introspection failure in a concurrent job) and verifies it is
        // cleared even when the plan produces no observable change.
        use voom_domain::bad_file::{BadFile, BadFileSource};
        use voom_domain::media::{Container, MediaFile};
        use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

        let fixture = TestFixture::with_policy(
            r#"policy "test" { phase tag { defaults { audio: first } } }"#,
        );
        let path = fixture.dir_path().join("movie.mkv");
        // Real bytes on disk so post-execution hashing succeeds and matches the
        // pre-execution hash — that is what triggers the no-change short-circuit.
        let payload = vec![7u8; 1024];
        std::fs::write(&path, &payload).unwrap();
        let known_hash = voom_discovery::hash_file(&path).unwrap();

        let mut file = MediaFile::new(path.clone());
        file.container = Container::Mkv;
        file.size = u64::try_from(payload.len()).unwrap();
        file.content_hash = Some(known_hash.clone());

        // A track-op-only plan: same container, same path, no container
        // conversion — `record_file_transition` will see hash_changed=false
        // and path_changed=false and return early without calling
        // `record_post_execution`.
        let mut plan = Plan::new(file.clone(), "tag-defaults", "tag");
        plan.actions = vec![PlannedAction::track_op(
            OperationType::SetDefault,
            0,
            ActionParams::Empty,
            "Mark first track default",
        )];

        let store: Arc<dyn voom_domain::storage::StorageTrait> =
            Arc::new(voom_sqlite_store::store::SqliteStore::in_memory().unwrap());
        store.upsert_file(&file).unwrap();

        // Pre-seed a bad_files row at `path`, simulating the state that the
        // bug leaves behind: a FileIntrospectionFailed event from a previous
        // pass (or re-introspection failure) wrote a bad_files row, and the
        // bundled cleanup in `record_post_execution` was never reached because
        // `record_file_transition` short-circuited on the no-change check.
        let orphan = BadFile::new(
            path.clone(),
            file.size,
            file.content_hash.clone(),
            "ffprobe failed: no such file".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&orphan).unwrap();
        assert!(
            store.bad_file_by_path(&path).unwrap().is_some(),
            "precondition: orphan bad_files row must exist before handle_plan_success"
        );

        let kernel = Arc::new(voom_kernel::Kernel::new());
        let ctx = ProcessContext {
            ffprobe_path: Some("/nonexistent/ffprobe"),
            ..fixture.make_ctx(kernel, store.clone())
        };

        let new_file =
            pipeline::handle_plan_success(plan, &file, "mkvtoolnix-executor", 0, false, &ctx)
                .await
                .expect("finalize successful plan");

        assert_eq!(
            new_file.path, path,
            "no-op plan must leave the file path unchanged"
        );
        assert_eq!(
            new_file.content_hash.as_deref(),
            Some(known_hash.as_str()),
            "no-op plan must leave the content hash unchanged (this is what triggers the short-circuit)"
        );
        assert!(
            store.bad_file_by_path(&path).unwrap().is_none(),
            "handle_plan_success must not leave an orphan bad_files row when the plan produces no observable change"
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
        let fixture =
            TestFixture::with_policy(r#"policy "test" { phase convert { container mkv } }"#);
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
        })
        .expect("record transition");

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
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
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
        let ctx = ProcessContext {
            flag_size_increase: true,
            ..fixture.make_ctx(Arc::new(kernel), store)
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
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;
        file.duration = 100.0;

        let plan = test_plan("normalize", false);

        let ctx = fixture.make_default_ctx();

        // Flag disabled — must early-return false without invoking ffprobe.
        assert!(!check_duration_shrink(&plan, &file, &ctx).await);
    }

    #[tokio::test]
    async fn test_check_duration_shrink_zero_input_duration_returns_false() {
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;
        file.duration = 0.0;

        let plan = test_plan("normalize", false);

        let ctx = ProcessContext {
            flag_duration_shrink: true,
            ..fixture.make_default_ctx()
        };

        // Input duration is 0.0 — can't compute a percentage; must early-return false.
        assert!(!check_duration_shrink(&plan, &file, &ctx).await);
    }

    #[tokio::test]
    async fn test_check_duration_shrink_cancelled_returns_false() {
        let fixture = TestFixture::new();
        let file_path = fixture.dir_path().join("test.mkv");
        std::fs::write(&file_path, vec![0u8; 1024]).unwrap();

        let mut file = MediaFile::new(file_path);
        file.size = 1024;
        file.duration = 100.0;

        let plan = test_plan("normalize", false);

        fixture.cancel();
        let ctx = ProcessContext {
            flag_duration_shrink: true,
            ..fixture.make_default_ctx()
        };

        // Token cancelled — must early-return false without launching ffprobe.
        assert!(!check_duration_shrink(&plan, &file, &ctx).await);
    }
}
