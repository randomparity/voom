use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use voom_domain::errors::{Result, VoomError};
use wait_timeout::ChildExt;

/// Run ffprobe on a file and return the JSON output.
///
/// Uses a timeout to prevent ffprobe from hanging indefinitely on
/// corrupted or problematic files.
pub fn run_ffprobe(
    ffprobe_path: &str,
    file_path: &Path,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let mut child = Command::new(ffprobe_path)
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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

    // Drain stdout and stderr in background threads to prevent pipe buffer
    // deadlock. Without this, ffprobe blocks writing to a full pipe while
    // we block waiting for it to exit.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let stdout_handle = std::thread::spawn(move || std::io::read_to_string(stdout));
    let stderr_handle = std::thread::spawn(move || std::io::read_to_string(stderr));

    let timed_out = match child.wait_timeout(timeout) {
        Ok(Some(_)) => false,
        Ok(None) => {
            child.kill().ok();
            child.wait().ok();
            true
        }
        Err(e) => {
            return Err(VoomError::ToolExecution {
                tool: "ffprobe".into(),
                message: format!("error waiting for ffprobe: {e}"),
            });
        }
    };

    let stdout_data = stdout_handle
        .join()
        .map_err(|_| VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: "stdout reader thread panicked".into(),
        })?
        .map_err(|e| VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!("failed to read stdout: {e}"),
        })?;

    let stderr_data = stderr_handle
        .join()
        .map_err(|_| VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: "stderr reader thread panicked".into(),
        })?
        .map_err(|e| VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!("failed to read stderr: {e}"),
        })?;

    if timed_out {
        return Err(VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!("ffprobe timed out after {}s", timeout.as_secs()),
        });
    }

    let status = child.wait().map_err(|e| VoomError::ToolExecution {
        tool: "ffprobe".into(),
        message: format!("failed to get exit status: {e}"),
    })?;

    if !status.success() {
        return Err(VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!(
                "ffprobe exited with status {}: {}",
                status,
                stderr_data.trim()
            ),
        });
    }

    let json: serde_json::Value =
        serde_json::from_str(&stdout_data).map_err(|e| VoomError::ToolExecution {
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
        let result = run_ffprobe(
            "/nonexistent/ffprobe",
            Path::new("/dummy.mkv"),
            Duration::from_secs(60),
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            VoomError::ToolNotFound { tool } => {
                assert_eq!(tool, "/nonexistent/ffprobe");
            }
            other => panic!("expected ToolNotFound, got: {other}"),
        }
    }
}
