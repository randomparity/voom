use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{
    Event, EventResult, PlanCompletedEvent, PlanExecutingEvent, PlanFailedEvent,
};
use voom_domain::media::Container;
use voom_domain::plan::{ActionResult, OperationType, Plan, PlannedAction};
use voom_kernel::Plugin;
use wait_timeout::ChildExt;

mod merge;
mod propedit;

/// Run a subprocess with a timeout, killing it if it exceeds the deadline.
fn run_with_timeout(tool: &str, args: &[impl AsRef<OsStr>], timeout: Duration) -> Result<Output> {
    let mut child = Command::new(tool)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VoomError::ToolExecution {
            tool: tool.into(),
            message: format!("failed to spawn {tool}: {e}"),
        })?;

    match child.wait_timeout(timeout) {
        Ok(Some(_status)) => child
            .wait_with_output()
            .map_err(|e| VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("failed to read output: {e}"),
            }),
        Ok(None) => {
            child.kill().ok();
            child.wait().ok();
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("{tool} timed out after {}s", timeout.as_secs()),
            })
        }
        Err(e) => Err(VoomError::ToolExecution {
            tool: tool.into(),
            message: format!("error waiting for {tool}: {e}"),
        }),
    }
}

/// Operations that can be handled via mkvpropedit (in-place metadata edits).
const PROPEDIT_OPS: &[OperationType] = &[
    OperationType::SetDefault,
    OperationType::ClearDefault,
    OperationType::SetForced,
    OperationType::ClearForced,
    OperationType::SetTitle,
    OperationType::SetLanguage,
    OperationType::SetContainerTag,
];

/// Operations that require mkvmerge (structural changes, remux).
const MERGE_OPS: &[OperationType] = &[
    OperationType::RemoveTrack,
    OperationType::ReorderTracks,
    OperationType::ConvertContainer,
];

/// All operations supported by this executor.
const SUPPORTED_OPS: &[OperationType] = &[
    OperationType::SetDefault,
    OperationType::ClearDefault,
    OperationType::SetForced,
    OperationType::ClearForced,
    OperationType::SetTitle,
    OperationType::SetLanguage,
    OperationType::SetContainerTag,
    OperationType::RemoveTrack,
    OperationType::ReorderTracks,
    OperationType::ConvertContainer,
];

/// MKVToolNix executor plugin.
///
/// Executes media plans using MKVToolNix command-line tools:
/// - **mkvpropedit** for in-place metadata operations (fast, no remux)
/// - **mkvmerge** for structural operations (track removal, reordering, container conversion)
///
/// Propedit actions are always run first since they operate in-place and are much faster.
/// Merge actions run second and produce a new file that replaces the original.
pub struct MkvtoolnixExecutorPlugin {
    capabilities: Vec<Capability>,
}

impl MkvtoolnixExecutorPlugin {
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Execute {
                operations: SUPPORTED_OPS
                    .iter()
                    .map(|op| op.as_str().to_string())
                    .collect(),
                formats: vec!["mkv".into()],
            }],
        }
    }

    /// Check whether this plugin can handle all operations in the given plan.
    ///
    /// Returns true if:
    /// - The file has an MKV container (or the plan includes ConvertContainer to MKV)
    /// - All actions use supported operation types
    pub fn can_handle(&self, plan: &Plan) -> bool {
        let is_mkv = plan.file.container == Container::Mkv;
        let is_convert_to_mkv = plan.actions.iter().any(|a| {
            a.operation == OperationType::ConvertContainer
                && a.parameters["target"].as_str() == Some("mkv")
        });

        if !is_mkv && !is_convert_to_mkv {
            return false;
        }

        plan.actions
            .iter()
            .all(|a| SUPPORTED_OPS.contains(&a.operation))
    }

    /// Execute all actions in a plan, returning results for each action.
    ///
    /// Propedit (metadata) actions run first, then merge (structural) actions.
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

        // Run propedit actions first (in-place, fast)
        if !propedit_actions.is_empty() {
            tracing::info!(
                path = %path.display(),
                count = propedit_actions.len(),
                "running propedit actions"
            );
            let propedit_results = propedit::execute_propedit_actions(path, &propedit_actions)?;
            results.extend(propedit_results);
        }

        // Run merge actions second (requires remux)
        if !merge_actions.is_empty() {
            tracing::info!(
                path = %path.display(),
                count = merge_actions.len(),
                "running merge actions"
            );
            let merge_results = merge::execute_merge_actions(path, &merge_actions)?;
            results.extend(merge_results);
        }

        Ok(results)
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

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == "plan.created"
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanCreated(plan_created) => {
                let plan = &plan_created.plan;

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

                let path = plan.file.path.clone();
                let phase_name = plan.phase_name.clone();

                // Emit executing event
                let executing_event = Event::PlanExecuting(PlanExecutingEvent {
                    path: path.clone(),
                    phase_name: phase_name.clone(),
                    action_count: plan.actions.len(),
                });

                match self.execute_plan(plan) {
                    Ok(results) => {
                        let actions_applied = results.iter().filter(|r| r.success).count();

                        let completed_event = Event::PlanCompleted(PlanCompletedEvent {
                            plan_id: plan.id,
                            path: path.clone(),
                            phase_name: phase_name.clone(),
                            actions_applied,
                        });

                        Ok(Some(EventResult {
                            plugin_name: self.name().to_string(),
                            produced_events: vec![executing_event, completed_event],
                            data: Some(serde_json::to_value(&results).unwrap_or_default()),
                        }))
                    }
                    Err(e) => {
                        let failed_event = Event::PlanFailed(PlanFailedEvent {
                            plan_id: plan.id,
                            path: path.clone(),
                            phase_name: phase_name.clone(),
                            error: e.to_string(),
                            error_code: None,
                            plugin_name: Some("mkvtoolnix-executor".into()),
                        });

                        Ok(Some(EventResult {
                            plugin_name: self.name().to_string(),
                            produced_events: vec![executing_event, failed_event],
                            data: None,
                        }))
                    }
                }
            }
            _ => Ok(None),
        }
    }
}

