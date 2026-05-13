use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::bad_file::BadFileSource;
use crate::job::JobType;
use crate::media::MediaFile;
use crate::plan::{ActionResult, ExecutionDetail, Plan};

/// All event types that flow through the event bus.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // Plan embeds MediaFile — boxing would add indirection on every dispatch
pub enum Event {
    FileDiscovered(FileDiscoveredEvent),
    /// Emitted by streaming discovery once per root when that root's
    /// filesystem walk has completed. Carries the session id and elapsed
    /// walk duration. Consumers (the streaming process pipeline) use this
    /// to unlock the per-root execution gate.
    RootWalkCompleted(RootWalkCompletedEvent),
    FileIntrospected(FileIntrospectedEvent),
    FileIntrospectionFailed(FileIntrospectionFailedEvent),
    /// Emitted by WASM metadata plugins. Consumed by the sqlite-store plugin
    /// to persist enriched metadata as plugin data keyed by file path.
    MetadataEnriched(MetadataEnrichedEvent),
    /// Emitted by subtitle generator plugins when a forced subtitle file
    /// has been written. Executors subscribe to mux the SRT into the container.
    SubtitleGenerated(SubtitleGeneratedEvent),
    PlanCreated(PlanCreatedEvent),
    PlanExecuting(PlanExecutingEvent),
    PlanCompleted(PlanCompletedEvent),
    PlanSkipped(PlanSkippedEvent),
    PlanFailed(PlanFailedEvent),
    JobStarted(JobStartedEvent),
    JobProgress(JobProgressEvent),
    JobCompleted(JobCompletedEvent),
    /// Emitted by plugins that need to enqueue background jobs without a
    /// compile-time dependency on the job-manager crate.  The job-manager
    /// plugin subscribes to this event and performs the actual enqueue.
    JobEnqueueRequested(JobEnqueueRequestedEvent),
    /// Emitted by the tool-detector plugin. Consumed by the sqlite-store plugin
    /// to persist tool info, exposed via the web server's GET /api/tools endpoint.
    ToolDetected(ToolDetectedEvent),
    /// Emitted by executor plugins during `init()`. Reports probed codec,
    /// format, and hardware acceleration support for the underlying tool.
    ExecutorCapabilities(ExecutorCapabilitiesEvent),
    PluginError(PluginErrorEvent),
    HealthStatus(HealthStatusEvent),
    /// Emitted at the end of a scan. Consumed by the report plugin to
    /// auto-capture a library snapshot.
    ScanComplete(ScanCompleteEvent),
    /// Emitted at the end of a standalone introspection batch (one per
    /// `voom process` run). This is a **session-level** event, not a
    /// per-file signal — subscribers wanting per-file completion should
    /// use `Event::FileIntrospected` (`file.introspected`) instead.
    IntrospectSessionCompleted(IntrospectSessionCompletedEvent),
    /// Emitted by the retention runner after each scheduled, on-demand, or
    /// end-of-CLI prune pass. Includes per-table results and an overall duration.
    RetentionCompleted(RetentionCompletedEvent),
    /// Emitted by the verifier plugin after each verification run (quick,
    /// thorough, or hash). Carries the outcome, mode, and error/warning counts.
    VerifyCompleted(VerifyCompletedEvent),
    /// Emitted by the verifier plugin when a file is moved to quarantine.
    FileQuarantined(FileQuarantinedEvent),
}

impl Event {
    // ── Event type constants ────────────────────────────────────
    // Use these instead of string literals in Plugin::handles() implementations
    // to get compile-time typo protection.
    pub const FILE_DISCOVERED: &str = "file.discovered";
    pub const ROOT_WALK_COMPLETED: &str = "root.walk.completed";
    pub const FILE_INTROSPECTED: &str = "file.introspected";
    pub const FILE_INTROSPECTION_FAILED: &str = "file.introspection_failed";
    pub const METADATA_ENRICHED: &str = "metadata.enriched";
    pub const SUBTITLE_GENERATED: &str = "subtitle.generated";
    pub const PLAN_CREATED: &str = "plan.created";
    pub const PLAN_EXECUTING: &str = "plan.executing";
    pub const PLAN_COMPLETED: &str = "plan.completed";
    pub const PLAN_SKIPPED: &str = "plan.skipped";
    pub const PLAN_FAILED: &str = "plan.failed";
    pub const JOB_STARTED: &str = "job.started";
    pub const JOB_PROGRESS: &str = "job.progress";
    pub const JOB_COMPLETED: &str = "job.completed";
    pub const JOB_ENQUEUE_REQUESTED: &str = "job.enqueue_requested";
    pub const TOOL_DETECTED: &str = "tool.detected";
    pub const EXECUTOR_CAPABILITIES: &str = "executor.capabilities";
    pub const PLUGIN_ERROR: &str = "plugin.error";
    pub const HEALTH_STATUS: &str = "health.status";
    pub const SCAN_COMPLETE: &str = "scan.complete";
    pub const INTROSPECT_SESSION_COMPLETED: &str = "introspect.session.completed";
    pub const RETENTION_COMPLETED: &str = "retention.completed";
    pub const VERIFY_COMPLETED: &str = "verify.completed";
    pub const FILE_QUARANTINED: &str = "file.quarantined";

