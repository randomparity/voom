//! `MKVToolNix` executor plugin: mkvmerge and mkvpropedit command building and execution.

pub mod merge;
pub mod propedit;
#[cfg(test)]
pub(crate) mod test_helpers;

use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{
    CodecCapabilities, Event, EventResult, ExecutorCapabilitiesEvent, PlanExecutingEvent,
};
use voom_domain::media::Container;
use voom_domain::plan::{ActionParams, ActionResult, OperationType, Plan, PlannedAction};
use voom_domain::temp_file::temp_path_with_ext;
use voom_domain::utils::language::is_valid_language;
use voom_domain::utils::sanitize::validate_metadata_value;
use voom_kernel::{Plugin, PluginContext};

// Propedit (in-place metadata) operations are identified by
// `OperationType::is_metadata_op()` — defined once in voom-domain.

/// Operations that require mkvmerge (structural changes, remux).
const MERGE_OPS: &[OperationType] = &[
    OperationType::RemoveTrack,
    OperationType::ReorderTracks,
    OperationType::ConvertContainer,
];

/// Check whether the given operation type is supported by this executor.
fn is_supported_op(op: OperationType) -> bool {
    op.is_metadata_op() || MERGE_OPS.contains(&op) || op == OperationType::MuxSubtitle
}

/// `MKVToolNix` executor plugin.
///
/// Executes media plans using `MKVToolNix` command-line tools:
/// - **mkvpropedit** for in-place metadata operations (fast, no remux)
/// - **mkvmerge** for structural operations (track removal, reordering, container conversion)
///
/// Propedit actions are always run first since they operate in-place and are much faster.
/// Merge actions run second and produce a new file that replaces the original.
/// Known input containers that `MKVToolNix` can remux.
const MKVTOOLNIX_FORMATS: &[&str] = &[
    "ass", "avi", "flac", "flv", "matroska", "mov", "mp4", "mpeg", "ogm", "srt", "ssa", "wav",
    "webm",
];

pub struct MkvtoolnixExecutorPlugin {
    capabilities: Vec<Capability>,
    available: bool,
}

impl MkvtoolnixExecutorPlugin {
    /// Create a new executor plugin. The plugin starts with `available = false`
    /// and must be initialized via `init()` to probe for mkvmerge on PATH.
    #[must_use]
    pub fn new() -> Self {
        let mut operations: Vec<OperationType> = OperationType::METADATA_OPS.to_vec();
        operations.extend_from_slice(MERGE_OPS);
        Self {
            capabilities: vec![Capability::Execute {
                operations,
                formats: vec!["mkv".into()],
            }],
            available: false,
        }
    }

    /// Create a plugin with `available` set to the given value.
    /// Bypasses the `init()` probe for testing.
    #[cfg(test)]
    fn with_available(mut self, available: bool) -> Self {
        self.available = available;
        self
    }

    /// Check whether this plugin can handle all operations in the given plan.
    ///
    /// Requires `init()` to have set `available = true` (mkvmerge found on PATH).
    /// Returns `false` if the plugin is unavailable.
    ///
    /// Returns true if:
    /// - The file has an MKV container (or the plan includes `ConvertContainer` to MKV)
    /// - All actions use supported operation types
    #[must_use]
    pub fn can_handle(&self, plan: &Plan) -> bool {
        if !self.available {
            return false;
        }

        let is_mkv = plan.file.container == Container::Mkv;
        let is_convert_to_mkv = plan.actions.iter().any(|a| {
            a.operation == OperationType::ConvertContainer
                && matches!(&a.parameters, ActionParams::Container { container } if *container == Container::Mkv)
        });

        if !is_mkv && !is_convert_to_mkv {
            return false;
        }

        plan.actions.iter().all(|a| is_supported_op(a.operation))
    }

