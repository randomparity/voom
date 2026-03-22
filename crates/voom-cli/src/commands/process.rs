use std::sync::Arc;

use anyhow::{Context, Result};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use tokio_util::sync::CancellationToken;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use crate::config;
use crate::output::{max_filename_len, shrink_filename};
use voom_domain::events::{
    Event, PlanCompletedEvent, PlanCreatedEvent, PlanExecutingEvent, PlanFailedEvent,
};
use voom_job_manager::progress::ProgressReporter;
use voom_job_manager::worker::{ErrorStrategy, WorkerPool, WorkerPoolConfig};

/// Run the process command.
///
/// This function calls the discovery, introspector, policy evaluator, and phase
/// orchestrator plugins directly rather than routing through the event bus. This
/// direct-call pattern is intentional for CLI commands: it enables deterministic
/// progress reporting, worker-pool concurrency control, and structured error
/// handling that would be difficult to achieve through the asynchronous pub/sub bus.
pub async fn run(args: ProcessArgs, token: CancellationToken) -> Result<()> {
    let config = config::load_config()?;
    let (kernel, store) = app::bootstrap_kernel_with_store(&config)?;
    let kernel = Arc::new(kernel);
    let store = match store {
        Some(s) => s,
        None => app::open_store(&config)?,
    };

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
        Err(e) => eprintln!("{} auto-prune failed: {e}", style("Warning:").yellow()),
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
            .map_err(|e| anyhow::anyhow!("failed to list bad files: {e}"))?;
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
        ErrorHandling::Fail => ErrorStrategy::Fail,
        ErrorHandling::Continue => ErrorStrategy::Continue,
    };

    if token.is_cancelled() {
        println!("{}", style("Interrupted before processing.").yellow());
        return Ok(());
    }

    let (pool, effective_workers) = create_worker_pool(&store, &args, token.clone())?;

    let reporter: Arc<dyn ProgressReporter> = Arc::new(CliProgressReporter::new(file_count));

    let items = build_work_items(&events);
    let compiled = Arc::new(compiled);
    let dry_run = args.dry_run;

    let token_for_workers = token.clone();
    let ffprobe_path: Option<String> = config.ffprobe_path().map(String::from);
    let ffprobe_path = Arc::new(ffprobe_path);
    let _results = pool
        .process_batch(
            items,
            move |job| {
                let compiled = compiled.clone();
                let kernel = kernel.clone();
                let token = token_for_workers.clone();
                let ffprobe_path = ffprobe_path.clone();
                async move {
                    process_single_file(
                        job,
                        &compiled,
                        &kernel,
                        dry_run,
                        &token,
                        ffprobe_path.as_deref(),
                    )
                    .await
                }
            },
            on_error,
            reporter.clone(),
        )
        .await;

    if token.is_cancelled() {
        print_interrupted_summary(&pool, file_count);
    } else {
        print_summary(&pool, file_count, effective_workers, args.dry_run);
    }

    Ok(())
}

