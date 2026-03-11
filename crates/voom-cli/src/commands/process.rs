use std::sync::Arc;

use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::{ErrorHandling, ProcessArgs};
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

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    // Load and compile the policy
    let policy_source = std::fs::read_to_string(&args.policy)
        .with_context(|| format!("Failed to read policy: {}", args.policy.display()))?;

    let compiled = voom_dsl::compile(&policy_source).map_err(|e| anyhow::anyhow!("{e}"))?;

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
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if events.is_empty() {
        println!("{}", "No media files found.".yellow());
        return Ok(());
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

                    let file = intro_result.file;
                    let file_path_str = file.path.display().to_string();

                    // Orchestrate
                    let orchestrator = voom_phase_orchestrator::PhaseOrchestratorPlugin::new();
                    let result = orchestrator
                        .orchestrate(&compiled, &file)
                        .map_err(|e| format!("orchestration failed: {e}"))?;

                    let needs_exec =
                        voom_phase_orchestrator::PhaseOrchestratorPlugin::needs_execution(&result);

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
                        // In a full implementation, we'd execute the plans here
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
