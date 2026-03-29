use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use anyhow::{Context, Result};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use tokio_util::sync::CancellationToken;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use crate::config;
use crate::output::{max_filename_len, shrink_filename, PROGRESS_FIXED_WIDTH};
use voom_domain::events::{
    Event, JobCompletedEvent, JobProgressEvent, JobStartedEvent, PlanCompletedEvent,
    PlanCreatedEvent, PlanExecutingEvent, PlanFailedEvent,
};
use voom_domain::plan::OperationType;
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
pub async fn run(args: ProcessArgs, token: CancellationToken) -> Result<()> {
    let config = config::load_config()?;
    let app::BootstrapResult {
        kernel,
        store,
        collector,
        job_queue,
    } = app::bootstrap_kernel_with_store(&config)?;
    let kernel = Arc::new(kernel);
    let capabilities = Arc::new(collector.snapshot());

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    let compiled = load_and_compile_policy(&args)?;

    print_run_header(&compiled.name, &path, args.dry_run);

    // Auto-prune stale file entries under the target directory
    match store.prune_missing_files_under(&path) {
        Ok(n) if n > 0 => println!("Pruned {n} stale entries."),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "auto-prune failed"),
    }

    if token.is_cancelled() {
        println!("{}", style("Interrupted before discovery.").yellow());
        return Ok(());
    }

    // Discover files
    let mut events = discover_files(&path, &args, &kernel)?;
    if events.is_empty() {
        println!("{}", style("No media files found.").yellow());
        return Ok(());
    }

    // Filter out known-bad files unless --force-rescan is set
    if !args.force_rescan {
        let bad_files = store
            .list_bad_files(&voom_domain::storage::BadFileFilters::default())
            .context("failed to list bad files")?;
        if !bad_files.is_empty() {
            let bad_paths: std::collections::HashSet<_> =
                bad_files.iter().map(|bf| &bf.path).collect();
            let before = events.len();
            events.retain(|e| !bad_paths.contains(&e.path));
            let skipped = before - events.len();
            if skipped > 0 {
                println!(
                    "Skipping {} known-bad files (use {} to re-attempt).",
                    style(skipped).yellow(),
                    style("--force-rescan").bold()
                );
            }
        }
    }

    if events.is_empty() {
        println!("{}", style("No processable files found.").yellow());
        return Ok(());
    }

    let file_count = events.len();
    println!("Found {} media files.", style(file_count).bold());

    let on_error = match args.on_error {
        ErrorHandling::Fail => JobErrorStrategy::Fail,
        ErrorHandling::Continue => JobErrorStrategy::Continue,
    };

    if token.is_cancelled() {
        println!("{}", style("Interrupted before processing.").yellow());
        return Ok(());
    }

    let (pool, effective_workers) = create_worker_pool(job_queue, &args, token.clone())?;

    let cli_reporter: Arc<dyn ProgressReporter> = Arc::new(CliProgressReporter::new(file_count));
    let bus_reporter: Arc<dyn ProgressReporter> = Arc::new(EventBusReporter::new(kernel.clone()));
    let reporter: Arc<dyn ProgressReporter> =
        Arc::new(CompositeReporter::new(vec![cli_reporter, bus_reporter]));

    let items = build_work_items(&events);
    let compiled = Arc::new(compiled);
    let dry_run = args.dry_run;
    let flag_size_increase = args.flag_size_increase;
    let modified_count = Arc::new(AtomicU64::new(0));

    let token_for_workers = token.clone();
    let ffprobe_path: Option<String> = config.ffprobe_path().map(String::from);
    let ffprobe_path = Arc::new(ffprobe_path);
    let modified_for_summary = modified_count.clone();
    let _results = pool
        .process_batch(
            items,
            move |job| {
                let compiled = compiled.clone();
                let kernel = kernel.clone();
                let token = token_for_workers.clone();
                let ffprobe_path = ffprobe_path.clone();
                let capabilities = capabilities.clone();
                let modified_count = modified_count.clone();
                async move {
                    let ctx = ProcessContext {
                        compiled: &compiled,
                        kernel,
                        dry_run,
                        flag_size_increase,
                        token: &token,
                        ffprobe_path: ffprobe_path.as_deref(),
                        capabilities: &capabilities,
                        modified_count: &modified_count,
                    };
                    process_single_file(job, &ctx).await
                }
            },
            on_error,
            reporter.clone(),
        )
        .await;

    let modified = modified_for_summary.load(AtomicOrdering::Relaxed);
    if token.is_cancelled() {
        print_interrupted_summary(&pool, file_count, modified);
    } else {
        print_summary(&pool, file_count, modified, effective_workers, args.dry_run);
    }

    Ok(())
}