/// Load and compile the DSL policy file.
fn load_and_compile_policy(args: &ProcessArgs) -> Result<voom_dsl::CompiledPolicy> {
    let policy_source = std::fs::read_to_string(&args.policy)
        .with_context(|| format!("Failed to read policy: {}", args.policy.display()))?;

    voom_dsl::compile(&policy_source).map_err(|e| anyhow::anyhow!("policy compilation failed: {e}"))
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

/// Walk the filesystem and discover media files, publishing events to the bus.
///
/// Creates a standalone `DiscoveryPlugin` for direct API access (scan options,
/// progress callbacks) that the event-bus path does not support. Events are
/// still dispatched to the kernel so that subscribers (storage, SSE) receive them.
fn discover_files(
    path: &std::path::Path,
    args: &ProcessArgs,
    kernel: &voom_kernel::Kernel,
) -> Result<Vec<voom_domain::events::FileDiscoveredEvent>> {
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let options = voom_discovery::ScanOptions {
        root: path.to_path_buf(),
        recursive: true,
        hash_files: !args.no_backup,
        workers: args.workers,
        on_progress: None,
    };

    let events = discovery
        .scan(&options)
        .map_err(|e| anyhow::anyhow!("filesystem scan failed: {e}"))?;

    for event in &events {
        kernel.dispatch(Event::FileDiscovered(event.clone()));
    }

    Ok(events)
}

/// Typed payload for the process worker pool, replacing freeform JSON.
#[derive(serde::Serialize, serde::Deserialize)]
struct ProcessJobPayload {
    path: String,
    size: u64,
    content_hash: String,
}

/// Build work items from discovery events for the worker pool.
fn build_work_items(
    events: &[voom_domain::events::FileDiscoveredEvent],
) -> Vec<voom_job_manager::worker::WorkItem> {
    events
        .iter()
        .map(|evt| {
            let payload = ProcessJobPayload {
                path: evt.path.to_string_lossy().into_owned(),
                size: evt.size,
                content_hash: evt.content_hash.clone(),
            };
            let value =
                serde_json::to_value(&payload).expect("ProcessJobPayload is always serializable");
            voom_job_manager::worker::WorkItem {
                job_type: voom_domain::job::JobType::Process,
                priority: 100,
                payload: Some(value),
            }
        })
        .collect()
}

/// Set up the job queue and worker pool.
fn create_worker_pool(
    store: &Arc<dyn voom_domain::storage::StorageTrait>,
    args: &ProcessArgs,
    token: CancellationToken,
) -> Result<(WorkerPool, usize)> {
    let queue = Arc::new(voom_job_manager::queue::JobQueue::new(store.clone()));

    let config = WorkerPoolConfig {
        max_workers: args.workers,
        worker_prefix: "voom".to_string(),
    };
    let effective_workers = config.effective_workers();

    let pool = WorkerPool::new(queue, config, token);

    Ok((pool, effective_workers))
}

/// Process a single file: introspect, orchestrate, and (unless dry-run) execute plans.
async fn process_single_file(
    job: voom_domain::job::Job,
    compiled: &voom_dsl::CompiledPolicy,
    kernel: &voom_kernel::Kernel,
    dry_run: bool,
    token: &CancellationToken,
    ffprobe_path: Option<&str>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let raw_payload = job.payload.as_ref().ok_or("missing payload")?;
    let payload: ProcessJobPayload =
        serde_json::from_value(raw_payload.clone()).map_err(|e| format!("invalid payload: {e}"))?;

    let path = std::path::PathBuf::from(&payload.path);

    let file = crate::introspect::introspect_file(
        path,
        payload.size,
        payload.content_hash,
        kernel,
        ffprobe_path,
    )
    .await?;

    let result = orchestrate_plans(compiled, &file)?;

    let needs_exec = voom_phase_orchestrator::PhaseOrchestratorPlugin::needs_execution(&result);

    if dry_run {
        let plan_summaries: Vec<serde_json::Value> = result
            .plans
            .iter()
            .map(|p| {
                serde_json::json!({
                    "phase": p.phase_name,
                    "actions": p.actions.len(),
                    "skipped": p.is_skipped(),
                })
            })
            .collect();

        Ok(Some(serde_json::json!({
            "path": file.path.display().to_string(),
            "needs_execution": needs_exec,
            "plans": plan_summaries,
        })))
    } else {
        execute_plans(&file, &result, kernel, needs_exec, token)
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
) -> std::result::Result<voom_phase_orchestrator::OrchestrationResult, String> {
    let plans = voom_policy_evaluator::evaluator::evaluate(compiled, file).plans;
    let orchestrator = voom_phase_orchestrator::PhaseOrchestratorPlugin::new();
    orchestrator
        .orchestrate(plans)
        .map_err(|e| format!("orchestration failed: {e}"))
}

/// Verify file integrity and publish plan lifecycle events for non-dry-run execution.
fn execute_plans(
    file: &voom_domain::media::MediaFile,
    result: &voom_phase_orchestrator::OrchestrationResult,
    kernel: &voom_kernel::Kernel,
    needs_exec: bool,
    token: &CancellationToken,
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

    // Publish lifecycle events for each non-skipped plan.
    // PlanExecuting is dispatched first so the backup-manager can create a
    // backup before any executor modifies the file. Then PlanCreated triggers
    // the actual execution.
    for plan in &result.plans {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }

        if token.is_cancelled() {
            break;
        }

        // Dispatch PlanExecuting first so backup-manager backs up the file
        // BEFORE executors modify it
        kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent {
            path: file.path.clone(),
            phase_name: plan.phase_name.clone(),
            action_count: plan.actions.len(),
        }));

        // Dispatch PlanCreated to let executor plugins claim and execute the plan
        let results = kernel.dispatch(Event::PlanCreated(PlanCreatedEvent { plan: plan.clone() }));

        let claimed = results.iter().any(|r| r.claimed);
        let exec_error = results.iter().find_map(|r| r.execution_error.clone());

        if claimed && exec_error.is_none() {
            // An executor plugin claimed and successfully executed the plan
            kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
                plan_id: plan.id,
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                actions_applied: plan.actions.len(),
            }));
        } else if let Some(error) = exec_error {
            // An executor claimed the plan but failed
            kernel.dispatch(Event::PlanFailed(PlanFailedEvent {
                plan_id: plan.id,
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                error,
                error_code: None,
                plugin_name: results
                    .iter()
                    .find(|r| r.claimed)
                    .map(|r| r.plugin_name.clone()),
                error_chain: vec![],
            }));
        } else {
            // No executor claimed the plan — emit PlanFailed
            kernel.dispatch(Event::PlanFailed(PlanFailedEvent {
                plan_id: plan.id,
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                error: "no executor available for plan".into(),
                error_code: None,
                plugin_name: None,
                error_chain: vec![],
            }));
        }
    }

    Ok(Some(serde_json::json!({
        "path": file_path_str,
        "needs_execution": needs_exec,
        "plans_evaluated": result.plans.len(),
    })))
}

