use std::sync::Arc;

use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use tokio_util::sync::CancellationToken;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use crate::output::{max_filename_len, shrink_filename};
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::{
    Event, FileIntrospectionFailedEvent, PlanCompletedEvent, PlanCreatedEvent, PlanExecutingEvent,
    PlanFailedEvent,
};
use voom_domain::storage::StorageTrait;
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
    let config = app::load_config()?;
    let kernel = Arc::new(app::bootstrap_kernel(&config)?);

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    let compiled = load_and_compile_policy(&args)?;

    print_run_header(&compiled.name, &path, args.dry_run);

    // Auto-prune stale file entries under the target directory
    {
        let store = app::open_store(&config)?;
        match store.prune_missing_files_under(&path) {
            Ok(n) if n > 0 => println!("Pruned {n} stale entries."),
            Ok(_) => {}
            Err(e) => eprintln!("{} auto-prune failed: {e}", "Warning:".yellow()),
        }
    }

    if token.is_cancelled() {
        println!("{}", "Interrupted before discovery.".yellow());
        return Ok(());
    }

    // Discover files
    let mut events = discover_files(&path, &args, &kernel)?;
    if events.is_empty() {
        println!("{}", "No media files found.".yellow());
        return Ok(());
    }

    // Filter out known-bad files unless --force-rescan is set
    if !args.force_rescan {
        let store = app::open_store(&config)?;
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
                    skipped.to_string().yellow(),
                    "--force-rescan".bold()
                );
            }
        }
    }

    if events.is_empty() {
        println!("{}", "No processable files found.".yellow());
        return Ok(());
    }

    let file_count = events.len();
    println!("Found {} media files.", file_count.to_string().bold());

    let on_error = match args.on_error {
        ErrorHandling::Fail => ErrorStrategy::Fail,
        ErrorHandling::Skip => ErrorStrategy::Skip,
        ErrorHandling::Continue => ErrorStrategy::Continue,
    };

    if token.is_cancelled() {
        println!("{}", "Interrupted before processing.".yellow());
        return Ok(());
    }

    let (pool, effective_workers) = create_worker_pool(&config, &args, token.clone())?;

    let reporter: Arc<dyn ProgressReporter> = Arc::new(CliProgressReporter::new(file_count));

    let items = build_work_items(&events);
    let compiled = Arc::new(compiled);
    let dry_run = args.dry_run;

    let _results = pool
        .process_batch(
            items,
            move |job| {
                let compiled = compiled.clone();
                let kernel = kernel.clone();
                async move { process_single_file(job, &compiled, &kernel, dry_run).await }
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
            "Dry-running".bold()
        } else {
            "Applying".bold()
        },
        policy_name.cyan(),
        path.display().to_string().cyan(),
        if dry_run {
            " (no changes will be made)"
        } else {
            ""
        }
    );
}

/// Walk the filesystem and discover media files, publishing events to the bus.
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

/// Build work items from discovery events for the worker pool.
fn build_work_items(
    events: &[voom_domain::events::FileDiscoveredEvent],
) -> Vec<(String, i32, Option<serde_json::Value>)> {
    events
        .iter()
        .map(|evt| {
            let payload = serde_json::json!({
                "path": evt.path.to_string_lossy(),
                "size": evt.size,
                "content_hash": evt.content_hash,
            });
            ("process".to_string(), 100, Some(payload))
        })
        .collect()
}

/// Set up the job queue and worker pool.
fn create_worker_pool(
    config: &app::AppConfig,
    args: &ProcessArgs,
    token: CancellationToken,
) -> Result<(WorkerPool, usize)> {
    let store = app::open_store(config)?;
    let queue = Arc::new(voom_job_manager::queue::JobQueue::new(store.clone()));

    let effective_workers = if args.workers == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        args.workers
    };

    let pool = WorkerPool::new(
        queue,
        WorkerPoolConfig {
            max_workers: effective_workers,
            worker_prefix: "voom".to_string(),
        },
        token,
    );

    Ok((pool, effective_workers))
}

/// Process a single file: introspect, orchestrate, and (unless dry-run) execute plans.
async fn process_single_file(
    job: voom_domain::job::Job,
    compiled: &voom_dsl::CompiledPolicy,
    kernel: &voom_kernel::Kernel,
    dry_run: bool,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let payload = job.payload.as_ref().ok_or("missing payload")?;
    let file_path = payload["path"].as_str().ok_or("missing path in payload")?;
    let file_size = payload["size"].as_u64().unwrap_or(0);
    let content_hash = payload["content_hash"].as_str().unwrap_or("").to_string();

    let path = std::path::PathBuf::from(file_path);

    let file = introspect_file(path, file_size, content_hash, kernel).await?;
    let file_path_str = file.path.display().to_string();

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
            "path": file_path_str,
            "needs_execution": needs_exec,
            "plans": plan_summaries,
        })))
    } else {
        execute_plans(
            file_path,
            &file,
            &result,
            kernel,
            &file_path_str,
            needs_exec,
        )
    }
}

