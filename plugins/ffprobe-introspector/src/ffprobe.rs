use std::path::Path;
use std::process::Command;

use voom_domain::errors::{Result, VoomError};

/// Run ffprobe on a file and return the JSON output.
pub fn run_ffprobe(ffprobe_path: &str, file_path: &Path) -> Result<serde_json::Value> {
    let output = Command::new(ffprobe_path)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_entries",
            "stream_side_data",
        ])
        .arg(file_path)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                VoomError::ToolNotFound {
                    tool: ffprobe_path.to_string(),
                }
            } else {
                VoomError::ToolExecution {
                    tool: "ffprobe".into(),
                    message: e.to_string(),
                }
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!(
                "ffprobe exited with status {}: {}",
                output.status,
                stderr.trim()
            ),
        });
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!("failed to parse ffprobe JSON output: {e}"),
        })?;

    Ok(json)
}

/// Check if ffprobe is available and return its version.
pub fn detect_ffprobe(ffprobe_path: &str) -> Result<String> {
    let output = Command::new(ffprobe_path)
        .args(["-version"])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                VoomError::ToolNotFound {
                    tool: ffprobe_path.to_string(),
                }
            } else {
                VoomError::ToolExecution {
                    tool: "ffprobe".into(),
                    message: e.to_string(),
                }
            }
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line is typically: "ffprobe version N.N.N ..."
    let version = stdout
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("ffprobe version "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("unknown")
        .to_string();

    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_ffprobe_not_found() {
        let result = detect_ffprobe("/nonexistent/ffprobe");
        assert!(result.is_err());
        match result.unwrap_err() {
            VoomError::ToolNotFound { tool } => {
                assert_eq!(tool, "/nonexistent/ffprobe");
            }
            other => panic!("expected ToolNotFound, got: {other}"),
        }
    }

    #[test]
    fn test_run_ffprobe_not_found() {
        let result = run_ffprobe("/nonexistent/ffprobe", Path::new("/dummy.mkv"));
        assert!(result.is_err());
        match result.unwrap_err() {
            VoomError::ToolNotFound { tool } => {
                assert_eq!(tool, "/nonexistent/ffprobe");
            }
            other => panic!("expected ToolNotFound, got: {other}"),
        }
    }
}
