//! Plan execution outcome reconstruction.
//!
//! After dispatching a `PlanCreated` event, the caller receives a
//! `Vec<EventResult>` from the kernel. The decision of whether the plan
//! succeeded, failed, or went unclaimed requires scanning the results for
//! several fields (`claimed`, `execution_error`, `execution_detail`,
//! `plugin_name`). This module concentrates that scan into a single
//! constructor so the caller does not perform three separate passes.

use voom_domain::events::{EventResult, PlanFailedEvent};

/// Result of executing a single plan via the event bus.
pub(super) enum PlanOutcome {
    /// An executor claimed and completed the plan.
    Success { executor: String },
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
        plan: &voom_domain::plan::Plan,
        file: &voom_domain::media::MediaFile,
    ) -> Self {
        let mut claimed_name: Option<&str> = None;
        let mut exec_error: Option<String> = None;
        let mut exec_detail: Option<voom_domain::plan::ExecutionDetail> = None;

        for r in results {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use voom_domain::events::EventResult;
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;

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
            PlanOutcome::Success { executor } => assert_eq!(executor, "ffmpeg-executor"),
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