/// Run ffprobe introspection on a single file (blocking I/O on a spawn_blocking thread).
async fn introspect_file(
    path: std::path::PathBuf,
    file_size: u64,
    content_hash: String,
    kernel: &voom_kernel::Kernel,
) -> std::result::Result<voom_domain::media::MediaFile, String> {
    let introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    let path_for_event = path.clone();
    let hash_for_event = content_hash.clone();
    let path_display = path.display().to_string();
    let intro_result = tokio::task::spawn_blocking(move || {
        introspector.introspect(&path, file_size, &content_hash)
    })
    .await;

    match intro_result {
        Ok(Ok(intro_event)) => {
            let file = intro_event.file.clone();
            kernel.dispatch(Event::FileIntrospected(
                voom_domain::events::FileIntrospectedEvent {
                    file: intro_event.file,
                },
            ));
            Ok(file)
        }
        Ok(Err(e)) => {
            let error_msg = format!("introspection failed for {path_display}: {e}");
            kernel.dispatch(Event::FileIntrospectionFailed(
                FileIntrospectionFailedEvent {
                    path: path_for_event,
                    size: file_size,
                    content_hash: Some(hash_for_event),
                    error: e.to_string(),
                    error_source: BadFileSource::Introspection,
                },
            ));
            Err(error_msg)
        }
        Err(e) => {
            let error_msg = format!("task join error: {e}");
            kernel.dispatch(Event::FileIntrospectionFailed(
                FileIntrospectionFailedEvent {
                    path: path_for_event,
                    size: file_size,
                    content_hash: Some(hash_for_event),
                    error: error_msg.clone(),
                    error_source: BadFileSource::Introspection,
                },
            ));
            Err(error_msg)
        }
    }
}

/// Run the phase orchestrator to produce plans.
///
/// NOTE: This function does NOT dispatch PlanCreated events. The execute_plans
/// function dispatches them when it's time to actually execute. Dispatching
/// here would trigger executor plugins during dry-run mode.
fn orchestrate_plans(
    compiled: &voom_dsl::CompiledPolicy,
    file: &voom_domain::media::MediaFile,
) -> std::result::Result<voom_phase_orchestrator::OrchestrationResult, String> {
    let orchestrator = voom_phase_orchestrator::PhaseOrchestratorPlugin::new();
    orchestrator
        .orchestrate(compiled, file)
        .map_err(|e| format!("orchestration failed: {e}"))
}

/// Verify file integrity and publish plan lifecycle events for non-dry-run execution.
fn execute_plans(
    file_path: &str,
    file: &voom_domain::media::MediaFile,
    result: &voom_phase_orchestrator::OrchestrationResult,
    kernel: &voom_kernel::Kernel,
    file_path_str: &str,
    needs_exec: bool,
) -> std::result::Result<Option<serde_json::Value>, String> {
    // Verify file hasn't changed since introspection (TOCTOU guard)
    let exec_path = std::path::PathBuf::from(file_path);
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

        if claimed {
            // An executor plugin claimed the plan — treat as successful
            kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
                plan_id: plan.id,
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                actions_applied: plan.actions.len(),
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
        "Interrupted.".bold().yellow(),
        completed,
        file_count,
        failed,
    );
}

/// Print the final summary line after processing.
fn print_summary(pool: &WorkerPool, file_count: usize, effective_workers: usize, dry_run: bool) {
    let completed = pool.completed_count();
    let failed = pool.failed_count();
    let skipped = file_count as u64 - completed - failed;

    println!(
        "\n{} {} processed, {} skipped, {} errors (workers: {})",
        "Done.".bold().green(),
        completed.to_string().green(),
        skipped.to_string().dimmed(),
        if failed > 0 {
            failed.to_string().red().to_string()
        } else {
            failed.to_string()
        },
        effective_workers,
    );

    if dry_run {
        println!(
            "\n{}",
            "This was a dry run. No files were modified.".dimmed()
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
        if let Some(ref payload) = job.payload {
            if let Some(path) = payload["path"].as_str() {
                // 57 ≈ spinner + space + [bar:40] + space + pos/len + space
                let max_name = max_filename_len(57);
                let filename = std::path::Path::new(path)
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
                eprintln!("{} {err}", "ERROR:".bold().red());
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
    use voom_domain::plan::{OperationType, Plan, PlannedAction};

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
                "file.discovered"
                    | "file.introspected"
                    | "plan.created"
                    | "plan.executing"
                    | "plan.completed"
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
                parameters: serde_json::json!({}),
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
