use std::path::Path;
use std::time::Duration;

use voom_domain::errors::{Result, VoomError};

/// Run ffprobe on a file and return the JSON output.
///
/// Uses a timeout to prevent ffprobe from hanging indefinitely on
/// corrupted or problematic files.
pub fn run_ffprobe(
    ffprobe_path: &str,
    file_path: &Path,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let file_arg = file_path.as_os_str().to_os_string();
    let args: Vec<std::ffi::OsString> = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        "-show_entries",
        "stream_side_data",
    ]
    .iter()
    .map(std::ffi::OsString::from)
    .chain(std::iter::once(file_arg))
    .collect();

    let output =
        voom_process::run_with_timeout(ffprobe_path, &args, timeout).map_err(|e| match &e {
            VoomError::ToolExecution { message, .. }
                if message.contains("No such file or directory")
                    || message.contains("os error 2") =>
            {
                VoomError::ToolNotFound {
                    tool: ffprobe_path.to_string(),
                }
            }
            _ => e,
        })?;

    if !output.status.success() {
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        return Err(VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!(
                "ffprobe exited with status {}: {}",
                output.status,
                stderr_text.trim()
            ),
        });
    }

    let stdout_data = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout_data).map_err(|e| VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!("failed to parse ffprobe JSON output: {e}"),
        })?;

    Ok(json)
}

/// Check if ffprobe is available and return its version.
pub fn detect_ffprobe(ffprobe_path: &str) -> Result<String> {
    let output =
        voom_process::run_with_timeout(ffprobe_path, &["-version"], Duration::from_secs(10))
            .map_err(|e| match &e {
                VoomError::ToolExecution { message, .. }
                    if message.contains("No such file or directory")
                        || message.contains("os error 2") =>
                {
                    VoomError::ToolNotFound {
                        tool: ffprobe_path.to_string(),
                    }
                }
                _ => e,
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
