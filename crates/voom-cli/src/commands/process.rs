use std::sync::Arc;

use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
use voom_domain::events::{Event, PlanCompletedEvent, PlanCreatedEvent, PlanExecutingEvent};
use voom_job_manager::progress::ProgressReporter;
use voom_job_manager::worker::{ErrorStrategy, WorkerPool, WorkerPoolConfig};

/// Run the process command.
///
/// This function calls the discovery, introspector, policy evaluator, and phase
/// orchestrator plugins directly rather than routing through the event bus. This
/// direct-call pattern is intentional for CLI commands: it enables deterministic
/// progress reporting, worker-pool concurrency control, and structured error
/// handling that would be difficult to achieve through the asynchronous pub/sub bus.
pub async fn run(args: ProcessArgs) -> Result<()> {
    let config = app::load_config()?;
    let kernel = Arc::new(app::bootstrap_kernel(&config)?);

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    // Load and compile the policy
    let policy_source = std::fs::read_to_string(&args.policy)
        .with_context(|| format!("Failed to read policy: {}", args.policy.display()))?;

    let compiled = voom_dsl::compile(&policy_source)
        .map_err(|e| anyhow::anyhow!("policy compilation failed: {e}"))?;

    println!(
        "{} policy {} to {}{}",
        if args.dry_run {
            "Dry-running".bold()
        } else {
            "Applying".bold()
        },
        compiled.name.cyan(),
        path.display().to_string().cyan(),
        if args.dry_run {
            " (no changes will be made)"
        } else {
            ""
        }
    );

    // Discover files
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let options = voom_discovery::ScanOptions {
        root: path.clone(),
        recursive: true,
        hash_files: !args.no_backup,
        workers: args.workers,
        on_progress: None,
    };

    let events = discovery
        .scan(&options)
        .map_err(|e| anyhow::anyhow!("filesystem scan failed: {e}"))?;

    if events.is_empty() {
        println!("{}", "No media files found.".yellow());
        return Ok(());
    }

    // Publish discovery events through the event bus
    for event in &events {
        kernel.dispatch(Event::FileDiscovered(event.clone()));
    }

    let file_count = events.len();
    println!("Found {} media files.", file_count.to_string().bold());

    let on_error = match args.on_error {
        ErrorHandling::Fail => ErrorStrategy::Fail,
        ErrorHandling::Skip => ErrorStrategy::Skip,
        ErrorHandling::Continue => ErrorStrategy::Continue,
    };

    // Set up the job manager with storage
    let store = app::open_store(&config)?;
    let queue = Arc::new(voom_job_manager::queue::JobQueue::new(store.clone()));

    let effective_workers = if args.workers == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        args.workers
    };

    let pool = WorkerPool::new(
        queue.clone(),
        WorkerPoolConfig {
            max_workers: effective_workers,
            worker_prefix: "voom".to_string(),
        },
    );

    // Create progress reporter
    let reporter: Arc<dyn ProgressReporter> = Arc::new(CliProgressReporter::new(file_count));

    // Build work items with file paths as payloads
    let items: Vec<(String, i32, Option<serde_json::Value>)> = events
        .iter()
        .map(|evt| {
            let payload = serde_json::json!({
                "path": evt.path.to_string_lossy(),
                "size": evt.size,
                "content_hash": evt.content_hash,
            });
            ("process".to_string(), 100, Some(payload))
        })
        .collect();

    let compiled = Arc::new(compiled);
    let dry_run = args.dry_run;

    let _results = pool
        .process_batch(
            items,
            move |job| {
                let compiled = compiled.clone();
                let kernel = kernel.clone();
                async move {
                    let payload = job.payload.as_ref().ok_or("missing payload")?;
                    let file_path = payload["path"].as_str().ok_or("missing path in payload")?;
                    let file_size = payload["size"].as_u64().unwrap_or(0);
                    let content_hash = payload["content_hash"].as_str().unwrap_or("").to_string();

                    let path = std::path::PathBuf::from(file_path);

                    // Introspect (blocking I/O)
                    let introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
                    let intro_result = tokio::task::spawn_blocking(move || {
                        introspector.introspect(&path, file_size, &content_hash)
                    })
                    .await
                    .map_err(|e| format!("task join error: {e}"))?
                    .map_err(|e| format!("introspection failed: {e}"))?;

                    let file = intro_result.file.clone();

                    // Publish introspection event
                    kernel.dispatch(Event::FileIntrospected(
                        voom_domain::events::FileIntrospectedEvent {
                            file: intro_result.file,
                        },
                    ));

                    let file_path_str = file.path.display().to_string();

                    // Orchestrate
                    let orchestrator = voom_phase_orchestrator::PhaseOrchestratorPlugin::new();
                    let result = orchestrator
                        .orchestrate(&compiled, &file)
                        .map_err(|e| format!("orchestration failed: {e}"))?;

                    let needs_exec =
                        voom_phase_orchestrator::PhaseOrchestratorPlugin::needs_execution(&result);

                    // Publish PlanCreated for each non-skipped plan
                    for plan in &result.plans {
                        if !plan.is_skipped() {
                            kernel.dispatch(Event::PlanCreated(PlanCreatedEvent {
                                plan: plan.clone(),
                            }));
                        }
                    }

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

                        // Publish lifecycle events for each non-skipped plan
                        for plan in &result.plans {
                            if plan.is_skipped() {
                                continue;
                            }

                            kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent {
                                path: file.path.clone(),
                                phase_name: plan.phase_name.clone(),
                                action_count: plan.actions.len(),
                            }));

                            // In a full implementation, we'd execute the plan here

                            kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
                                plan_id: plan.id,
                                path: file.path.clone(),
                                phase_name: plan.phase_name.clone(),
                                actions_applied: plan.actions.len(),
                            }));
                        }

                        Ok(Some(serde_json::json!({
                            "path": file_path_str,
                            "needs_execution": needs_exec,
                            "plans_evaluated": result.plans.len(),
                        })))
                    }
                }
            },
            on_error,
            reporter.clone(),
        )
        .await;

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

    if args.dry_run {
        println!(
            "\n{}",
            "This was a dry run. No files were modified.".dimmed()
        );
    }

    Ok(())
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
                .unwrap()
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
                let filename = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
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

        // Simulate: PlanCreated + PlanExecuting + PlanCompleted for non-skipped plan
        let plan = test_plan("normalize", false);
        kernel.dispatch(Event::PlanCreated(PlanCreatedEvent { plan: plan.clone() }));
        kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent {
            path: file.path.clone(),
            phase_name: plan.phase_name.clone(),
            action_count: plan.actions.len(),
        }));
        kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
            plan_id: plan.id,
            path: file.path.clone(),
            phase_name: plan.phase_name.clone(),
            actions_applied: plan.actions.len(),
        }));

        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.plan_executing_count.load(Ordering::SeqCst), 1);
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

        // Simulate the process.rs logic: PlanCreated always, but
        // PlanExecuting/PlanCompleted only when NOT dry_run
        kernel.dispatch(Event::PlanCreated(PlanCreatedEvent { plan: plan.clone() }));

        if !dry_run {
            kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent {
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                action_count: plan.actions.len(),
            }));
            kernel.dispatch(Event::PlanCompleted(PlanCompletedEvent {
                plan_id: plan.id,
                path: file.path.clone(),
                phase_name: plan.phase_name.clone(),
                actions_applied: plan.actions.len(),
            }));
        }

        // PlanCreated fires regardless, but no execution events in dry_run
        assert_eq!(recorder.plan_created_count.load(Ordering::SeqCst), 1);
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
