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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use voom_domain::media::{Container, MediaFile};
    use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};

    use super::*;

    fn conversion_plan(file: &MediaFile, container: Container) -> Plan {
        Plan::new(file.clone(), "policy", "container").with_action(PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container { container },
            "convert container",
        ))
    }

    #[test]
    fn resolve_post_execution_path_uses_existing_converted_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original_path = dir.path().join("movie.mp4");
        let converted_path = dir.path().join("movie.mkv");
        std::fs::write(&original_path, b"original").expect("write original");
        std::fs::write(&converted_path, b"converted").expect("write converted");
        let file = MediaFile::new(original_path);
        let plan = conversion_plan(&file, Container::Mkv);

        let resolved = resolve_post_execution_path(&file, &[plan]);

        assert_eq!(resolved, converted_path);
    }

    #[test]
    fn resolve_post_execution_path_falls_back_when_target_missing() {
        let file = MediaFile::new(PathBuf::from("/movies/movie.mp4"));
        let plan = conversion_plan(&file, Container::Mkv);

        let resolved = resolve_post_execution_path(&file, &[plan]);

        assert_eq!(resolved, file.path);
    }

    #[test]
    fn resolve_post_execution_path_ignores_skipped_conversion_plan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original_path = dir.path().join("movie.mp4");
        let converted_path = dir.path().join("movie.mkv");
        std::fs::write(&converted_path, b"converted").expect("write converted");
        let file = MediaFile::new(original_path);
        let plan = conversion_plan(&file, Container::Mkv).with_skip_reason("not needed");

        let resolved = resolve_post_execution_path(&file, &[plan]);

        assert_eq!(resolved, file.path);
    }
}
