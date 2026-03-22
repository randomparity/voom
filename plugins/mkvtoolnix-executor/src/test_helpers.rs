//! Shared test helpers for mkvtoolnix-executor tests.

use voom_domain::plan::{ActionParams, OperationType, PlannedAction};

pub fn make_action(
    op: OperationType,
    track_index: Option<u32>,
    params: ActionParams,
) -> PlannedAction {
    match track_index {
        Some(idx) => PlannedAction::track_op(op, idx, params, format!("{:?} action", op)),
        None => PlannedAction::file_op(op, params, format!("{:?} action", op)),
    }
}
