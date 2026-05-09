//! Quick mode: ffprobe header check.
//!
//! Runs `ffprobe -v error -show_entries format=duration <file>` and
//! interprets stderr/exit. Cheap (<1s typical).

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::verification::{
    VerificationMode, VerificationOutcome, VerificationRecord, VerificationRecordInput,
};

use crate::util::truncate;

/// Run quick verification on `path` for the file with id `file_id`.
///
/// # Errors
/// Returns `VoomError::ToolExecution` if ffprobe cannot be spawned or
/// times out. A non-zero exit status from ffprobe is captured as an
/// `Error` outcome in the returned record, not as an `Err`.
pub fn run_quick(
    file_id: &str,
    path: &Path,
    ffprobe_path: &str,
    timeout: Duration,
) -> Result<VerificationRecord> {
    let args: Vec<std::ffi::OsString> = [
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ]
    .iter()
    .map(std::ffi::OsString::from)
    .chain(std::iter::once(path.as_os_str().to_os_string()))
    .collect();

    let started = Utc::now();
    let output = voom_process::run_with_timeout(ffprobe_path, &args, timeout).map_err(|e| {
        VoomError::ToolExecution {
            tool: "ffprobe".into(),
            message: format!("quick verify: {e}"),
        }
    })?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let outcome = if !output.status.success() {
        VerificationOutcome::Error
    } else if stderr.trim().is_empty() {
        VerificationOutcome::Ok
    } else {
        VerificationOutcome::Warning
    };
    let error_count = u32::from(outcome == VerificationOutcome::Error);
    let warning_count = if outcome == VerificationOutcome::Warning {
        u32::try_from(stderr.lines().count()).unwrap_or(u32::MAX)
    } else {
        0
    };

    let truncated = truncate(&stderr, 4096);
    let details = if truncated.is_empty() {
        None
    } else {
        Some(truncated)
    };

    Ok(VerificationRecord::new(VerificationRecordInput {
        id: Uuid::new_v4(),
        file_id: file_id.to_string(),
        verified_at: started,
        mode: VerificationMode::Quick,
        outcome,
        error_count,
        warning_count,
        content_hash: None,
        details,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_strings() {
        let s = "a".repeat(5000);
        let t = truncate(&s, 4096);
        assert!(t.len() <= 4096 + "...[truncated]".len());
        assert!(t.ends_with("[truncated]"));
    }

    #[test]
    fn truncate_handles_multibyte_safely() {
        let s = "ä".repeat(5000);
        let t = truncate(&s, 100);
        assert!(t.ends_with("[truncated]"));
    }

    #[test]
    fn run_quick_with_missing_tool_errors() {
        let r = run_quick(
            "file-id",
            Path::new("/dev/null"),
            "/nonexistent/ffprobe-binary",
            Duration::from_secs(5),
        );
        assert!(r.is_err());
    }
}