    /// One-line human-readable summary of the event payload.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use voom_domain::events::{Event, FileDiscoveredEvent};
    ///
    /// let event = Event::FileDiscovered(FileDiscoveredEvent::new(
    ///     PathBuf::from("/movies/test.mkv"),
    ///     1_500_000,
    ///     Some("abc123".into()),
    /// ));
    /// let summary = event.summary();
    /// assert!(summary.contains("/movies/test.mkv"));
    /// assert!(summary.contains("1500000"));
    /// ```
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn summary(&self) -> String {
        match self {
            Event::FileDiscovered(e) => {
                format!("path={} size={}", e.path.display(), e.size)
            }
            Event::RootWalkCompleted(e) => {
                format!(
                    "session={} root={} duration_ms={}",
                    e.session,
                    e.root.display(),
                    e.duration_ms
                )
            }
            Event::FileIntrospected(e) => {
                format!(
                    "path={} tracks={}",
                    e.file.path.display(),
                    e.file.tracks.len()
                )
            }
            Event::FileIntrospectionFailed(e) => {
                format!("path={} error={}", e.path.display(), e.error)
            }
            Event::PlanCreated(e) => {
                format!(
                    "phase={} actions={}",
                    e.plan.phase_name,
                    e.plan.actions.len()
                )
            }
            Event::PlanExecuting(e) => {
                format!("path={} phase={}", e.path.display(), e.phase_name)
            }
            Event::PlanCompleted(e) => {
                format!("path={} phase={}", e.path.display(), e.phase_name)
            }
            Event::PlanSkipped(e) => {
                format!(
                    "path={} phase={} reason={}",
                    e.path.display(),
                    e.phase_name,
                    e.skip_reason
                )
            }
            Event::PlanFailed(e) => {
                format!(
                    "path={} phase={} error={}",
                    e.path.display(),
                    e.phase_name,
                    e.error
                )
            }
            Event::JobStarted(e) => {
                format!("job_id={} desc={}", e.job_id, e.description)
            }
            Event::JobProgress(e) => {
                format!("job_id={} progress={:.1}%", e.job_id, e.progress * 100.0)
            }
            Event::JobCompleted(e) => {
                format!("job_id={} success={}", e.job_id, e.success)
            }
            Event::ToolDetected(e) => {
                format!("tool={} version={}", e.tool_name, e.version)
            }
            Event::MetadataEnriched(e) => {
                format!("path={} source={}", e.path.display(), e.source)
            }
            Event::SubtitleGenerated(e) => {
                format!(
                    "path={} subtitle={} lang={}",
                    e.path.display(),
                    e.subtitle_path.display(),
                    e.language
                )
            }
            Event::ExecutorCapabilities(e) => {
                format!(
                    "plugin={} decoders={} encoders={} formats={} hw={}",
                    e.plugin_name,
                    e.codecs.decoders.len(),
                    e.codecs.encoders.len(),
                    e.formats.len(),
                    e.hw_accels.len()
                )
            }
            Event::HealthStatus(e) => {
                format!("check={} passed={}", e.check_name, e.passed)
            }
            Event::PluginError(e) => {
                format!(
                    "plugin={} event={} error={}",
                    e.plugin_name, e.event_type, e.error
                )
            }
            Event::JobEnqueueRequested(e) => {
                format!(
                    "job_type={:?} priority={} requester={}",
                    e.job_type, e.priority, e.requester
                )
            }
            Event::ScanComplete(e) => {
                format!(
                    "files_discovered={} files_introspected={}",
                    e.files_discovered, e.files_introspected
                )
            }
            Event::IntrospectSessionCompleted(e) => {
                format!("files_introspected={}", e.files_introspected)
            }
            Event::RetentionCompleted(e) => {
                let total_deleted: u64 = e.per_table.iter().map(|t| t.deleted).sum();
                format!(
                    "trigger={:?} deleted={total_deleted} ms={}",
                    e.trigger, e.duration_ms
                )
            }
            Event::VerifyCompleted(e) => format!(
                "path={} mode={} outcome={} errors={} warnings={}",
                e.path.display(),
                e.mode.as_str(),
                e.outcome.as_str(),
                e.error_count,
                e.warning_count,
            ),
            Event::FileQuarantined(e) => format!(
                "from={} to={} reason={}",
                e.from.display(),
                e.to.display(),
                e.reason,
            ),
        }
    }

    /// Returns the event type string used for subscription matching.
    #[must_use]
    pub fn event_type(&self) -> &str {
        match self {
            Event::FileDiscovered(_) => Self::FILE_DISCOVERED,
            Event::RootWalkCompleted(_) => Self::ROOT_WALK_COMPLETED,
            Event::FileIntrospected(_) => Self::FILE_INTROSPECTED,
            Event::FileIntrospectionFailed(_) => Self::FILE_INTROSPECTION_FAILED,
            Event::MetadataEnriched(_) => Self::METADATA_ENRICHED,
            Event::SubtitleGenerated(_) => Self::SUBTITLE_GENERATED,
            Event::PlanCreated(_) => Self::PLAN_CREATED,
            Event::PlanExecuting(_) => Self::PLAN_EXECUTING,
            Event::PlanCompleted(_) => Self::PLAN_COMPLETED,
            Event::PlanSkipped(_) => Self::PLAN_SKIPPED,
            Event::PlanFailed(_) => Self::PLAN_FAILED,
            Event::JobStarted(_) => Self::JOB_STARTED,
            Event::JobProgress(_) => Self::JOB_PROGRESS,
            Event::JobCompleted(_) => Self::JOB_COMPLETED,
            Event::JobEnqueueRequested(_) => Self::JOB_ENQUEUE_REQUESTED,
            Event::ToolDetected(_) => Self::TOOL_DETECTED,
            Event::ExecutorCapabilities(_) => Self::EXECUTOR_CAPABILITIES,
            Event::PluginError(_) => Self::PLUGIN_ERROR,
            Event::HealthStatus(_) => Self::HEALTH_STATUS,
            Event::ScanComplete(_) => Self::SCAN_COMPLETE,
            Event::IntrospectSessionCompleted(_) => Self::INTROSPECT_SESSION_COMPLETED,
            Event::RetentionCompleted(_) => Self::RETENTION_COMPLETED,
            Event::VerifyCompleted(_) => Self::VERIFY_COMPLETED,
            Event::FileQuarantined(_) => Self::FILE_QUARANTINED,
        }
    }
}