    /// Execute all actions in a plan, returning results for each action.
    ///
    /// For MKV files: propedit (metadata) actions run first, then merge (structural).
    /// For non-MKV files being converted to MKV: merge runs first (to create the MKV),
    /// then propedit operates on the resulting file.
    pub fn execute_plan(&self, plan: &Plan) -> Result<Vec<ActionResult>> {
        let path = &plan.file.path;

        if !path.exists() {
            return Err(VoomError::ToolExecution {
                tool: "mkvtoolnix".into(),
                message: format!("file not found: {}", path.display()),
            });
        }

        // Handle MuxSubtitle actions separately
        if plan
            .actions
            .iter()
            .any(|a| a.operation == OperationType::MuxSubtitle)
        {
            let mut results = Vec::new();
            for action in plan
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::MuxSubtitle)
            {
                results.extend(self.execute_mux_subtitle(path, action)?);
            }
            return Ok(results);
        }

        // Classify actions
        let (propedit_actions, merge_actions) = classify_actions(&plan.actions);

        let mut results = Vec::new();

        // For non-MKV source files, merge must run first to create the MKV
        // before propedit can operate on it.
        let is_mkv = plan.file.container == Container::Mkv;
        let propedit_first = is_mkv || merge_actions.is_empty();

        if propedit_first {
            // MKV source: propedit first (in-place, fast), then merge
            if !propedit_actions.is_empty() {
                tracing::info!(
                    path = %path.display(),
                    count = propedit_actions.len(),
                    "running propedit actions"
                );
                let propedit_results = propedit::execute_propedit_actions(path, &propedit_actions)?;
                results.extend(propedit_results);
            }

            if !merge_actions.is_empty() {
                tracing::info!(
                    path = %path.display(),
                    count = merge_actions.len(),
                    "running merge actions"
                );
                let merge_results = merge::execute_merge_actions(path, &merge_actions)?;
                results.extend(merge_results);
            }
        } else {
            // Non-MKV source: merge first (convert to MKV), then propedit
            // on the resulting .mkv file (merge removes the original).
            let mut converted_path = path.clone();
            tracing::info!(
                path = %path.display(),
                count = merge_actions.len(),
                "running merge actions (convert to MKV first)"
            );
            let merge_results = merge::execute_merge_actions(path, &merge_actions)?;
            results.extend(merge_results);

            // After ConvertContainer, the file is now at the .mkv path
            converted_path.set_extension("mkv");

            if !propedit_actions.is_empty() {
                tracing::info!(
                    path = %converted_path.display(),
                    count = propedit_actions.len(),
                    "running propedit actions (on converted MKV)"
                );
                let propedit_results =
                    propedit::execute_propedit_actions(&converted_path, &propedit_actions)?;
                results.extend(propedit_results);
            }
        }

