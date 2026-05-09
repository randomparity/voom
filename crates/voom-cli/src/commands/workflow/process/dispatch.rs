//! Plan lifecycle dispatch helpers.
//!
//! # Plan lifecycle contract
//!
//! The event bus is the sole channel for telling other plugins (backup-manager,
//! executors, sqlite-store) that a plan is in motion. The lifecycle events
//! MUST be dispatched in the order below so that downstream plugins see a
//! consistent view of each plan:
//!
//! 1. `PlanExecuting` — dispatched first so backup-manager can back up the
//!    file BEFORE any executor modifies it.
//! 2. `PlanCreated` — lets executor plugins claim and run the plan. Returns
//!    an `EventResult` vec that encodes the outcome (see
//!    [`super::plan_outcome::PlanOutcome::from_event_result`]).
//! 3. One of:
//!    - `PlanCompleted` (on success)
//!    - `PlanSkipped` (skip reason set)
//!    - `PlanFailed` (executor error, safeguard abort, or no executor
//!      available — caller decides when and why).
//!
//! The pre-execution safeguard (`check_disk_space`) intentionally dispatches
//! only `PlanFailed` without a preceding `PlanCreated` so executors do not
//! run. Post-execution safeguards (`check_size_increase`,
//! `check_duration_shrink`) dispatch only `PlanFailed` because
//! `PlanCreated` was already emitted by [`PlanDispatcher::begin`].
//! Skipped plans are persisted directly and then dispatched as `PlanSkipped`;
//! they do not dispatch `PlanCreated` because there is no executor work to
//! claim.
//!
//! Every dispatch goes through this module so that the `log_plugin_errors`
//! pairing is handled in one place.

use voom_domain::events::{
    plan_begin_events_for_path, Event, EventResult, PlanCompletedEvent, PlanFailedEvent,
    PlanSkippedEvent,
};
use voom_domain::storage::PlanStorage;
use voom_kernel::Kernel;

/// Log any `plugin.error` events produced during event dispatch so
/// CLI users see plugin failures rather than silent swallowing.
pub(super) fn log_plugin_errors(results: &[EventResult]) {
    for result in results {
        for produced in &result.produced_events {
            if let Event::PluginError(err) = produced {
                tracing::warn!(
                    plugin = %err.plugin_name,
                    event = %err.event_type,
                    error = %err.error,
                    "plugin error during dispatch"
                );
            }
        }
    }
}

/// Dispatch an event through the kernel and log any plugin errors from the
/// returned `EventResult`s. Returns the results so callers that need to
/// inspect them (e.g. `PlanCreated` for outcome reconstruction) can.
pub(super) fn dispatch_and_log(kernel: &Kernel, event: Event) -> Vec<EventResult> {
    let results = kernel.dispatch(event);
    log_plugin_errors(&results);
    results
}

/// Persist a plan row without dispatching executor-facing lifecycle events.
pub(super) fn persist_plan(
    store: &dyn PlanStorage,
    plan: &voom_domain::plan::Plan,
) -> voom_domain::Result<()> {
    store.save_plan(plan)?;
    Ok(())
}

/// Typed helpers for the plan lifecycle. Each method dispatches exactly one
/// event, always pairs with `log_plugin_errors`, and documents the role of
/// that event in the contract described at the module level.
pub(super) struct PlanDispatcher<'a> {
    kernel: &'a Kernel,
}

impl<'a> PlanDispatcher<'a> {
    pub(super) fn new(kernel: &'a Kernel) -> Self {
        Self { kernel }
    }

    /// Step 1 + 2: dispatch `PlanExecuting` then `PlanCreated` and return the
    /// `PlanCreated` event-bus results so the caller can reconstruct the
    /// outcome via `PlanOutcome::from_event_result`.
    ///
    /// Dispatching `PlanExecuting` first ensures backup-manager creates a
    /// backup before any executor mutates the file.
    pub(super) fn begin(
        &self,
        plan: &voom_domain::plan::Plan,
        file: &voom_domain::media::MediaFile,
    ) -> Vec<EventResult> {
        let mut plan_created_results = Vec::new();
        for event in plan_begin_events_for_path(file.path.clone(), plan.clone()) {
            let results = dispatch_and_log(self.kernel, event.clone());
            if matches!(event, Event::PlanCreated(_)) {
                plan_created_results = results;
            }
        }
        plan_created_results
    }

