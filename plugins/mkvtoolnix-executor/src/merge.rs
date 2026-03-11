use std::fs;
use std::path::Path;
use std::process::Command;

use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{ActionResult, OperationType, PlannedAction};

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

    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let temp_path = parent.join(format!("{}.tmp.mkv", stem));

    let args = build_merge_args(path, &temp_path, actions);

    tracing::info!(
        input = %path.display(),
        output = %temp_path.display(),
        actions = actions.len(),
        "executing mkvmerge"
    );
    tracing::debug!(args = ?args, "mkvmerge arguments");

    let output =
        Command::new("mkvmerge")
            .args(&args)
            .output()
            .map_err(|e| VoomError::ToolExecution {
                tool: "mkvmerge".into(),
                message: format!("failed to spawn mkvmerge: {e}"),
            })?;

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

        // If we converted container and the original had a different extension, remove it
        if final_path != path && path.exists() {
            let _ = fs::remove_file(path);
        }

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
        // Clean up temp file on failure
        let _ = fs::remove_file(&temp_path);

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        tracing::error!(
            input = %path.display(),
            stderr = %stderr,
            "mkvmerge failed"
        );
        Err(VoomError::ToolExecution {
            tool: "mkvmerge".into(),
            message: format!("mkvmerge exited with status {}: {}", output.status, stderr),
        })
    }
}

/// Determine the final output path after merge operations.
///
/// If there is a ConvertContainer action, the file gets an `.mkv` extension.
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

    // Collect track removal indices
    let remove_indices: Vec<u32> = actions
        .iter()
        .filter(|a| a.operation == OperationType::RemoveTrack)
        .filter_map(|a| a.track_index)
        .collect();

    // Build track removal flags if needed.
    // mkvmerge's track selection flags accept comma-separated track IDs to INCLUDE.
    // We use negation: `--video-tracks !id1,id2` is NOT supported.
    // Instead, we use `--track-order` combined with explicit track type selection.
    //
    // The simplest reliable approach: use `-d TID` (video), `-a TID` (audio), `-s TID` (subtitle)
    // flags which accept track IDs to include. But we need to know track types.
    //
    // Since PlannedAction includes parameters with track type info, we classify removals.
    // For the general case, we group removed tracks by type from their parameters.
    if !remove_indices.is_empty() {
        // Group removals by track type using the "track_type" parameter
        let mut video_removes: Vec<u32> = Vec::new();
        let mut audio_removes: Vec<u32> = Vec::new();
        let mut subtitle_removes: Vec<u32> = Vec::new();

        for action in actions.iter() {
            if action.operation == OperationType::RemoveTrack {
                if let Some(idx) = action.track_index {
                    let track_type = action.parameters["track_type"]
                        .as_str()
                        .unwrap_or("unknown");
                    match track_type {
                        t if t.starts_with("video") || t == "Video" => video_removes.push(idx),
                        t if t.starts_with("audio") || t.starts_with("Audio") => {
                            audio_removes.push(idx)
                        }
                        t if t.starts_with("subtitle") || t.starts_with("Subtitle") => {
                            subtitle_removes.push(idx)
                        }
                        _ => {
                            // If track type is unknown, use a general approach:
                            // add to all removal lists and let mkvmerge ignore non-matching
                            video_removes.push(idx);
                            audio_removes.push(idx);
                            subtitle_removes.push(idx);
                        }
                    }
                }
            }
        }

        // Build exclusion args using mkvmerge's `--no-*` or specific track lists.
        // For each type with removals, we output the track IDs NOT to include.
        // mkvmerge uses `--video-tracks TID1,TID2` to include only those tracks.
        // But since we only know which to remove, we specify the "inverted" list
        // by telling mkvmerge to skip specific TIDs.
        //
        // Actually, mkvmerge DOES support negation with `!` prefix:
        // `--video-tracks !3` means "all video tracks except TID 3"
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
    }

    // Handle track reordering
    for action in actions.iter() {
        if action.operation == OperationType::ReorderTracks {
            if let Some(order) = action.parameters["order"].as_array() {
                let track_order: Vec<String> = order
                    .iter()
                    .filter_map(|v| v.as_u64())
                    .map(|idx| format!("0:{}", idx))
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
    fn test_build_merge_args_remove_track() {
        let action = make_action(
            OperationType::RemoveTrack,
            Some(3),
            serde_json::json!({"track_type": "subtitle_main"}),
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
            serde_json::json!({"order": [0, 2, 1, 3]}),
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
            serde_json::json!({"target": "mkv"}),
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
            serde_json::json!({"track_type": "audio_commentary"}),
        );
        let a2 = make_action(
            OperationType::RemoveTrack,
            Some(4),
            serde_json::json!({"track_type": "subtitle_commentary"}),
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
            serde_json::json!({"track_type": "audio_commentary"}),
        );
        let a2 = make_action(
            OperationType::ReorderTracks,
            None,
            serde_json::json!({"order": [0, 1, 2, 4]}),
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
