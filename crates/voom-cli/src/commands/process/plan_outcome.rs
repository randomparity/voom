//! Plan execution outcome reconstruction.
//!
//! After dispatching a `PlanCreated` event, the caller receives a
//! `Vec<EventResult>` from the kernel. The decision of whether the plan
//! succeeded, failed, or went unclaimed requires scanning the results for
//! several fields (`claimed`, `execution_error`, `execution_detail`,
//! `plugin_name`). This module concentrates that scan into a single
//! constructor so the caller does not perform three separate passes.

use voom_domain::events::{Event, EventResult, PlanFailedEvent};
use voom_domain::plan::{OperationType, PhaseOutput, Plan};

/// Result of executing a single plan via the event bus.
pub(super) enum PlanOutcome {
    /// An executor claimed and completed the plan.
    Success {
        executor: String,
        phase_output: PhaseOutput,
    },
    /// Execution failed (executor error or unclaimed).
    Failed(PlanFailedEvent),
}

impl PlanOutcome {
    /// Build a `PlanOutcome` from the `EventResult` vec returned by a
    /// `PlanCreated` dispatch.
    ///
    /// Performs a single scan of the results, distinguishing three cases:
    /// 1. A plugin claimed the plan with no error → `Success`
    /// 2. A plugin reported an execution error → `Failed` with executor name
    ///    attached (if any plugin also claimed it)
    /// 3. No plugin claimed the plan → `Failed` with "no executor available"
    pub(super) fn from_event_result(
        results: &[EventResult],
        plan: &Plan,
        file: &voom_domain::media::MediaFile,
    ) -> Self {
        let mut claimed_name: Option<&str> = None;
        let mut exec_error: Option<String> = None;
        let mut exec_detail: Option<voom_domain::plan::ExecutionDetail> = None;
        let mut produced_events = Vec::new();

        for r in results {
            produced_events.extend(r.produced_events.iter().cloned());
            if r.claimed && claimed_name.is_none() {
                claimed_name = Some(&r.plugin_name);
            }
            if exec_error.is_none() {
                if let Some(ref e) = r.execution_error {
                    exec_error = Some(e.clone());
                }
            }
            if exec_detail.is_none() {
                if let Some(ref d) = r.execution_detail {
                    exec_detail = Some(d.clone());
                }
            }
        }

        if let Some(name) = claimed_name {
            if exec_error.is_none() {
                return Self::Success {
                    executor: name.to_string(),
                    phase_output: phase_output_from_success(plan, &produced_events),
                };
            }
        }

        if let Some(error) = exec_error {
            let mut failed =
                PlanFailedEvent::new(plan.id, file.path.clone(), plan.phase_name.clone(), error);
            failed.plugin_name = claimed_name.map(ToString::to_string);
            failed.execution_detail = exec_detail;
            return Self::Failed(failed);
        }

        Self::Failed(PlanFailedEvent::new(
            plan.id,
            file.path.clone(),
            plan.phase_name.clone(),
            "no executor available for plan",
        ))
    }
}

fn phase_output_from_success(plan: &Plan, produced_events: &[Event]) -> PhaseOutput {
    let mut output = PhaseOutput::new()
        .with_completed(true)
        .with_modified(phase_modifies_file(plan));

    for event in produced_events {
        let Event::VerifyCompleted(verify) = event else {
            continue;
        };
        if verify.file_id != plan.file.id.to_string() {
            continue;
        }
        output = output
            .with_outcome(verify.outcome.as_str())
            .with_error_count(verify.error_count)
            .with_warning_count(verify.warning_count);
    }

    output
}

fn phase_modifies_file(plan: &Plan) -> bool {
    plan.actions
        .iter()
        .any(|action| action.operation != OperationType::VerifyMedia)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use voom_domain::events::{Event, EventResult, VerifyCompletedDetails, VerifyCompletedEvent};
    use voom_domain::media::MediaFile;
    use voom_domain::plan::{ActionParams, PlannedAction, VerifyMediaParams};
    use voom_domain::verification::{VerificationMode, VerificationOutcome};

    use super::*;

    fn plan_and_file() -> (Plan, MediaFile) {
        let file = MediaFile::new(PathBuf::from("/movies/example.mkv"));
        let plan = Plan::new(file.clone(), "policy", "metadata");
        (plan, file)
    }

    #[test]
    fn claimed_success_preserves_executor_name() {
        let (plan, file) = plan_and_file();
        let results = [EventResult::plan_succeeded("ffmpeg-executor", None)];

        let outcome = PlanOutcome::from_event_result(&results, &plan, &file);

        match outcome {
            PlanOutcome::Success { executor, .. } => assert_eq!(executor, "ffmpeg-executor"),
            PlanOutcome::Failed(_) => panic!("expected success outcome"),
        }
    }

    #[test]
    fn claimed_verify_success_preserves_phase_output_counts() {
        let (mut plan, file) = plan_and_file();
        plan.actions.push(PlannedAction::file_op(
            OperationType::VerifyMedia,
            ActionParams::VerifyMedia(VerifyMediaParams {
                mode: VerificationMode::Quick,
            }),
            "verify media",
        ));
        let produced = Event::VerifyCompleted(VerifyCompletedEvent::new(
            plan.file.id.to_string(),
            plan.file.path.clone(),
            VerifyCompletedDetails::new(
                VerificationMode::Quick,
                VerificationOutcome::Warning,
                1,
                2,
                uuid::Uuid::new_v4(),
            ),
        ));
        let mut result = EventResult::plan_succeeded("ffmpeg-executor", None);
        result.produced_events = vec![produced];

        let outcome = PlanOutcome::from_event_result(&[result], &plan, &file);

        match outcome {
            PlanOutcome::Success { phase_output, .. } => {
                assert!(phase_output.completed);
                assert!(!phase_output.modified);
                assert_eq!(phase_output.outcome.as_deref(), Some("warning"));
                assert_eq!(phase_output.error_count, 1);
                assert_eq!(phase_output.warning_count, 2);
            }
            PlanOutcome::Failed(_) => panic!("expected success outcome"),
        }
    }

    #[test]
    fn claimed_failure_preserves_executor_and_error() {
        let (plan, file) = plan_and_file();
        let results = [EventResult::plan_failed("ffmpeg-executor", "bad codec")];

        let outcome = PlanOutcome::from_event_result(&results, &plan, &file);

        match outcome {
            PlanOutcome::Failed(failed) => {
                assert_eq!(failed.plugin_name.as_deref(), Some("ffmpeg-executor"));
                assert_eq!(failed.error, "bad codec");
                assert_eq!(failed.path, file.path);
                assert_eq!(failed.phase_name, plan.phase_name);
            }
            PlanOutcome::Success { .. } => panic!("expected failure outcome"),
        }
    }

    #[test]
    fn unclaimed_result_reports_no_executor() {
        let (plan, file) = plan_and_file();
        let results = [EventResult::new("observer")];

        let outcome = PlanOutcome::from_event_result(&results, &plan, &file);

        match outcome {
            PlanOutcome::Failed(failed) => {
                assert_eq!(failed.plugin_name, None);
                assert_eq!(failed.error, "no executor available for plan");
            }
            PlanOutcome::Success { .. } => panic!("expected failure outcome"),
        }
    }
}