/// Load and compile the DSL policy file.
fn load_and_compile_policy(args: &ProcessArgs) -> Result<voom_dsl::CompiledPolicy> {
    let resolved = crate::config::resolve_policy_path(&args.policy);
    let policy_source = std::fs::read_to_string(&resolved)
        .with_context(|| format!("Failed to read policy: {}", resolved.display()))?;

    voom_dsl::compile_policy(&policy_source).context("policy compilation failed")
}

/// Print the header line describing what we are about to do.
fn print_run_header(policy_name: &str, path: &std::path::Path, dry_run: bool) {
    println!(
        "{} policy {} to {}{}",
        if dry_run {
            style("Dry-running").bold()
        } else {
            style("Applying").bold()
        },
        style(policy_name).cyan(),
        style(path.display()).cyan(),
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
    path: &std::path::Path,
    args: &ProcessArgs,
    kernel: &voom_kernel::Kernel,
) -> Result<Vec<voom_domain::events::FileDiscoveredEvent>> {
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let mut options = voom_discovery::ScanOptions::new(path.to_path_buf());
    options.hash_files = !args.no_backup;
    options.workers = args.workers;

    let events = discovery.scan(&options).context("filesystem scan failed")?;

    for event in &events {
        kernel.dispatch(Event::FileDiscovered(event.clone()));
    }

    Ok(events)
}

use crate::introspect::DiscoveredFilePayload;

/// Build work items from discovery events for the worker pool.
fn build_work_items(
    events: &[voom_domain::events::FileDiscoveredEvent],
) -> Vec<voom_job_manager::worker::WorkItem<DiscoveredFilePayload>> {
    events
        .iter()
        .map(|evt| {
            voom_job_manager::worker::WorkItem::new(
                voom_domain::job::JobType::Process,
                100,
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

/// Shared context for processing a single file.
struct ProcessContext<'a> {
    compiled: &'a voom_dsl::CompiledPolicy,
    kernel: Arc<voom_kernel::Kernel>,
    dry_run: bool,
    flag_size_increase: bool,
    token: &'a CancellationToken,
    ffprobe_path: Option<&'a str>,
    capabilities: &'a voom_domain::CapabilityMap,
    modified_count: &'a AtomicU64,
}

/// Process a single file: introspect, orchestrate, and (unless dry-run) execute plans.
async fn process_single_file(
    job: voom_domain::job::Job,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let payload = parse_job_payload(&job).map_err(|e| format!("job payload: {e}"))?;

    let path = std::path::PathBuf::from(&payload.path);

    let file = crate::introspect::introspect_file(
        path,
        payload.size,
        payload.content_hash,
        &ctx.kernel,
        ctx.ffprobe_path,
    )
    .await
    .map_err(|e| format!("introspect {}: {e}", payload.path))?;

    let result = orchestrate_plans(ctx.compiled, &file, ctx.capabilities);

    // Collect safeguard violations across all plans and tag the file
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
        ctx.kernel.dispatch(Event::FileIntrospected(
            voom_domain::events::FileIntrospectedEvent::new(tagged_file),
        ));
    }

    let needs_exec = voom_phase_orchestrator::PhaseOrchestratorPlugin::needs_execution(&result);

    if needs_exec {
        ctx.modified_count.fetch_add(1, AtomicOrdering::Relaxed);
    }

    if ctx.dry_run {
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
    } else {
        execute_plans(
            &file,
            &result,
            ctx.kernel.clone(),
            needs_exec,
            ctx.flag_size_increase,
            ctx.token,
            ctx.ffprobe_path,
        )
        .await
    }
}

/// Run the phase orchestrator to produce plans.
///
/// NOTE: This function does NOT dispatch `PlanCreated` events. The `execute_plans`
/// function dispatches them when it's time to actually execute. Dispatching
/// here would trigger executor plugins during dry-run mode.
fn orchestrate_plans(
    compiled: &voom_dsl::CompiledPolicy,
    file: &voom_domain::media::MediaFile,
    capabilities: &voom_domain::CapabilityMap,
) -> voom_phase_orchestrator::OrchestrationResult {
    let plans = voom_policy_evaluator::PolicyEvaluatorPlugin::new()
        .evaluate_with_capabilities(compiled, file, capabilities)
        .plans;
    let orchestrator = voom_phase_orchestrator::PhaseOrchestratorPlugin::new();
    orchestrator.orchestrate(plans)
}

/// Verify file integrity and publish plan lifecycle events for non-dry-run execution.
///
/// Each plan execution is offloaded to `spawn_blocking` because executor
/// plugins run subprocesses synchronously via `voom-process`, which would
/// otherwise block the tokio worker thread.
async fn execute_plans(
    file: &voom_domain::media::MediaFile,
    result: &voom_phase_orchestrator::OrchestrationResult,
    kernel: Arc<voom_kernel::Kernel>,
    needs_exec: bool,
    flag_size_increase: bool,
    token: &CancellationToken,
    ffprobe_path: Option<&str>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let file_path_str = file.path.display().to_string();

    // Verify file hasn't changed since introspection (TOCTOU guard)
    let exec_path = file.path.clone();
    if !file.content_hash.is_empty() {
        match voom_discovery::hash_file(&exec_path) {
            Ok(current_hash) if current_hash != file.content_hash => {
                tracing::warn!(path = %exec_path.display(), "file changed since introspection, skipping");
                return Ok(Some(serde_json::json!({
                    "path": file_path_str,
                    "skipped": true,
                    "reason": "file changed since introspection",
                })));
            }
            Err(e) => {
                tracing::warn!(path = %exec_path.display(), error = %e, "hash check failed, skipping");
                return Ok(Some(serde_json::json!({
                    "path": file_path_str,
                    "skipped": true,
                    "reason": format!("hash check failed: {e}"),
                })));
            }
            _ => {} // hash matches, proceed
        }
    }

    // Execute each non-skipped plan. PlanExecuting is dispatched first so
    // the backup-manager creates a backup before any executor modifies the
    // file. PlanCreated then triggers the actual execution.
    //
    // PlanCompleted/PlanFailed are dispatched *after* post-execution checks
    // (size-increase guard) so the backup is still available for restore.
    let mut any_executed = false;
    for plan in &result.plans {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }

        if token.is_cancelled() {
            break;
        }

        let plan_clone = plan.clone();
        let file_clone = file.clone();
        let kernel_clone = kernel.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            execute_single_plan(&plan_clone, &file_clone, &kernel_clone)
        })
        .await
        .map_err(|e| format!("plan execution join error: {e}"))?;

        match outcome {
            PlanOutcome::Success => {
                any_executed = true;

                // Size-increase guard: check *before* committing, while
                // the backup is still on disk.
                if flag_size_increase {
                    let check_path = resolve_post_execution_path(file, std::slice::from_ref(plan));
                    if let Ok(meta) = std::fs::metadata(&check_path) {
                        let new_size = meta.len();
                        if new_size > file.size && file.size > 0 {
                            tracing::warn!(
                                path = %check_path.display(),
                                before = file.size,
                                after = new_size,
                                "output larger than original, restoring"
                            );
                            // Remove the converted output before
                            // restoring so no orphan file is left
                            // behind (e.g. .mkv after mp4→mkv).
                            if check_path != file.path {
                                if let Err(e) = std::fs::remove_file(&check_path) {
                                    tracing::warn!(
                                        path = %check_path.display(),
                                        error = %e,
                                        "failed to remove converted output"
                                    );
                                }
                            }
                            kernel.dispatch(Event::PlanFailed(PlanFailedEvent::new(
                                plan.id,
                                file.path.clone(),
                                plan.phase_name.clone(),
                                format!("output grew from {} to {} bytes", file.size, new_size,),
                            )));
                            continue;
                        }
                    }
                }

                kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent::new(
                    plan.id,
                    file.path.clone(),
                    plan.phase_name.clone(),
                    plan.actions.len(),
                )));
            }
            PlanOutcome::Failed(failed) => {
                kernel.dispatch(Event::PlanFailed(failed));
            }
        }
    }

    // Re-introspect after execution so the database reflects the actual
    // file on disk (new container, path, tracks, etc.).
    if any_executed {
        let current_path = resolve_post_execution_path(file, &result.plans);
        if current_path.exists() {
            let size = std::fs::metadata(&current_path)
                .map(|m| m.len())
                .unwrap_or(file.size);
            let hash = voom_discovery::hash_file(&current_path).unwrap_or_default();
            let ffp = ffprobe_path.map(String::from);
            let kernel_clone = kernel.clone();
            let _ = crate::introspect::introspect_file(
                current_path,
                size,
                hash,
                &kernel_clone,
                ffp.as_deref(),
            )
            .await;
        }
    }

    Ok(Some(serde_json::json!({
        "path": file_path_str,
        "needs_execution": needs_exec,
        "plans_evaluated": result.plans.len(),
    })))
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
    // Find the last ConvertContainer action across all executed plans
    for plan in plans.iter().rev() {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }
        for action in &plan.actions {
            if action.operation == OperationType::ConvertContainer {
                if let voom_domain::plan::ActionParams::Container { container } = &action.parameters
                {
                    let new_path = file.path.with_extension(container.as_str());
                    if new_path.exists() {
                        return new_path;
                    }
                }
            }
        }
    }
    file.path.clone()
}

