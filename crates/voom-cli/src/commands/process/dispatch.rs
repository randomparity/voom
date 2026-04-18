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
//!
//! Every dispatch goes through this module so that the `log_plugin_errors`
//! pairing is handled in one place.

use voom_domain::events::{
    Event, EventResult, PlanCompletedEvent, PlanCreatedEvent, PlanExecutingEvent, PlanFailedEvent,
    PlanSkippedEvent,
};
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
        dispatch_and_log(
            self.kernel,
            Event::PlanExecuting(PlanExecutingEvent::new(
                plan.id,
                file.path.clone(),
                plan.phase_name.clone(),
                plan.actions.len(),
            )),
        );
        dispatch_and_log(
            self.kernel,
            Event::PlanCreated(PlanCreatedEvent::new(plan.clone())),
        )
    }

    /// Dispatch `PlanCreated` only — used for skipped plans so sqlite-store
    /// can persist the row before we immediately follow with `PlanSkipped`.
    pub(super) fn created(&self, plan: &voom_domain::plan::Plan) {
        dispatch_and_log(
            self.kernel,
            Event::PlanCreated(PlanCreatedEvent::new(plan.clone())),
        );
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