/// Classify actions into propedit (metadata) and merge (structural) groups.
fn classify_actions(actions: &[PlannedAction]) -> (Vec<&PlannedAction>, Vec<&PlannedAction>) {
    let mut propedit = Vec::new();
    let mut merge = Vec::new();

    for action in actions {
        if PROPEDIT_OPS.contains(&action.operation) {
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
        Plan {
            id: uuid::Uuid::new_v4(),
            file,
            policy_name: "test-policy".into(),
            phase_name: "normalize".into(),
            actions,
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        }
    }

    fn make_mp4_plan(actions: Vec<PlannedAction>) -> Plan {
        let mut file = MediaFile::new(PathBuf::from("/media/movie.mp4"));
        file.container = Container::Mp4;
        Plan {
            id: uuid::Uuid::new_v4(),
            file,
            policy_name: "test-policy".into(),
            phase_name: "normalize".into(),
            actions,
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        }
    }

    fn make_action(
        op: OperationType,
        track_index: Option<u32>,
        params: serde_json::Value,
    ) -> PlannedAction {
        PlannedAction {
            operation: op,
            track_index,
            parameters: params,
            description: format!("{:?} action", op),
        }
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        assert_eq!(plugin.name(), "mkvtoolnix-executor");
        assert_eq!(plugin.version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(plugin.capabilities().len(), 1);
        assert_eq!(plugin.capabilities()[0].kind(), "execute");
    }

    #[test]
    fn test_handles_plan_created() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        assert!(plugin.handles("plan.created"));
        assert!(!plugin.handles("file.discovered"));
        assert!(!plugin.handles("plan.completed"));
        assert!(!plugin.handles("plan.executing"));
    }

    #[test]
    fn test_can_handle_mkv() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let plan = make_mkv_plan(vec![
            make_action(OperationType::SetDefault, Some(1), serde_json::json!({})),
            make_action(
                OperationType::RemoveTrack,
                Some(3),
                serde_json::json!({"track_type": "subtitle_main"}),
            ),
        ]);
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_non_mkv() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let plan = make_mp4_plan(vec![make_action(
            OperationType::SetDefault,
            Some(1),
            serde_json::json!({}),
        )]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_convert_to_mkv() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let plan = make_mp4_plan(vec![make_action(
            OperationType::ConvertContainer,
            None,
            serde_json::json!({"target": "mkv"}),
        )]);
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_unsupported_op() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let plan = make_mkv_plan(vec![make_action(
            OperationType::TranscodeVideo,
            Some(0),
            serde_json::json!({"codec": "hevc"}),
        )]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_classify_actions() {
        let actions = vec![
            make_action(OperationType::SetDefault, Some(1), serde_json::json!({})),
            make_action(OperationType::ClearForced, Some(2), serde_json::json!({})),
            make_action(
                OperationType::RemoveTrack,
                Some(3),
                serde_json::json!({"track_type": "subtitle_main"}),
            ),
            make_action(
                OperationType::ReorderTracks,
                None,
                serde_json::json!({"order": [0, 1, 2]}),
            ),
            make_action(
                OperationType::SetLanguage,
                Some(1),
                serde_json::json!({"language": "eng"}),
            ),
        ];

        let (propedit, merge) = classify_actions(&actions);
        assert_eq!(propedit.len(), 3); // SetDefault, ClearForced, SetLanguage
        assert_eq!(merge.len(), 2); // RemoveTrack, ReorderTracks
    }

    #[test]
    fn test_on_event_ignores_non_plan_events() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent {
            path: PathBuf::from("/test.mkv"),
            size: 1024,
            content_hash: "abc".into(),
        });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_empty_plan() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let plan = make_mkv_plan(vec![]);
        let event = Event::PlanCreated(voom_domain::events::PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_skipped_plan() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let mut plan = make_mkv_plan(vec![make_action(
            OperationType::SetDefault,
            Some(1),
            serde_json::json!({}),
        )]);
        plan.skip_reason = Some("already correct".into());
        let event = Event::PlanCreated(voom_domain::events::PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_capability_operations() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let cap = &plugin.capabilities()[0];
        assert!(cap.supports_operation("set_default"));
        assert!(cap.supports_operation("remove_track"));
        assert!(cap.supports_operation("convert_container"));
        assert!(!cap.supports_operation("transcode_video"));
    }

    #[test]
    fn test_capability_format() {
        let plugin = MkvtoolnixExecutorPlugin::new();
        let cap = &plugin.capabilities()[0];
        assert!(cap.supports_format("mkv"));
        assert!(!cap.supports_format("mp4"));
    }
}
