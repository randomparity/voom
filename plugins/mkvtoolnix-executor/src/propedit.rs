use std::path::Path;
use std::time::Duration;

use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{ActionParams, ActionResult, OperationType, PlannedAction};
use voom_domain::utils::sanitize::validate_metadata_value;
use voom_process::run_with_timeout;

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

    let output = run_with_timeout("mkvpropedit", &args, Duration::from_secs(300))?;

    if output.status.success() {
        Ok(actions
            .iter()
            .map(|a| ActionResult::success(a.operation, a.description.clone()))
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

/// Properties that live in the Matroska Segment/Info element and can be
/// edited via `mkvpropedit --edit info --set/--delete`.
/// Other metadata (ENCODER, etc.) lives in the Tags element and requires
/// `--tags all:` to clear.
fn is_segment_info_property(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "title"
            | "date"
            | "segment-uid"
            | "prev-uid"
            | "next-uid"
            | "segment-filename"
            | "prev-filename"
            | "next-filename"
            | "muxing-application"
            | "writing-application"
    )
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
                    let title = match &action.parameters {
                        ActionParams::Title { title } => title.as_str(),
                        _ => "",
                    };
                    validate_metadata_value(title)?;
                    args.push("--edit".into());
                    args.push(format!("track:{}", idx + 1));
                    args.push("--set".into());
                    args.push(format!("name={title}"));
                }
            }
            OperationType::SetLanguage => {
                if let Some(idx) = action.track_index {
                    let language = match &action.parameters {
                        ActionParams::Language { language } => language.as_str(),
                        _ => "und",
                    };
                    validate_metadata_value(language)?;
                    args.push("--edit".into());
                    args.push(format!("track:{}", idx + 1));
                    args.push("--set".into());
                    args.push(format!("language={language}"));
                }
            }
            OperationType::SetContainerTag => {
                let (tag, value) = match &action.parameters {
                    ActionParams::SetTag { tag, value } => (tag.as_str(), value.as_str()),
                    _ => ("title", ""),
                };
                if is_segment_info_property(tag) {
                    validate_metadata_value(tag)?;
                    validate_metadata_value(value)?;
                    args.push("--edit".into());
                    args.push("info".into());
                    args.push("--set".into());
                    args.push(format!("{tag}={value}"));
                } else {
                    tracing::debug!(
                        tag = tag,
                        "skipping set_tag in propedit: not a segment info property"
                    );
                }
            }
            OperationType::ClearContainerTags => {
                // MKV stores metadata in two places: segment info properties
                // (title, date, etc.) and Tags elements (ENCODER, etc.).
                // `--tags all:` clears all Tags elements (empty filename = remove).
                args.push("--tags".into());
                args.push("all:".into());
            }
            OperationType::DeleteContainerTag => {
                // MKV Tags element entries (ENCODER, etc.) cannot be individually
                // deleted via mkvpropedit CLI — only `--tags all:` can clear them.
                // Segment info properties (title) can be deleted with --edit info.
                // We handle the known segment info case; for Tags entries,
                // ClearContainerTags (--tags all:) is the correct approach.
                let tag = match &action.parameters {
                    ActionParams::DeleteTag { tag } => tag.as_str(),
                    _ => "",
                };
                if is_segment_info_property(tag) {
                    validate_metadata_value(tag)?;
                    args.push("--edit".into());
                    args.push("info".into());
                    args.push("--delete".into());
                    args.push(tag.to_string());
                } else {
                    tracing::debug!(
                        tag = tag,
                        "skipping delete_tag in propedit: not a segment info property, use clear_tags instead"
                    );
                }
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

    use crate::test_helpers::make_action;

    #[test]
    fn test_build_propedit_args_set_default() {
        let action = make_action(OperationType::SetDefault, Some(2), ActionParams::Empty);
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
        let action = make_action(OperationType::ClearDefault, Some(0), ActionParams::Empty);
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
        let action = make_action(OperationType::SetForced, Some(3), ActionParams::Empty);
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
        let action = make_action(OperationType::ClearForced, Some(1), ActionParams::Empty);
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
            ActionParams::Title {
                title: "English Commentary".into(),
            },
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
            ActionParams::Language {
                language: "jpn".into(),
            },
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
            ActionParams::SetTag {
                tag: "title".into(),
                value: "My Movie".into(),
            },
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
        let a1 = make_action(OperationType::SetDefault, Some(1), ActionParams::Empty);
        let a2 = make_action(OperationType::ClearDefault, Some(2), ActionParams::Empty);
        let a3 = make_action(
            OperationType::SetLanguage,
            Some(3),
            ActionParams::Language {
                language: "eng".into(),
            },
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
            ActionParams::Title {
                title: "Bad\x00Title".into(),
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let result = build_propedit_args(Path::new("/media/movie.mkv"), &actions);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_propedit_args_rejects_control_chars_in_language() {
        let action = make_action(
            OperationType::SetLanguage,
            Some(0),
            ActionParams::Language {
                language: "en\x00g".into(),
            },
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
            ActionParams::SetTag {
                tag: "title".into(),
                value: "Bad\x01Value".into(),
            },
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
            ActionParams::ClearTags {
                tags: vec!["title".into(), "ENCODER".into()],
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        // Uses --tags all: to clear all Matroska Tags elements
        assert_eq!(args, vec!["/media/movie.mkv", "--tags", "all:",]);
    }

    #[test]
    fn test_build_propedit_args_delete_container_tag_segment_info() {
        // "title" is a segment info property — emits --edit info --delete
        let action = make_action(
            OperationType::DeleteContainerTag,
            None,
            ActionParams::DeleteTag {
                tag: "title".into(),
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        assert_eq!(
            args,
            vec!["/media/movie.mkv", "--edit", "info", "--delete", "title",]
        );
    }

    #[test]
    fn test_build_propedit_args_delete_container_tag_non_segment_info() {
        // "ENCODER" is a Tags element entry — skipped in propedit (needs --tags all:)
        let action = make_action(
            OperationType::DeleteContainerTag,
            None,
            ActionParams::DeleteTag {
                tag: "ENCODER".into(),
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_propedit_args(Path::new("/media/movie.mkv"), &actions).unwrap();
        // Only the file path, no delete args since ENCODER isn't a segment info property
        assert_eq!(args, vec!["/media/movie.mkv"]);
    }
}
