use std::path::Path;
use std::time::Duration;

use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{ActionResult, OperationType, PlannedAction};
use voom_domain::utils::sanitize::validate_metadata_value;

/// Build and execute mkvpropedit commands for metadata operations.
///
/// All actions are batched into a single mkvpropedit invocation for efficiency.
/// Returns one `ActionResult` per input action.
pub fn execute_propedit_actions(
    path: &Path,
    actions: &[&PlannedAction],
) -> Result<Vec<ActionResult>> {
    if actions.is_empty() {
        return Ok(Vec::new());
    }

    let args = build_propedit_args(path, actions)?;

    tracing::info!(
        path = %path.display(),
        actions = actions.len(),
        "executing mkvpropedit"
    );
    tracing::debug!(args = ?args, "mkvpropedit arguments");

    let output = crate::run_with_timeout("mkvpropedit", &args, Duration::from_secs(300))?;

    if output.status.success() {
        Ok(actions
            .iter()
            .map(|a| ActionResult {
                operation: a.operation,
                success: true,
                description: a.description.clone(),
                error: None,
            })
            .collect())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        tracing::error!(
            path = %path.display(),
            stderr = %stderr,
            "mkvpropedit failed"
        );
        Err(VoomError::ToolExecution {
            tool: "mkvpropedit".into(),
            message: format!(
                "mkvpropedit exited with status {}: {}",
                output.status, stderr
            ),
        })
    }
}

/// Push `--edit track:N --set flag=value` args for a track flag operation.
fn push_track_flag(args: &mut Vec<String>, action: &PlannedAction, flag_value: &str) {
    if let Some(idx) = action.track_index {
        args.push("--edit".into());
        args.push(format!("track:{}", idx + 1));
        args.push("--set".into());
        args.push(flag_value.into());
    }
}