/// Result of executing a single plan via the event bus.
enum PlanOutcome {
    /// An executor claimed and completed the plan.
    Success,
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
    kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent::new(
        file.path.clone(),
        plan.phase_name.clone(),
        plan.actions.len(),
    )));

    let results = kernel.dispatch(Event::PlanCreated(PlanCreatedEvent::new(plan.clone())));

    let claimed = results.iter().any(|r| r.claimed);
    let exec_error = results.iter().find_map(|r| r.execution_error.clone());

    if claimed && exec_error.is_none() {
        PlanOutcome::Success
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
    println!(
        "\n{} {}/{} processed, {} modified, {} errors",
        style("Interrupted.").bold().yellow(),
        completed,
        file_count,
        modified,
        failed,
    );
}

/// Print the final summary line after processing.
fn print_summary(
    pool: &WorkerPool,
    file_count: usize,
    modified: u64,
    effective_workers: usize,
    dry_run: bool,
) {
    let completed = pool.completed_count();
    let failed = pool.failed_count();
    let skipped = (file_count as u64)
        .saturating_sub(completed)
        .saturating_sub(failed);

    let modified_label = if dry_run { "would modify" } else { "modified" };

    println!(
        "\n{} {} processed, {} {modified_label}, {} skipped, {} errors (workers: {})",
        style("Done.").bold().green(),
        style(completed).green(),
        style(modified).cyan(),
        style(skipped).dim(),
        if failed > 0 {
            style(failed).red().to_string()
        } else {
            failed.to_string()
        },
        effective_workers,
    );

    if dry_run {
        println!(
            "\n{}",
            style("This was a dry run. No files were modified.").dim()
        );
    }
}