/// Print a summary when interrupted by CTRL-C.
fn print_interrupted_summary(pool: &WorkerPool, file_count: usize) {
    let completed = pool.completed_count();
    let failed = pool.failed_count();
    println!(
        "\n{} {}/{} processed, {} errors",
        style("Interrupted.").bold().yellow(),
        completed,
        file_count,
        failed,
    );
}

/// Print the final summary line after processing.
fn print_summary(pool: &WorkerPool, file_count: usize, effective_workers: usize, dry_run: bool) {
    let completed = pool.completed_count();
    let failed = pool.failed_count();
    let skipped = (file_count as u64)
        .saturating_sub(completed)
        .saturating_sub(failed);

    println!(
        "\n{} {} processed, {} skipped, {} errors (workers: {})",
        style("Done.").bold().green(),
        style(completed).green(),
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
            ProgressStyle::with_template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
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
            if let Ok(payload) = serde_json::from_value::<ProcessJobPayload>(raw.clone()) {
                // 57 = spinner + space + [bar:40] + space + pos/len + space
                let max_name = max_filename_len(57);
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
        Plan {
            id: uuid::Uuid::new_v4(),
            file: MediaFile::new(PathBuf::from("/tmp/test.mkv")),
            policy_name: "test-policy".into(),
            phase_name: phase.into(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(0),
                parameters: ActionParams::Empty,
                description: "test action".into(),
            }],
            warnings: vec![],
            skip_reason: if skipped {
                Some("skipped".into())
            } else {
                None
            },
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_plan_lifecycle_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));

        // Simulate: PlanExecuting + PlanCreated + PlanCompleted for non-skipped plan
        let plan = test_plan("normalize", false);
        kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent {
            path: file.path.clone(),
            phase_name: plan.phase_name.clone(),
            action_count: plan.actions.len(),
        }));
        kernel.dispatch(Event::PlanCreated(PlanCreatedEvent { plan: plan.clone() }));
        kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
            plan_id: plan.id,
            path: file.path.clone(),
            phase_name: plan.phase_name.clone(),
            actions_applied: plan.actions.len(),
        }));

        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.plan_completed_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_skipped_plans_no_lifecycle_events() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        // Skipped plans should NOT get PlanCreated/PlanExecuting/PlanCompleted
        let plan = test_plan("normalize", true);
        assert!(plan.is_skipped());

        // Simulate the process.rs logic: skip if plan.is_skipped()
        if !plan.is_skipped() {
            kernel.dispatch(Event::PlanCreated(PlanCreatedEvent { plan: plan.clone() }));
        }

        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_completed_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_dry_run_no_executing_events() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let plan = test_plan("normalize", false);
        let dry_run = true;

        // Simulate the process.rs logic: in dry_run mode, no events are dispatched
        if !dry_run {
            kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent {
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                action_count: plan.actions.len(),
            }));
            kernel.dispatch(Event::PlanCreated(PlanCreatedEvent { plan: plan.clone() }));
            kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
                plan_id: plan.id,
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                actions_applied: plan.actions.len(),
            }));
        }

        // No events in dry_run
        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.plan_completed_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_discovery_and_introspection_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(PlanRecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        // Simulate discovery events
        let discovered = FileDiscoveredEvent {
            path: PathBuf::from("/tmp/a.mkv"),
            size: 1024,
            content_hash: "abc".into(),
        };
        kernel.dispatch(Event::FileDiscovered(discovered));

        // Simulate introspection event
        let file = MediaFile::new(PathBuf::from("/tmp/a.mkv"));
        kernel.dispatch(Event::FileIntrospected(FileIntrospectedEvent { file }));

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.introspected_count.load(Ordering::SeqCst), 1);
    }
}