/// Build mkvpropedit arguments for a set of metadata actions.
///
/// Returns the full argument list (not including the binary name).
/// All actions are grouped into a single invocation using multiple `--edit` flags.
///
/// Track numbering: mkvpropedit uses 1-based track numbers. Our `PlannedAction.track_index`
/// is 0-based, so we add 1.
pub fn build_propedit_args(path: &Path, actions: &[&PlannedAction]) -> Result<Vec<String>> {
    let mut args = vec![path.to_string_lossy().into_owned()];

    for action in actions {
        match action.operation {
            OperationType::SetDefault => push_track_flag(&mut args, action, "flag-default=1"),
            OperationType::ClearDefault => push_track_flag(&mut args, action, "flag-default=0"),
            OperationType::SetForced => push_track_flag(&mut args, action, "flag-forced=1"),
            OperationType::ClearForced => push_track_flag(&mut args, action, "flag-forced=0"),
            OperationType::SetTitle => {
                if let Some(idx) = action.track_index {
                    let title = action.parameters["title"].as_str().unwrap_or("");
                    validate_metadata_value(title)?;
                    args.push("--edit".into());
                    args.push(format!("track:{}", idx + 1));
                    args.push("--set".into());
                    args.push(format!("name={}", title));
                }
            }
            OperationType::SetLanguage => {
                if let Some(idx) = action.track_index {
                    let language = action.parameters["language"].as_str().unwrap_or("und");
                    args.push("--edit".into());
                    args.push(format!("track:{}", idx + 1));
                    args.push("--set".into());
                    args.push(format!("language={}", language));
                }
            }
            OperationType::SetContainerTag => {
                let tag = action.parameters["tag"].as_str().unwrap_or("title");
                let value = action.parameters["value"].as_str().unwrap_or("");
                validate_metadata_value(tag)?;
                validate_metadata_value(value)?;
                args.push("--edit".into());
                args.push("info".into());
                args.push("--set".into());
                args.push(format!("{}={}", tag, value));
            }
            OperationType::ClearContainerTags => {
                if let Some(tags) = action.parameters["tags"].as_array() {
                    for tag_val in tags {
                        if let Some(tag) = tag_val.as_str() {
                            validate_metadata_value(tag)?;
                            args.push("--edit".into());
                            args.push("info".into());
                            args.push("--delete".into());
                            args.push(tag.to_string());
                        }
                    }
                }
            }
            OperationType::DeleteContainerTag => {
                let tag = action.parameters["tag"].as_str().unwrap_or("");
                validate_metadata_value(tag)?;
                args.push("--edit".into());
                args.push("info".into());
                args.push("--delete".into());
                args.push(tag.to_string());
            }
            _ => {
                // Non-propedit operations are ignored here
                tracing::warn!(
                    operation = ?action.operation,
                    "skipping non-propedit operation in propedit builder"
                );
            }
        }
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
    fn test_build_propedit_args_set_default() {
        let action = make_action(OperationType::SetDefault, Some(2), serde_json::json!({}));
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:3",
                "--set",
                "flag-default=1",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_clear_default() {
        let action = make_action(OperationType::ClearDefault, Some(0), serde_json::json!({}));
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:1",
                "--set",
                "flag-default=0",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_set_forced() {
        let action = make_action(OperationType::SetForced, Some(3), serde_json::json!({}));
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:4",
                "--set",
                "flag-forced=1",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_clear_forced() {
        let action = make_action(OperationType::ClearForced, Some(1), serde_json::json!({}));
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:2",
                "--set",
                "flag-forced=0",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_set_title() {
        let action = make_action(
            OperationType::SetTitle,
            Some(1),
            serde_json::json!({"title": "English Commentary"}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:2",
                "--set",
                "name=English Commentary",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_set_language() {
        let action = make_action(
            OperationType::SetLanguage,
            Some(2),
            serde_json::json!({"language": "jpn"}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:3",
                "--set",
                "language=jpn",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_set_container_tag() {
        let action = make_action(
            OperationType::SetContainerTag,
            None,
            serde_json::json!({"tag": "title", "value": "My Movie"}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "info",
                "--set",
                "title=My Movie",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_multiple() {
        let a1 = make_action(OperationType::SetDefault, Some(1), serde_json::json!({}));
        let a2 = make_action(OperationType::ClearDefault, Some(2), serde_json::json!({}));
        let a3 = make_action(
            OperationType::SetLanguage,
            Some(3),
            serde_json::json!({"language": "eng"}),
        );
        let actions: Vec<&PlannedAction> = vec![&a1, &a2, &a3];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "track:2",
                "--set",
                "flag-default=1",
                "--edit",
                "track:3",
                "--set",
                "flag-default=0",
                "--edit",
                "track:4",
                "--set",
                "language=eng",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_empty() {
        let actions: Vec<&PlannedAction> = vec![];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(args, vec!["/media/movie.mkv"]);
    }

    #[test]
    fn test_build_propedit_args_rejects_control_chars_in_title() {
        let action = make_action(
            OperationType::SetTitle,
            Some(0),
            serde_json::json!({"title": "Bad\x00Title"}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let result = build_propedit_args(Path::new("/media/movie.mkv"), &actions);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_propedit_args_rejects_control_chars_in_container_tag() {
        let action = make_action(
            OperationType::SetContainerTag,
            None,
            serde_json::json!({"tag": "title", "value": "Bad\x01Value"}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let result = build_propedit_args(Path::new("/media/movie.mkv"), &actions);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_propedit_args_clear_container_tags() {
        let action = make_action(
            OperationType::ClearContainerTags,
            None,
            serde_json::json!({"tags": ["title", "encoder"]}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec![
                "/media/movie.mkv",
                "--edit",
                "info",
                "--delete",
                "title",
                "--edit",
                "info",
                "--delete",
                "encoder",
            ]
        );
    }

    #[test]
    fn test_build_propedit_args_delete_container_tag() {
        let action = make_action(
            OperationType::DeleteContainerTag,
            None,
            serde_json::json!({"tag": "encoder"}),
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec!["/media/movie.mkv", "--edit", "info", "--delete", "encoder",]
        );
    }
}