/// Reporter that dispatches job lifecycle events through the kernel event bus.
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

/// CLI progress reporter using indicatif progress bars.
struct CliProgressReporter {
    _multi: MultiProgress,
    overall: ProgressBar,
}

impl CliProgressReporter {
    fn new(total: usize) -> Self {
        let multi = MultiProgress::new();
        let overall = multi.add(ProgressBar::new(total as u64));
        overall.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}",
            )
            .expect("valid progress template")
            .progress_chars("#>-"),
        );
        Self {
            _multi: multi,
            overall,
        }
    }
}

impl ProgressReporter for CliProgressReporter {
    fn on_batch_start(&self, _total: usize) {}

    fn on_job_start(&self, job: &voom_domain::job::Job) {
        if let Some(ref raw) = job.payload {
            if let Ok(payload) = serde_json::from_value::<DiscoveredFilePayload>(raw.clone()) {
                let max_name = max_filename_len(PROGRESS_FIXED_WIDTH);
                let filename = std::path::Path::new(&payload.path)
                    .file_name()
                    .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
                    .unwrap_or_default();
                self.overall.set_message(filename);
            }
        }
    }

    fn on_job_progress(&self, _id: uuid::Uuid, _progress: f64, _msg: Option<&str>) {}

    fn on_job_complete(&self, _id: uuid::Uuid, _success: bool, error: Option<&str>) {
        if let Some(err) = error {
            self.overall.suspend(|| {
                eprintln!("{} {err}", style("ERROR:").bold().red());
            });
        }
        self.overall.inc(1);
    }

    fn on_batch_complete(&self, _completed: u64, _failed: u64) {
        self.overall.finish_and_clear();
    }
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
    }

    impl PlanRecordingPlugin {
        fn new() -> Self {
            Self {
                discovered_count: AtomicUsize::new(0),
                introspected_count: AtomicUsize::new(0),
                plan_created_count: AtomicUsize::new(0),
                plan_executing_count: AtomicUsize::new(0),
                plan_completed_count: AtomicUsize::new(0),
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
        kernel.register_plugin(recorder.clone(), 50);

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

    #[tokio::test]
    async fn test_dispatch_plan_events_skips_skipped_plans() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let skipped_plan = test_plan("normalize", true);
        assert!(skipped_plan.is_skipped());

        let result =
            voom_phase_orchestrator::OrchestrationResult::new(vec![skipped_plan], vec![], false);

        let token = CancellationToken::new();
        let kernel = Arc::new(kernel);
        let _ = execute_plans(&file, &result, kernel, true, false, &token, None).await;

        // Skipped plans should NOT trigger any lifecycle events
        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_completed_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_discovery_and_introspection_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        // Simulate discovery events
        let discovered = FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 1024, "abc".into());
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
        kernel.register_plugin(recorder.clone(), 50);
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
    fn test_event_bus_reporter_batch_methods_are_noop() {
        let kernel = Arc::new(voom_kernel::Kernel::new());
        let reporter = EventBusReporter::new(kernel);
        // These should not panic
        reporter.on_batch_start(10);
        reporter.on_batch_complete(5, 0);
    }
}
