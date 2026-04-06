use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use scopeguard::ScopeGuard;
use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{
    ActionParams, ActionResult, ExecutionDetail, OperationType, PlannedAction,
};
use voom_domain::temp_file::temp_path_with_ext;
use voom_process::run_with_timeout;

/// Execute mkvmerge operations (remux, track removal, reorder).
///
/// These require creating a new file and replacing the original.
/// The process is:
/// 1. Build mkvmerge arguments
/// 2. Write to a temp file (`.tmp.mkv`) in the same directory
/// 3. On success, rename temp over the original (or to new extension if container changed)
/// 4. On failure, clean up the temp file
pub fn execute_merge_actions(path: &Path, actions: &[&PlannedAction]) -> Result<Vec<ActionResult>> {
    if actions.is_empty() {
        return Ok(Vec::new());
    }

    if !path.exists() {
        return Err(VoomError::ToolExecution {
            tool: "mkvmerge".into(),
            message: format!("file not found: {}", path.display()),
        });
    }

    let temp_path = temp_path_with_ext(path, "mkv");

    let args = build_merge_args(path, &temp_path, actions);

    // Guard ensures temp file is cleaned up even on panic
    let _guard = scopeguard::guard(temp_path.clone(), |p| {
        let _ = fs::remove_file(&p);
    });

    tracing::info!(
        input = %path.display(),
        output = %temp_path.display(),
        actions = actions.len(),
        "executing mkvmerge"
    );
    tracing::debug!(args = ?args, "mkvmerge arguments");

    let command_str = voom_process::shell_quote_args("mkvmerge", &args);
    let start = Instant::now();
    let output = run_with_timeout("mkvmerge", &args, Duration::from_secs(1800))?;
    let duration_ms = start.elapsed().as_millis() as u64;

    // mkvmerge returns 0 for success, 1 for warnings (still successful), 2 for errors
    if output.status.code().unwrap_or(2) <= 1 {
        // Determine the final destination path.
        // If there's a ConvertContainer action, the output stays as .mkv (already the temp name).
        // Otherwise, replace the original file.
        let final_path = determine_final_path(path, actions);

        // Replace original with temp file
        fs::rename(&temp_path, &final_path).map_err(|e| VoomError::ToolExecution {
            tool: "mkvmerge".into(),
            message: format!(
                "failed to rename {} to {}: {}",
                temp_path.display(),
                final_path.display(),
                e
            ),
        })?;

        // Defuse the guard — temp file was successfully renamed
        ScopeGuard::into_inner(_guard);

        // If we converted container and the original had a different extension, remove it
        if final_path != path && path.exists() {
            let _ = fs::remove_file(path);
        }

        let detail = ExecutionDetail {
            command: command_str,
            exit_code: output.status.code(),
            stderr_tail: String::new(),
            duration_ms,
        };
        Ok(actions
            .iter()
            .map(|a| {
                ActionResult::success(a.operation, a.description.clone())
                    .with_execution_detail(detail.clone())
            })
            .collect())
    } else {
        // Guard will clean up temp file when it drops

        let tail = voom_process::stderr_tail(&output.stderr, 20);
        tracing::error!(
            input = %path.display(),
            stderr = %tail,
            "mkvmerge failed"
        );
        Err(VoomError::ToolExecution {
            tool: "mkvmerge".into(),
            message: format!(
                "mkvmerge exited with status {}:\n{}\ncmd: {}",
                output.status, tail, command_str
            ),
        })
    }
}

/// Determine the final output path after merge operations.
///
/// If there is a `ConvertContainer` action, the file gets an `.mkv` extension.
/// Otherwise, the output replaces the original file.
fn determine_final_path(original: &Path, actions: &[&PlannedAction]) -> std::path::PathBuf {
    for action in actions {
        if action.operation == OperationType::ConvertContainer {
            // Change the extension to mkv
            let mut final_path = original.to_path_buf();
            final_path.set_extension("mkv");
            return final_path;
        }
    }
    // No container conversion — replace in place
    original.to_path_buf()
}

