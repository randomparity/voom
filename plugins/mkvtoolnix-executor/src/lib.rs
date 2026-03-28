//! `MKVToolNix` executor plugin: mkvmerge and mkvpropedit command building and execution.

pub mod merge;
pub mod propedit;
#[cfg(test)]
pub(crate) mod test_helpers;

use std::process::Command;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{CodecCapabilities, Event, EventResult, ExecutorCapabilitiesEvent};
use voom_domain::media::Container;
use voom_domain::plan::{ActionParams, ActionResult, OperationType, Plan, PlannedAction};
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
    op.is_metadata_op() || MERGE_OPS.contains(&op)
}

/// `MKVToolNix` executor plugin.
///
/// Executes media plans using `MKVToolNix` command-line tools:
/// - **mkvpropedit** for in-place metadata operations (fast, no remux)
/// - **mkvmerge** for structural operations (track removal, reordering, container conversion)
///
/// Propedit actions are always run first since they operate in-place and are much faster.
/// Merge actions run second and produce a new file that replaces the original.
/// Known input containers that MKVToolNix can remux.
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
            let mut converted_path = path.to_path_buf();
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
    fn name(&self) -> &str {
        "mkvtoolnix-executor"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::PLAN_CREATED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanCreated(plan_event) => self.handle_plan_created(plan_event),
            _ => Ok(None),
        }
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
        let available = Command::new("mkvmerge")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());

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
                crf: None,
                preset: None,
                bitrate: None,
                channels: None,
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
            "abc".into(),
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