    /// Dispatch `PlanCompleted` after a successful execution.
    pub(super) fn completed(
        &self,
        plan: &voom_domain::plan::Plan,
        file: &voom_domain::media::MediaFile,
        keep_backups: bool,
    ) {
        dispatch_and_log(
            self.kernel,
            Event::PlanCompleted(PlanCompletedEvent::new(
                plan.id,
                file.path.clone(),
                plan.phase_name.clone(),
                plan.actions.len(),
                keep_backups,
            )),
        );
    }

    /// Dispatch `PlanSkipped` for a plan the policy chose not to execute.
    pub(super) fn skipped(
        &self,
        plan: &voom_domain::plan::Plan,
        file: &voom_domain::media::MediaFile,
        reason: &str,
    ) {
        dispatch_and_log(
            self.kernel,
            Event::PlanSkipped(PlanSkippedEvent::new(
                plan.id,
                file.path.clone(),
                plan.phase_name.clone(),
                reason.to_string(),
            )),
        );
    }

    /// Dispatch `PlanFailed` for an executor error or a safeguard abort.
    pub(super) fn failed(&self, event: PlanFailedEvent) {
        dispatch_and_log(self.kernel, Event::PlanFailed(event));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult};
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;

    use super::*;

    #[derive(Default)]
    struct RecordingPlugin {
        events: Mutex<Vec<&'static str>>,
    }

    impl RecordingPlugin {
        fn event_names(&self) -> Vec<&'static str> {
            self.events.lock().unwrap().clone()
        }
    }

    impl voom_kernel::Plugin for RecordingPlugin {
        fn name(&self) -> &'static str {
            "recording-plugin"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn capabilities(&self) -> &[Capability] {
            &[]
        }

        fn handles(&self, event_type: &str) -> bool {
            matches!(
                event_type,
                Event::PLAN_EXECUTING
                    | Event::PLAN_CREATED
                    | Event::PLAN_COMPLETED
                    | Event::PLAN_SKIPPED
                    | Event::PLAN_FAILED
            )
        }

        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            let event_name = match event {
                Event::PlanExecuting(_) => Event::PLAN_EXECUTING,
                Event::PlanCreated(_) => Event::PLAN_CREATED,
                Event::PlanCompleted(_) => Event::PLAN_COMPLETED,
                Event::PlanSkipped(_) => Event::PLAN_SKIPPED,
                Event::PlanFailed(_) => Event::PLAN_FAILED,
                _ => return Ok(None),
            };
            self.events.lock().unwrap().push(event_name);
            Ok(Some(EventResult::plan_succeeded(self.name(), None)))
        }
    }

    fn sample_file() -> MediaFile {
        MediaFile::new(PathBuf::from("/tmp/test.mkv"))
    }

    fn sample_plan() -> Plan {
        Plan::new(sample_file(), "test-policy", "normalize")
    }

    fn kernel_with_recorder() -> (Kernel, Arc<RecordingPlugin>) {
        let mut kernel = Kernel::new();
        let recorder = Arc::new(RecordingPlugin::default());
        kernel.register_plugin(recorder.clone(), 50).unwrap();
        (kernel, recorder)
    }

    #[test]
    fn begin_dispatches_executing_before_created_only() {
        let (kernel, recorder) = kernel_with_recorder();
        let plan = sample_plan();
        let file = sample_file();

        let results = PlanDispatcher::new(&kernel).begin(&plan, &file);

        assert_eq!(
            recorder.event_names(),
            vec![Event::PLAN_EXECUTING, Event::PLAN_CREATED]
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn completed_skipped_and_failed_dispatch_single_events() {
        let (kernel, recorder) = kernel_with_recorder();
        let plan = sample_plan();
        let file = sample_file();
        let dispatcher = PlanDispatcher::new(&kernel);

        dispatcher.completed(&plan, &file, false);
        dispatcher.skipped(&plan, &file, "policy skip");
        dispatcher.failed(PlanFailedEvent::new(
            plan.id,
            file.path,
            plan.phase_name,
            "executor failed",
        ));

        assert_eq!(
            recorder.event_names(),
            vec![
                Event::PLAN_COMPLETED,
                Event::PLAN_SKIPPED,
                Event::PLAN_FAILED
            ]
        );
    }

    #[test]
    fn persist_plan_does_not_dispatch_created() {
        let (kernel, recorder) = kernel_with_recorder();
        let store = voom_domain::test_support::InMemoryStore::new();
        let plan = sample_plan();

        persist_plan(&store, &plan).unwrap();
        PlanDispatcher::new(&kernel).skipped(&plan, &sample_file(), "policy skip");

        assert_eq!(recorder.event_names(), vec![Event::PLAN_SKIPPED]);
    }
}