/// Build mkvmerge arguments for structural operations.
///
/// Returns the full argument list (not including the binary name).
pub fn build_merge_args(
    input_path: &Path,
    output_path: &Path,
    actions: &[&PlannedAction],
) -> Vec<String> {
    let mut args = vec!["-o".into(), output_path.to_string_lossy().into_owned()];

    // Group track removals by type in a single pass using the "track_type" parameter.
    // mkvmerge supports negation with `!` prefix:
    // `--video-tracks !3` means "all video tracks except TID 3"
    let mut video_removes: Vec<u32> = Vec::new();
    let mut audio_removes: Vec<u32> = Vec::new();
    let mut subtitle_removes: Vec<u32> = Vec::new();

    for action in actions.iter() {
        if action.operation == OperationType::RemoveTrack {
            if let Some(idx) = action.track_index {
                let track_type = match &action.parameters {
                    ActionParams::RemoveTrack { track_type, .. } => *track_type,
                    _ => continue,
                };
                match track_type.track_category() {
                    "video" => video_removes.push(idx),
                    "audio" => audio_removes.push(idx),
                    "subtitle" => subtitle_removes.push(idx),
                    _ => {
                        // Attachment or unknown — add to all removal lists and let mkvmerge ignore non-matching
                        video_removes.push(idx);
                        audio_removes.push(idx);
                        subtitle_removes.push(idx);
                    }
                }
            }
        }
    }

    if !video_removes.is_empty() {
        let ids: Vec<String> = video_removes.iter().map(|i| i.to_string()).collect();
        args.push("--video-tracks".into());
        args.push(format!("!{}", ids.join(",")));
    }

    if !audio_removes.is_empty() {
        let ids: Vec<String> = audio_removes.iter().map(|i| i.to_string()).collect();
        args.push("--audio-tracks".into());
        args.push(format!("!{}", ids.join(",")));
    }

    if !subtitle_removes.is_empty() {
        let ids: Vec<String> = subtitle_removes.iter().map(|i| i.to_string()).collect();
        args.push("--subtitle-tracks".into());
        args.push(format!("!{}", ids.join(",")));
    }

    // Handle track reordering
    for action in actions.iter() {
        if action.operation == OperationType::ReorderTracks {
            if let ActionParams::ReorderTracks { order } = &action.parameters {
                let track_order: Vec<String> = order
                    .iter()
                    .filter_map(|v| v.parse::<u64>().ok())
                    .map(|idx| format!("0:{idx}"))
                    .collect();
                if !track_order.is_empty() {
                    args.push("--track-order".into());
                    args.push(track_order.join(","));
                }
            }
        }
    }

    // Add the input file
    args.push(input_path.to_string_lossy().into_owned());

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::test_helpers::make_action;
    use voom_domain::media::{Container, TrackType};

    #[test]
    fn test_build_merge_args_remove_track() {
        let action = make_action(
            OperationType::RemoveTrack,
            Some(3),
            ActionParams::RemoveTrack {
                reason: "test".into(),
                track_type: TrackType::SubtitleMain,
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_merge_args(
            Path::new("/media/movie.mkv"),
            Path::new("/media/movie.tmp.mkv"),
            &actions,
        );
        assert_eq!(
            args,
            vec![
                "-o",
                "/media/movie.tmp.mkv",
                "--subtitle-tracks",
                "!3",
                "/media/movie.mkv",
            ]
        );
    }

    #[test]
    fn test_build_merge_args_reorder() {
        let action = make_action(
            OperationType::ReorderTracks,
            None,
            ActionParams::ReorderTracks {
                order: vec!["0".into(), "2".into(), "1".into(), "3".into()],
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_merge_args(
            Path::new("/media/movie.mkv"),
            Path::new("/media/movie.tmp.mkv"),
            &actions,
        );
        assert_eq!(
            args,
            vec![
                "-o",
                "/media/movie.tmp.mkv",
                "--track-order",
                "0:0,0:2,0:1,0:3",
                "/media/movie.mkv",
            ]
        );
    }

    #[test]
    fn test_build_merge_args_convert_container() {
        let action = make_action(
            OperationType::ConvertContainer,
            None,
            ActionParams::Container {
                container: Container::Mkv,
            },
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let args = build_merge_args(
            Path::new("/media/movie.mp4"),
            Path::new("/media/movie.tmp.mkv"),
            &actions,
        );
        // ConvertContainer is just a remux — no special flags needed beyond -o
        assert_eq!(
            args,
            vec!["-o", "/media/movie.tmp.mkv", "/media/movie.mp4",]
        );
    }

    #[test]
    fn test_build_merge_args_multiple_removes() {
        let a1 = make_action(
            OperationType::RemoveTrack,
            Some(2),
            ActionParams::RemoveTrack {
                reason: "test".into(),
                track_type: TrackType::AudioMain,
            },
        );
        let a2 = make_action(
            OperationType::RemoveTrack,
            Some(4),
            ActionParams::RemoveTrack {
                reason: "test".into(),
                track_type: TrackType::SubtitleMain,
            },
        );
        let actions: Vec<&PlannedAction> = vec![&a1, &a2];
        let args = build_merge_args(
            Path::new("/media/movie.mkv"),
            Path::new("/media/movie.tmp.mkv"),
            &actions,
        );
        assert_eq!(
            args,
            vec![
                "-o",
                "/media/movie.tmp.mkv",
                "--audio-tracks",
                "!2",
                "--subtitle-tracks",
                "!4",
                "/media/movie.mkv",
            ]
        );
    }

    #[test]
    fn test_build_merge_args_combined() {
        let a1 = make_action(
            OperationType::RemoveTrack,
            Some(3),
            ActionParams::RemoveTrack {
                reason: "test".into(),
                track_type: TrackType::AudioMain,
            },
        );
        let a2 = make_action(
            OperationType::ReorderTracks,
            None,
            ActionParams::ReorderTracks {
                order: vec!["0".into(), "1".into(), "2".into(), "4".into()],
            },
        );
        let actions: Vec<&PlannedAction> = vec![&a1, &a2];
        let args = build_merge_args(
            Path::new("/media/movie.mkv"),
            Path::new("/media/movie.tmp.mkv"),
            &actions,
        );
        assert_eq!(
            args,
            vec![
                "-o",
                "/media/movie.tmp.mkv",
                "--audio-tracks",
                "!3",
                "--track-order",
                "0:0,0:1,0:2,0:4",
                "/media/movie.mkv",
            ]
        );
    }
}