/// Result returned by a plugin after processing an event.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventResult {
    pub plugin_name: String,
    pub produced_events: Vec<Event>,
    pub data: Option<serde_json::Value>,
    /// When `true`, the event bus stops dispatching this event to lower-priority
    /// handlers. Produced events from the claiming result still cascade normally.
    #[serde(default)]
    pub claimed: bool,
    /// Set when an executor claims a plan but fails to execute it.
    /// Allows callers to distinguish claimed+succeeded from claimed+failed
    /// without parsing the `data` JSON.
    #[serde(default)]
    pub execution_error: Option<String>,
    /// Subprocess output captured by the executor, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_detail: Option<ExecutionDetail>,
}

impl EventResult {
    #[must_use]
    pub fn new(plugin_name: impl Into<String>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![],
            data: None,
            claimed: false,
            execution_error: None,
            execution_detail: None,
        }
    }
}

impl EventResult {
    /// Attach subprocess execution detail to this result.
    #[must_use]
    pub fn with_execution_detail(mut self, detail: ExecutionDetail) -> Self {
        self.execution_detail = Some(detail);
        self
    }

    /// Build a result for executor plugins when a plan execution succeeds.
    ///
    /// Lifecycle events (`PlanExecuting`, `PlanCompleted`) are dispatched by the
    /// orchestrator in `process.rs`, not produced by executors, to avoid
    /// duplicate dispatches.
    #[must_use]
    pub fn plan_succeeded(plugin_name: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![],
            data,
            claimed: true,
            execution_error: None,
            execution_detail: None,
        }
    }

    /// Build a result for executor plugins when a plan execution fails.
    ///
    /// Lifecycle events (`PlanExecuting`, `PlanFailed`) are dispatched by the
    /// orchestrator in `process.rs`, not produced by executors, to avoid
    /// duplicate dispatches.
    #[must_use]
    pub fn plan_failed(plugin_name: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![],
            data: None,
            claimed: true,
            execution_error: Some(error.into()),
            execution_detail: None,
        }
    }

    /// Wrap the outcome of an executor's plan execution into an `EventResult`.
    ///
    /// On success the result carries the action results as JSON data and is
    /// marked as claimed. When all action results are failures (subprocess
    /// exited non-zero), a *failed* result is returned with the execution
    /// detail attached so it can be persisted. On `Err` (spawn/timeout with
    /// no detail) a failed result without detail is returned.
    #[must_use]
    pub fn from_plan_execution(
        plugin_name: &str,
        outcome: crate::errors::Result<Vec<ActionResult>>,
    ) -> Self {
        match outcome {
            Ok(results) => {
                let actions_applied = results.iter().filter(|r| r.success).count();
                let detail = results.iter().find_map(|r| r.execution_detail.clone());

                // All actions failed — treat as execution failure so the
                // detail propagates to PlanFailedEvent and gets persisted.
                if actions_applied == 0 && !results.is_empty() {
                    let error_msg = results
                        .iter()
                        .find_map(|r| r.error.clone())
                        .unwrap_or_else(|| "all actions failed".into());
                    let mut result = Self::plan_failed(plugin_name, error_msg);
                    if let Some(d) = detail {
                        result = result.with_execution_detail(d);
                    }
                    return result;
                }

                let mut result = Self::plan_succeeded(
                    plugin_name,
                    Some(serde_json::json!({
                        "actions_applied": actions_applied,
                        "results": serde_json::to_value(&results).unwrap_or_default(),
                    })),
                );
                result.execution_detail = detail;
                result
            }
            Err(e) => Self::plan_failed(plugin_name, e.to_string()),
        }
    }
}

// --- Event payload structs ---

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiscoveredEvent {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: Option<String>,
}

impl FileDiscoveredEvent {
    #[must_use]
    pub fn new(path: PathBuf, size: u64, content_hash: Option<String>) -> Self {
        Self {
            path,
            size,
            content_hash,
        }
    }
}

/// Payload of [`Event::RootWalkCompleted`]. Emitted exactly once per root
/// by streaming discovery.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootWalkCompletedEvent {
    pub root: PathBuf,
    pub session: crate::transition::ScanSessionId,
    pub duration_ms: u64,
}

