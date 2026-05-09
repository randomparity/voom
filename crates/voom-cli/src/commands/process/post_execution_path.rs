use voom_domain::plan::OperationType;

/// Determine the file path after plan execution.
///
/// If a `ConvertContainer` action changed the container, the file extension
/// will have changed on disk (e.g. `.mp4` -> `.mkv`). Derive the new path
/// from the plan actions; fall back to the original path if unchanged.
pub(super) fn resolve_post_execution_path(
    file: &voom_domain::media::MediaFile,
    plans: &[voom_domain::plan::Plan],
) -> std::path::PathBuf {
    if let Some(container) = find_last_container_action(plans) {
        let new_path = file.path.with_extension(container.as_str());
        if new_path.exists() {
            return new_path;
        }
    }
    file.path.clone()
}

fn find_last_container_action(
    plans: &[voom_domain::plan::Plan],
) -> Option<voom_domain::media::Container> {
    for plan in plans.iter().rev() {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }
        for action in &plan.actions {
            if action.operation == OperationType::ConvertContainer {
                if let voom_domain::plan::ActionParams::Container { container } = &action.parameters
                {
                    return Some(*container);
                }
            }
        }
    }
    None
}