        Ok(results)
    }

    /// Handle a `subtitle.generated` event by converting it into a
    /// `PlanCreated` event with a `MuxSubtitle` action.
    ///
    /// Emits `PlanExecuting` before `PlanCreated` so that backup-manager
    /// creates a backup before the executor modifies the file.
    fn handle_subtitle_generated(
        &self,
        event: &voom_domain::events::SubtitleGeneratedEvent,
    ) -> Result<Option<EventResult>> {
        if !self.available {
            return Ok(None);
        }

        // Only handle MKV files
        let ext = event
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if Container::from_extension(ext) != Container::Mkv {
            return Ok(None);
        }

        validate_metadata_value(&event.language).map_err(|e| VoomError::ToolExecution {
            tool: "mkvmerge".into(),
            message: format!("invalid language: {e}"),
        })?;
        if let Some(title) = &event.title {
            validate_metadata_value(title).map_err(|e| VoomError::ToolExecution {
                tool: "mkvmerge".into(),
                message: format!("invalid title: {e}"),
            })?;
        }

        let phase_name = "subtitle_mux";
        let mut file = voom_domain::media::MediaFile::new(event.path.clone());
        file.container = Container::Mkv;
        let mut plan = Plan::new(file, "subtitle-mux", phase_name);
        plan.actions = vec![PlannedAction::file_op(
            OperationType::MuxSubtitle,
            ActionParams::MuxSubtitle {
                subtitle_path: event.subtitle_path.clone(),
                language: event.language.clone(),
                forced: event.forced,
                title: event.title.clone(),
            },
            "Mux subtitle into container",
        )];

        let produced_events = vec![
            Event::PlanExecuting(PlanExecutingEvent::new(
                plan.id,
                event.path.clone(),
                phase_name,
                1,
            )),
            Event::PlanCreated(voom_domain::events::PlanCreatedEvent::new(plan)),
        ];

        let mut result = EventResult::new(self.name());
        result.claimed = true;
        result.produced_events = produced_events;
        Ok(Some(result))
    }

    /// Execute a `MuxSubtitle` action by running mkvmerge.
    fn execute_mux_subtitle(
        &self,
        path: &std::path::Path,
        action: &PlannedAction,
    ) -> Result<Vec<ActionResult>> {
        let ActionParams::MuxSubtitle {
            subtitle_path,
            language,
            forced,
            title,
        } = &action.parameters
        else {
            return Err(VoomError::ToolExecution {
                tool: "mkvmerge".into(),
                message: "expected MuxSubtitle params".into(),
            });
        };

        if !is_valid_language(language) {
            return Err(VoomError::ToolExecution {
                tool: "mkvmerge".into(),
                message: format!("invalid ISO 639 language code: \"{language}\""),
            });
        }

        let tmp = temp_path_with_ext(path, "mkv");
        let _guard = scopeguard::guard(tmp.clone(), |p| {
            let _ = std::fs::remove_file(&p);
        });
        let mut args = vec![
            "-o".to_string(),
            tmp.to_string_lossy().into_owned(),
            "--language".to_string(),
            format!("0:{language}"),
            "--forced-display-flag".to_string(),
            format!("0:{}", i32::from(*forced)),
        ];
        if let Some(title) = title {
            args.push("--track-name".to_string());
            args.push(format!("0:{title}"));
        }
        args.push(path.to_string_lossy().into_owned());
        args.push(subtitle_path.to_string_lossy().into_owned());

        let command_str = voom_process::shell_quote_args("mkvmerge", &args);
        const SUBTITLE_MUX_TIMEOUT: Duration = Duration::from_secs(120);
        let start = std::time::Instant::now();
        let output =
            voom_process::run_with_timeout_env("mkvmerge", &args, SUBTITLE_MUX_TIMEOUT, &[]);
        let duration_ms = start.elapsed().as_millis() as u64;

        match output {
            Ok(o) if o.status.success() || o.status.code() == Some(1) => {
                std::fs::rename(&tmp, path).map_err(|e| VoomError::ToolExecution {
                    tool: "mkvmerge".into(),
                    message: format!("failed to rename temp file: {e}"),
                })?;
                // Defuse guard — temp file was successfully renamed
                scopeguard::ScopeGuard::into_inner(_guard);
                let detail = voom_domain::plan::ExecutionDetail {
                    command: command_str,
                    exit_code: o.status.code(),
                    // exit code 1 = mkvmerge warnings; capture stderr for diagnostics
                    stderr_tail: voom_process::stderr_tail(&o.stderr, 20),
                    duration_ms,
                };
                Ok(vec![ActionResult::success(
                    action.operation,
                    &action.description,
                )
                .with_execution_detail(detail)])
            }
            Ok(o) => {
                let tail = voom_process::stderr_tail(&o.stderr, 20);
                let display_tail = if tail.is_empty() {
                    "(no output)"
                } else {
                    &tail
                };
                let error_msg = format!(
                    "mkvmerge failed (exit {}):\n{}\ncmd: {}",
                    o.status.code().unwrap_or(-1),
                    display_tail,
                    command_str
                );
                let detail = voom_domain::plan::ExecutionDetail {
                    command: command_str,
                    exit_code: o.status.code(),
                    stderr_tail: tail,
                    duration_ms,
                };
                Ok(vec![ActionResult::failure(
                    action.operation,
                    &action.description,
                    &error_msg,
                )
                .with_execution_detail(detail)])
            }
            Err(e) => Err(VoomError::ToolExecution {
                tool: "mkvmerge".into(),
                message: format!("mkvmerge subtitle mux failed: {e}"),
            }),
        }
    }

    /// Handle a `plan.created` event.
    fn handle_plan_created(
        &self,
        plan_event: &voom_domain::events::PlanCreatedEvent,
    ) -> Result<Option<EventResult>> {
        let plan = &plan_event.plan;

        // Skip empty or already-skipped plans
        if plan.is_empty() || plan.is_skipped() {
            return Ok(None);
        }

        // Check if we can handle this plan
        if !self.can_handle(plan) {
            tracing::debug!(
                path = %plan.file.path.display(),
                phase = %plan.phase_name,
                "plan not handled by mkvtoolnix executor"
            );
            return Ok(None);
        }

        Ok(Some(EventResult::from_plan_execution(
            self.name(),
            self.execute_plan(plan),
        )))
    }
}

