//! Shared test helpers for mkvtoolnix-executor tests.

use voom_domain::plan::{ActionParams, OperationType, PlannedAction};

pub fn make_action(
    op: OperationType,
    track_index: Option<u32>,
    params: ActionParams,
) -> PlannedAction {
    PlannedAction {
        operation: op,
        track_index,
        parameters: params,
        description: format!("{:?} action", op),
    }
}