impl RootWalkCompletedEvent {
    #[must_use]
    pub fn new(root: PathBuf, session: crate::transition::ScanSessionId, duration_ms: u64) -> Self {
        Self {
            root,
            session,
            duration_ms,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIntrospectedEvent {
    pub file: MediaFile,
}

impl FileIntrospectedEvent {
    #[must_use]
    pub fn new(file: MediaFile) -> Self {
        Self { file }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIntrospectionFailedEvent {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: Option<String>,
    pub error: String,
    pub error_source: BadFileSource,
}

impl FileIntrospectionFailedEvent {
    #[must_use]
    pub fn new(
        path: PathBuf,
        size: u64,
        content_hash: Option<String>,
        error: String,
        error_source: BadFileSource,
    ) -> Self {
        Self {
            path,
            size,
            content_hash,
            error,
            error_source,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEnrichedEvent {
    pub path: PathBuf,
    pub source: String,
    pub metadata: serde_json::Value,
}

impl MetadataEnrichedEvent {
    #[must_use]
    pub fn new(path: PathBuf, source: String, metadata: serde_json::Value) -> Self {
        Self {
            path,
            source,
            metadata,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleGeneratedEvent {
    pub path: PathBuf,
    pub subtitle_path: PathBuf,
    pub language: String,
    pub forced: bool,
    #[serde(default)]
    pub title: Option<String>,
    /// Scan session this subtitle was produced within. Propagated by
    /// producers (subtitle-generator plugins) so the downstream executor
    /// that muxes the subtitle into its container records a VOOM mutation
    /// for the rewrite. None when generation happens outside a scan session.
    #[serde(default)]
    pub scan_session: Option<crate::transition::ScanSessionId>,
}

impl SubtitleGeneratedEvent {
    #[must_use]
    pub fn new(
        path: PathBuf,
        subtitle_path: PathBuf,
        language: impl Into<String>,
        forced: bool,
    ) -> Self {
        Self {
            path,
            subtitle_path,
            language: language.into(),
            forced,
            title: None,
            scan_session: None,
        }
    }

    /// Attach a scan session ID to this event. Producers that emit
    /// `SubtitleGenerated` from within an active scan session must call
    /// this so downstream executors can record the rewrite as
    /// VOOM-originated.
    #[must_use]
    pub fn with_scan_session(mut self, scan_session: crate::transition::ScanSessionId) -> Self {
        self.scan_session = Some(scan_session);
        self
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCreatedEvent {
    pub plan: Plan,
}

impl PlanCreatedEvent {
    #[must_use]
    pub fn new(plan: Plan) -> Self {
        Self { plan }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecutingEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub action_count: usize,
}

impl PlanExecutingEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        action_count: usize,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            action_count,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCompletedEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub actions_applied: usize,
    /// When `true`, the backup-manager should retain the `.vbak` file
    /// instead of deleting it on completion.
    #[serde(default)]
    pub keep_backups: bool,
}

impl PlanCompletedEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        actions_applied: usize,
        keep_backups: bool,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            actions_applied,
            keep_backups,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSkippedEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub skip_reason: String,
}

impl PlanSkippedEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        skip_reason: impl Into<String>,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            skip_reason: skip_reason.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanFailedEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub error: String,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub plugin_name: Option<String>,
    /// Chain of causal errors from source to root cause.
    /// Populated when structured error information is available.
    #[serde(default)]
    pub error_chain: Vec<String>,
    /// Subprocess output captured by the executor, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_detail: Option<ExecutionDetail>,
}

impl PlanFailedEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            error: error.into(),
            error_code: None,
            plugin_name: None,
            error_chain: Vec::new(),
            execution_detail: None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStartedEvent {
    pub job_id: Uuid,
    pub description: String,
}

impl JobStartedEvent {
    #[must_use]
    pub fn new(job_id: Uuid, description: impl Into<String>) -> Self {
        Self {
            job_id,
            description: description.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgressEvent {
    pub job_id: Uuid,
    pub progress: f64,
    #[serde(default)]
    pub message: Option<String>,
}

impl JobProgressEvent {
    #[must_use]
    pub fn new(job_id: Uuid, progress: f64) -> Self {
        Self {
            job_id,
            progress,
            message: None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCompletedEvent {
    pub job_id: Uuid,
    pub success: bool,
    #[serde(default)]
    pub message: Option<String>,
}

impl JobCompletedEvent {
    #[must_use]
    pub fn new(job_id: Uuid, success: bool) -> Self {
        Self {
            job_id,
            success,
            message: None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEnqueueRequestedEvent {
    pub job_type: JobType,
    pub priority: i32,
    pub payload: Option<serde_json::Value>,
    pub requester: String,
}

impl JobEnqueueRequestedEvent {
    #[must_use]
    pub fn new(
        job_type: JobType,
        priority: i32,
        payload: Option<serde_json::Value>,
        requester: impl Into<String>,
    ) -> Self {
        Self {
            job_type,
            priority,
            payload,
            requester: requester.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDetectedEvent {
    pub tool_name: String,
    pub version: String,
    pub path: PathBuf,
}

impl ToolDetectedEvent {
    #[must_use]
    pub fn new(tool_name: impl Into<String>, version: impl Into<String>, path: PathBuf) -> Self {
        Self {
            tool_name: tool_name.into(),
            version: version.into(),
            path,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecCapabilities {
    pub decoders: Vec<String>,
    pub encoders: Vec<String>,
    /// Hardware-accelerated decoders (e.g. `hevc_cuvid`, `h264_qsv`).
    #[serde(default)]
    pub hw_decoders: Vec<String>,
    /// Hardware-accelerated encoders (e.g. `av1_nvenc`, `hevc_vaapi`).
    #[serde(default)]
    pub hw_encoders: Vec<String>,
}

impl CodecCapabilities {
    #[must_use]
    pub fn new(decoders: Vec<String>, encoders: Vec<String>) -> Self {
        Self {
            decoders,
            encoders,
            hw_decoders: vec![],
            hw_encoders: vec![],
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            decoders: vec![],
            encoders: vec![],
            hw_decoders: vec![],
            hw_encoders: vec![],
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorParallelLimit {
    pub resource: String,
    pub max_parallel: usize,
}

impl ExecutorParallelLimit {
    #[must_use]
    pub fn new(resource: impl Into<String>, max_parallel: usize) -> Self {
        Self {
            resource: resource.into(),
            max_parallel,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorCapabilitiesEvent {
    pub plugin_name: String,
    pub codecs: CodecCapabilities,
    pub formats: Vec<String>,
    pub hw_accels: Vec<String>,
    #[serde(default)]
    pub parallel_limits: Vec<ExecutorParallelLimit>,
}

impl ExecutorCapabilitiesEvent {
    #[must_use]
    pub fn new(
        plugin_name: impl Into<String>,
        codecs: CodecCapabilities,
        formats: Vec<String>,
        hw_accels: Vec<String>,
    ) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            codecs,
            formats,
            hw_accels,
            parallel_limits: vec![],
        }
    }

    #[must_use]
    pub fn with_parallel_limits(mut self, limits: Vec<ExecutorParallelLimit>) -> Self {
        self.parallel_limits = limits;
        self
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginErrorEvent {
    pub plugin_name: String,
    pub event_type: String,
    pub error: String,
}

impl PluginErrorEvent {
    #[must_use]
    pub fn new(
        plugin_name: impl Into<String>,
        event_type: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            event_type: event_type.into(),
            error: error.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatusEvent {
    pub check_name: String,
    pub passed: bool,
    pub details: Option<String>,
}

impl HealthStatusEvent {
    #[must_use]
    pub fn new(check_name: impl Into<String>, passed: bool, details: Option<String>) -> Self {
        Self {
            check_name: check_name.into(),
            passed,
            details,
        }
    }
}

/// Emitted exactly once at the end of a successful `scan` run, after
/// discovery + introspection finish. Carries both totals so subscribers
/// (currently just the report plugin's snapshot writer) do not need a
/// preceding `IntrospectSessionCompleted` to learn the introspected count.
///
/// **Do NOT also emit `IntrospectSessionCompleted` from the same run.** See
/// `crates/voom-cli/src/commands/scan.rs` and issue #153.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanCompleteEvent {
    pub files_discovered: u64,
    pub files_introspected: u64,
}

impl ScanCompleteEvent {
    #[must_use]
    pub fn new(files_discovered: u64, files_introspected: u64) -> Self {
        Self {
            files_discovered,
            files_introspected,
        }
    }
}

/// Emitted exactly once at the end of a standalone re-introspection batch
/// (currently only the `process` command, which re-introspects already-
/// discovered files before evaluating policies). Reserved for paths that
/// did NOT run discovery — full scans must emit `ScanComplete` instead.
///
/// This is a **session-level** event. Subscribers wanting a per-file
/// completion signal should use `FileIntrospectedEvent`
/// (`file.introspected`), which fires once per file. See issue #193.
///
/// The report plugin subscribes to this to auto-capture a library snapshot.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrospectSessionCompletedEvent {
    pub files_introspected: u64,
}

impl IntrospectSessionCompletedEvent {
    #[must_use]
    pub fn new(files_introspected: u64) -> Self {
        Self { files_introspected }
    }
}

/// Why retention ran.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetentionTrigger {
    /// Periodic task during `serve` mode.
    Scheduled,
    /// End-of-run hook from `voom scan` / `voom process`.
    CliEndOfRun,
    /// On-demand from `voom db prune`.
    OnDemand,
}

/// Result of pruning a single table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TableRetentionResult {
    pub table: String,
    pub deleted: u64,
    pub kept: u64,
    pub error: Option<String>,
}

impl TableRetentionResult {
    #[must_use]
    pub fn new(table: impl Into<String>, deleted: u64, kept: u64, error: Option<String>) -> Self {
        Self {
            table: table.into(),
            deleted,
            kept,
            error,
        }
    }
}

/// Emitted once per retention pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RetentionCompletedEvent {
    pub trigger: RetentionTrigger,
    pub per_table: Vec<TableRetentionResult>,
    pub duration_ms: u64,
}

impl RetentionCompletedEvent {
    #[must_use]
    pub fn new(
        trigger: RetentionTrigger,
        per_table: Vec<TableRetentionResult>,
        duration_ms: u64,
    ) -> Self {
        Self {
            trigger,
            per_table,
            duration_ms,
        }
    }
}

/// Emitted by the verifier plugin after each verification run.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyCompletedEvent {
    pub file_id: String,
    pub path: PathBuf,
    pub mode: crate::verification::VerificationMode,
    pub outcome: crate::verification::VerificationOutcome,
    pub error_count: u32,
    pub warning_count: u32,
    pub verification_id: Uuid,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct VerifyCompletedDetails {
    pub mode: crate::verification::VerificationMode,
    pub outcome: crate::verification::VerificationOutcome,
    pub error_count: u32,
    pub warning_count: u32,
    pub verification_id: Uuid,
}

impl VerifyCompletedDetails {
    #[must_use]
    pub fn new(
        mode: crate::verification::VerificationMode,
        outcome: crate::verification::VerificationOutcome,
        error_count: u32,
        warning_count: u32,
        verification_id: Uuid,
    ) -> Self {
        Self {
            mode,
            outcome,
            error_count,
            warning_count,
            verification_id,
        }
    }
}

impl VerifyCompletedEvent {
    #[must_use]
    pub fn new(file_id: impl Into<String>, path: PathBuf, details: VerifyCompletedDetails) -> Self {
        Self {
            file_id: file_id.into(),
            path,
            mode: details.mode,
            outcome: details.outcome,
            error_count: details.error_count,
            warning_count: details.warning_count,
            verification_id: details.verification_id,
        }
    }
}

/// Emitted by the verifier plugin when a file is moved to quarantine.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileQuarantinedEvent {
    pub file_id: String,
    pub from: PathBuf,
    pub to: PathBuf,
    pub reason: String,
}

impl FileQuarantinedEvent {
    #[must_use]
    pub fn new(
        file_id: impl Into<String>,
        from: PathBuf,
        to: PathBuf,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            file_id: file_id.into(),
            from,
            to,
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_strings() {
        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: PathBuf::from("/test.mkv"),
            size: 1024,
            content_hash: Some("abc".into()),
        });
        assert_eq!(event.event_type(), "file.discovered");

        let event = Event::PlanExecuting(PlanExecutingEvent {
            plan_id: Uuid::new_v4(),
            path: PathBuf::from("/test.mkv"),
            phase_name: "normalize".into(),
            action_count: 3,
        });
        assert_eq!(event.event_type(), "plan.executing");
    }

    #[test]
    fn test_event_serde_roundtrip() {
        let event = Event::ToolDetected(ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.1".into(),
            path: PathBuf::from("/usr/bin/ffprobe"),
        });
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "tool.detected");
    }

    #[test]
    fn test_event_msgpack_roundtrip() {
        let event = Event::JobProgress(JobProgressEvent {
            job_id: Uuid::new_v4(),
            progress: 0.75,
            message: Some("Processing...".into()),
        });
        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "job.progress");
    }

    #[test]
    fn test_job_progress_missing_optional_fields() {
        // Simulate deserializing from a payload that omits the optional `message` field.
        let id = Uuid::new_v4();
        let json = format!(r#"{{"job_id":"{id}","progress":0.5}}"#);
        let event: JobProgressEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.job_id, id);
        assert!(event.message.is_none());
    }

    #[test]
    fn test_plugin_error_event_serde_roundtrip() {
        let event = Event::PluginError(PluginErrorEvent {
            plugin_name: "bad-plugin".into(),
            event_type: "file.discovered".into(),
            error: "something went wrong".into(),
        });
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "plugin.error");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "plugin.error");
    }

    #[test]
    fn test_plan_failed_missing_optional_fields() {
        let json = r#"{"plan_id":"00000000-0000-0000-0000-000000000000","path":"/test.mkv","phase_name":"normalize","error":"failed"}"#;
        let event: PlanFailedEvent = serde_json::from_str(json).unwrap();
        assert!(event.error_code.is_none());
        assert!(event.plugin_name.is_none());
        assert!(event.execution_detail.is_none());
    }

    #[test]
    fn test_plan_failed_with_execution_detail_roundtrip() {
        use crate::plan::ExecutionDetail;

        let mut event = PlanFailedEvent::new(
            Uuid::nil(),
            PathBuf::from("/test.mkv"),
            "normalize",
            "ffmpeg exited with 1",
        );
        event.execution_detail = Some(ExecutionDetail {
            command: "ffmpeg -i /test.mkv out.mkv".into(),
            exit_code: Some(1),
            stderr_tail: "Error opening input".into(),
            duration_ms: 1234,
        });

        let json = serde_json::to_string(&event).unwrap();
        let restored: PlanFailedEvent = serde_json::from_str(&json).unwrap();
        assert!(restored.execution_detail.is_some());
        let detail = restored.execution_detail.unwrap();
        assert_eq!(detail.exit_code, Some(1));
        assert_eq!(detail.stderr_tail, "Error opening input");
        assert_eq!(detail.duration_ms, 1234);
    }

    #[test]
    fn test_file_introspection_failed_event_type() {
        let event = Event::FileIntrospectionFailed(FileIntrospectionFailedEvent {
            path: PathBuf::from("/test/bad.mkv"),
            size: 1024,
            content_hash: Some("abc".into()),
            error: "ffprobe failed".into(),
            error_source: BadFileSource::Introspection,
        });
        assert_eq!(event.event_type(), "file.introspection_failed");
    }

    #[test]
    fn test_file_introspection_failed_serde_roundtrip() {
        let event = Event::FileIntrospectionFailed(FileIntrospectionFailedEvent {
            path: PathBuf::from("/test/bad.mkv"),
            size: 2048,
            content_hash: None,
            error: "corrupt header".into(),
            error_source: BadFileSource::Parse,
        });
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "file.introspection_failed");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "file.introspection_failed");
    }

    #[test]
    fn test_job_enqueue_requested_serde_roundtrip() {
        use crate::job::JobType;

        let event = Event::JobEnqueueRequested(JobEnqueueRequestedEvent::new(
            JobType::Introspect,
            50,
            Some(serde_json::json!({"path": "/media/test.mkv"})),
            "ffprobe-introspector",
        ));
        assert_eq!(event.event_type(), "job.enqueue_requested");

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "job.enqueue_requested");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "job.enqueue_requested");
    }

    #[test]
    fn test_health_status_serde_roundtrip() {
        let event = Event::HealthStatus(HealthStatusEvent::new(
            "data_dir_writable",
            true,
            Some("/data/voom".into()),
        ));
        assert_eq!(event.event_type(), "health.status");

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "health.status");
        if let Event::HealthStatus(e) = &deserialized {
            assert_eq!(e.check_name, "data_dir_writable");
            assert!(e.passed);
            assert_eq!(e.details.as_deref(), Some("/data/voom"));
        } else {
            panic!("expected HealthStatus event");
        }

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "health.status");
        if let Event::HealthStatus(e) = &deserialized {
            assert_eq!(e.check_name, "data_dir_writable");
            assert!(e.passed);
        } else {
            panic!("expected HealthStatus event");
        }
    }

    #[test]
    fn test_subtitle_generated_event_type() {
        let event = Event::SubtitleGenerated(SubtitleGeneratedEvent::new(
            PathBuf::from("/media/movie.mkv"),
            PathBuf::from("/media/movie.forced-eng.srt"),
            "eng",
            true,
        ));
        assert_eq!(event.event_type(), "subtitle.generated");
    }

    #[test]
    fn test_subtitle_generated_json_roundtrip() {
        let event = Event::SubtitleGenerated(SubtitleGeneratedEvent::new(
            PathBuf::from("/media/movie.mkv"),
            PathBuf::from("/media/movie.forced-eng.srt"),
            "eng",
            true,
        ));
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "subtitle.generated");
        if let Event::SubtitleGenerated(e) = deserialized {
            assert_eq!(e.path, PathBuf::from("/media/movie.mkv"));
            assert_eq!(
                e.subtitle_path,
                PathBuf::from("/media/movie.forced-eng.srt")
            );
            assert_eq!(e.language, "eng");
            assert!(e.forced);
            assert!(e.title.is_none());
        } else {
            panic!("expected SubtitleGenerated variant");
        }
    }

    #[test]
    fn test_subtitle_generated_msgpack_roundtrip() {
        let event = Event::SubtitleGenerated(SubtitleGeneratedEvent::new(
            PathBuf::from("/media/movie.mkv"),
            PathBuf::from("/media/movie.forced-eng.srt"),
            "eng",
            true,
        ));
        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "subtitle.generated");
        if let Event::SubtitleGenerated(e) = deserialized {
            assert_eq!(e.path, PathBuf::from("/media/movie.mkv"));
            assert_eq!(
                e.subtitle_path,
                PathBuf::from("/media/movie.forced-eng.srt")
            );
            assert_eq!(e.language, "eng");
            assert!(e.forced);
        } else {
            panic!("expected SubtitleGenerated variant");
        }
    }

    #[test]
    fn test_subtitle_generated_missing_optional_title() {
        let json =
            r#"{"path":"/movie.mkv","subtitle_path":"/movie.srt","language":"eng","forced":true}"#;
        let event: SubtitleGeneratedEvent = serde_json::from_str(json).unwrap();
        assert!(event.title.is_none());
        assert!(event.forced);
        assert_eq!(event.language, "eng");
    }

    #[test]
    fn test_executor_capabilities_serde_roundtrip() {
        let event = Event::ExecutorCapabilities(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(
                vec!["h264".into(), "hevc".into()],
                vec!["libx264".into(), "libx265".into()],
            ),
            vec!["matroska".into(), "mp4".into()],
            vec!["videotoolbox".into()],
        ));
        assert_eq!(event.event_type(), "executor.capabilities");

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "executor.capabilities");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "executor.capabilities");
    }

    #[test]
    fn test_executor_capabilities_parallel_limits_serde_roundtrip() {
        let event = Event::ExecutorCapabilities(
            ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                CodecCapabilities::empty(),
                vec![],
                vec!["cuda".into()],
            )
            .with_parallel_limits(vec![ExecutorParallelLimit::new("hw:nvenc", 4)]),
        );

        let json = serde_json::to_string(&event).unwrap();
        let restored: Event = serde_json::from_str(&json).unwrap();

        let Event::ExecutorCapabilities(restored) = restored else {
            panic!("expected executor capabilities");
        };
        assert_eq!(restored.parallel_limits.len(), 1);
        assert_eq!(restored.parallel_limits[0].resource, "hw:nvenc");
        assert_eq!(restored.parallel_limits[0].max_parallel, 4);
    }

    #[test]
    fn test_executor_capabilities_legacy_payload_defaults_parallel_limits() {
        let json = r#"{
            "ExecutorCapabilities": {
                "plugin_name": "ffmpeg-executor",
                "codecs": {
                    "decoders": [],
                    "encoders": [],
                    "hw_decoders": [],
                    "hw_encoders": []
                },
                "formats": [],
                "hw_accels": ["cuda"]
            }
        }"#;

        let restored: Event = serde_json::from_str(json).unwrap();
        let Event::ExecutorCapabilities(restored) = restored else {
            panic!("expected executor capabilities");
        };
        assert!(restored.parallel_limits.is_empty());
    }

    #[test]
    fn test_executor_capabilities_empty_codecs() {
        let caps = CodecCapabilities::empty();
        assert!(caps.decoders.is_empty());
        assert!(caps.encoders.is_empty());

        let event = ExecutorCapabilitiesEvent::new(
            "mkvtoolnix-executor",
            caps,
            vec!["matroska".into()],
            vec![],
        );
        assert_eq!(event.plugin_name, "mkvtoolnix-executor");
        assert!(event.hw_accels.is_empty());
    }

    #[test]
    fn test_job_completed_missing_optional_fields() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{"job_id":"{id}","success":true}}"#);
        let event: JobCompletedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.job_id, id);
        assert!(event.success);
        assert!(event.message.is_none());
    }

    #[test]
    fn test_plan_skipped_event_type() {
        let event = Event::PlanSkipped(PlanSkippedEvent::new(
            Uuid::new_v4(),
            PathBuf::from("/test.mkv"),
            "transcode",
            "no GPU available",
        ));
        assert_eq!(event.event_type(), "plan.skipped");
    }

    #[test]
    fn test_scan_complete_event_type() {
        let event = Event::ScanComplete(ScanCompleteEvent {
            files_discovered: 42,
            files_introspected: 40,
        });
        assert_eq!(event.event_type(), "scan.complete");
        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.event_type(), "scan.complete");
    }

    #[test]
    fn test_introspect_session_completed_event_type() {
        let event = Event::IntrospectSessionCompleted(IntrospectSessionCompletedEvent {
            files_introspected: 15,
        });
        assert_eq!(event.event_type(), "introspect.session.completed");
        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.event_type(), "introspect.session.completed");
    }

    #[test]
    fn test_plan_skipped_serde_roundtrip() {
        let event = Event::PlanSkipped(PlanSkippedEvent::new(
            Uuid::new_v4(),
            PathBuf::from("/test.mkv"),
            "transcode",
            "skip_when condition met",
        ));
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "plan.skipped");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "plan.skipped");
    }

    #[test]
    fn retention_completed_event_type_and_summary() {
        let event = Event::RetentionCompleted(RetentionCompletedEvent {
            trigger: RetentionTrigger::Scheduled,
            per_table: vec![
                TableRetentionResult {
                    table: "jobs".into(),
                    deleted: 12,
                    kept: 34,
                    error: None,
                },
                TableRetentionResult {
                    table: "event_log".into(),
                    deleted: 5,
                    kept: 10,
                    error: None,
                },
            ],
            duration_ms: 42,
        });
        assert_eq!(event.event_type(), Event::RETENTION_COMPLETED);
        let s = event.summary();
        assert!(s.contains("deleted=17"), "got: {s}");
        assert!(s.contains("ms=42"), "got: {s}");
    }
}

#[cfg(test)]
mod verify_event_tests {
    use super::*;
    use crate::verification::{VerificationMode, VerificationOutcome};

    #[test]
    fn verify_completed_summary() {
        let ev = Event::VerifyCompleted(VerifyCompletedEvent::new(
            "file-id",
            PathBuf::from("/m/x.mkv"),
            VerifyCompletedDetails {
                mode: VerificationMode::Thorough,
                outcome: VerificationOutcome::Error,
                error_count: 3,
                warning_count: 1,
                verification_id: Uuid::nil(),
            },
        ));
        let s = ev.summary();
        assert!(s.contains("path=/m/x.mkv"));
        assert!(s.contains("mode=thorough"));
        assert!(s.contains("outcome=error"));
        assert!(s.contains("errors=3"));
        assert!(s.contains("warnings=1"));
        assert_eq!(ev.event_type(), Event::VERIFY_COMPLETED);
    }

    #[test]
    fn file_quarantined_summary() {
        let ev = Event::FileQuarantined(FileQuarantinedEvent::new(
            "file-id",
            PathBuf::from("/m/bad.mkv"),
            PathBuf::from("/q/bad.mkv"),
            "decode error",
        ));
        let s = ev.summary();
        assert!(s.contains("from=/m/bad.mkv"));
        assert!(s.contains("to=/q/bad.mkv"));
        assert!(s.contains("reason=decode error"));
        assert_eq!(ev.event_type(), Event::FILE_QUARANTINED);
    }

    #[test]
    fn verify_completed_event_json_roundtrip() {
        let ev = VerifyCompletedEvent::new(
            "f",
            PathBuf::from("/x.mkv"),
            VerifyCompletedDetails {
                mode: VerificationMode::Hash,
                outcome: VerificationOutcome::Ok,
                error_count: 0,
                warning_count: 0,
                verification_id: Uuid::nil(),
            },
        );
        let json = serde_json::to_string(&ev).unwrap();
        let back: VerifyCompletedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev.path, back.path);
        assert_eq!(ev.mode, back.mode);
        assert_eq!(ev.outcome, back.outcome);
        assert_eq!(ev.error_count, back.error_count);
    }

    #[test]
    fn file_quarantined_event_json_roundtrip() {
        let ev = FileQuarantinedEvent::new("f", PathBuf::from("/a"), PathBuf::from("/b"), "r");
        let json = serde_json::to_string(&ev).unwrap();
        let back: FileQuarantinedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev.from, back.from);
        assert_eq!(ev.to, back.to);
        assert_eq!(ev.reason, back.reason);
    }
}