impl Default for MkvtoolnixExecutorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for MkvtoolnixExecutorPlugin {
    fn name(&self) -> &'static str {
        "mkvtoolnix-executor"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        if self.available {
            &self.capabilities
        } else {
            &[]
        }
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::PLAN_CREATED || event_type == Event::SUBTITLE_GENERATED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanCreated(plan_event) => self.handle_plan_created(plan_event),
            Event::SubtitleGenerated(e) => self.handle_subtitle_generated(e),
            _ => Ok(None),
        }
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
        let available =
            voom_process::probe_tool_status("mkvmerge", &["--version"], Duration::from_secs(10));

        self.available = available;

        if !available {
            tracing::warn!("mkvmerge not found; mkvtoolnix executor disabled");
            return Ok(vec![]);
        }

        let formats: Vec<String> = MKVTOOLNIX_FORMATS
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        let event = ExecutorCapabilitiesEvent::new(
            "mkvtoolnix-executor",
            CodecCapabilities::empty(),
            formats,
            vec![],
        );

        Ok(vec![Event::ExecutorCapabilities(event)])
    }
}

/// Classify actions into propedit (metadata) and merge (structural) groups.
fn classify_actions(actions: &[PlannedAction]) -> (Vec<&PlannedAction>, Vec<&PlannedAction>) {
    let mut propedit = Vec::new();
    let mut merge = Vec::new();

    for action in actions {
        if action.operation.is_metadata_op() {
            propedit.push(action);
        } else if MERGE_OPS.contains(&action.operation) {
            merge.push(action);
        } else {
            tracing::warn!(
                operation = ?action.operation,
                "unsupported operation in mkvtoolnix executor"
            );
        }
    }

    (propedit, merge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::MediaFile;

    fn make_mkv_plan(actions: Vec<PlannedAction>) -> Plan {
        let mut file = MediaFile::new(PathBuf::from("/media/movie.mkv"));
        file.container = Container::Mkv;
        {
            let mut plan = Plan::new(file, "test-policy", "normalize");
            plan.actions = actions;
            plan
        }
    }

    fn make_mp4_plan(actions: Vec<PlannedAction>) -> Plan {
        let mut file = MediaFile::new(PathBuf::from("/media/movie.mp4"));
        file.container = Container::Mp4;
        {
            let mut plan = Plan::new(file, "test-policy", "normalize");
            plan.actions = actions;
            plan
        }
    }

    use crate::test_helpers::make_action;
    use voom_domain::plan::ActionParams;

    #[test]
    fn test_plugin_metadata() {
        let plugin = MkvtoolnixExecutorPlugin::new().with_available(true);
        assert_eq!(plugin.name(), "mkvtoolnix-executor");
        assert_eq!(plugin.version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(plugin.capabilities().len(), 1);
        assert_eq!(plugin.capabilities()[0].kind(), "execute");
    }

    #[test]
    fn test_handles_plan_created() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        assert!(plugin.handles(Event::PLAN_CREATED));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::PLAN_COMPLETED));
        assert!(!plugin.handles(Event::PLAN_EXECUTING));
    }

    #[test]
    fn test_handles_subtitle_generated() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        assert!(plugin.handles(Event::SUBTITLE_GENERATED));
    }

    #[test]
    fn test_subtitle_generated_non_mkv_returns_none() {
        let plugin = MkvtoolnixExecutorPlugin::new().with_available(true);
        let event = Event::SubtitleGenerated(voom_domain::events::SubtitleGeneratedEvent::new(
            PathBuf::from("/media/movie.mp4"),
            PathBuf::from("/media/movie.forced-eng.srt"),
            "eng",
            true,
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_can_handle_mkv() {
        let plugin = MkvtoolnixExecutorPlugin::new().with_available(true);
        let plan = make_mkv_plan(vec![
            make_action(OperationType::SetDefault, Some(1), ActionParams::Empty),
            make_action(
                OperationType::RemoveTrack,
                Some(3),
                ActionParams::RemoveTrack {
                    reason: "test".into(),
                    track_type: voom_domain::media::TrackType::SubtitleMain,
                },
            ),
        ]);
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_non_mkv() {
        let plugin = MkvtoolnixExecutorPlugin::new().with_available(true);
        let plan = make_mp4_plan(vec![make_action(
            OperationType::SetDefault,
            Some(1),
            ActionParams::Empty,
        )]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_convert_to_mkv() {
        let plugin = MkvtoolnixExecutorPlugin::new().with_available(true);
        let plan = make_mp4_plan(vec![make_action(
            OperationType::ConvertContainer,
            None,
            ActionParams::Container {
                container: Container::Mkv,
            },
        )]);
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_unsupported_op() {
        let plugin = MkvtoolnixExecutorPlugin::new().with_available(true);
        let plan = make_mkv_plan(vec![make_action(
            OperationType::TranscodeVideo,
            Some(0),
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: Default::default(),
            },
        )]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_unavailable() {
        let plugin = MkvtoolnixExecutorPlugin::new(); // available defaults to false
        let plan = make_mkv_plan(vec![make_action(
            OperationType::SetDefault,
            Some(1),
            ActionParams::Empty,
        )]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_classify_actions() {
        let actions = vec![
            make_action(OperationType::SetDefault, Some(1), ActionParams::Empty),
            make_action(OperationType::ClearForced, Some(2), ActionParams::Empty),
            make_action(
                OperationType::RemoveTrack,
                Some(3),
                ActionParams::RemoveTrack {
                    reason: "test".into(),
                    track_type: voom_domain::media::TrackType::SubtitleMain,
                },
            ),
            make_action(
                OperationType::ReorderTracks,
                None,
                ActionParams::ReorderTracks {
                    order: vec!["0".into(), "1".into(), "2".into()],
                },
            ),
            make_action(
                OperationType::SetLanguage,
                Some(1),
                ActionParams::Language {
                    language: "eng".into(),
                },
            ),
        ];

        let (propedit, merge) = classify_actions(&actions);
        assert_eq!(propedit.len(), 3); // SetDefault, ClearForced, SetLanguage
        assert_eq!(merge.len(), 2); // RemoveTrack, ReorderTracks
    }

    #[test]
    fn test_on_event_ignores_non_plan_events() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/test.mkv"),
            1024,
            Some("abc".into()),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_empty_plan() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let plan = make_mkv_plan(vec![]);
        let event = Event::PlanCreated(voom_domain::events::PlanCreatedEvent::new(plan));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_skipped_plan() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let mut plan = make_mkv_plan(vec![make_action(
            OperationType::SetDefault,
            Some(1),
            ActionParams::Empty,
        )]);
        plan.skip_reason = Some("already correct".into());
        let event = Event::PlanCreated(voom_domain::events::PlanCreatedEvent::new(plan));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }
}
