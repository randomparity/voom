//! Thorough mode: full ffmpeg decode pass.
//!
//! Runs `ffmpeg -v error -i <file> -f null -` and counts error/warning lines
//! on stderr. Detects truncated streams, packet errors, decode failures.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::verification::{VerificationMode, VerificationOutcome, VerificationRecord};

use crate::util::truncate;

/// Run thorough verification on `path`. `timeout` is the absolute kill-after.
///
/// # Errors
/// Returns an error if the tool cannot be invoked or times out.
pub fn run_thorough(
    file_id: &str,
    path: &Path,
    ffmpeg_path: &str,
    timeout: Duration,
) -> Result<VerificationRecord> {
    let path_str = path.to_str().ok_or_else(|| VoomError::ToolExecution {
        tool: "ffmpeg".into(),
        message: format!("path is not valid UTF-8: {}", path.display()),
    })?;
    let args: Vec<std::ffi::OsString> = ["-v", "error", "-i", path_str, "-f", "null", "-"]
        .iter()
        .map(std::ffi::OsString::from)
        .collect();

    let started = Utc::now();
    let output = voom_process::run_with_timeout(ffmpeg_path, &args, timeout).map_err(|e| {
        VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("thorough verify: {e}"),
        }
    })?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let (error_count, warning_count) = classify_lines(&stderr);
    let outcome = if !output.status.success() || error_count > 0 {
        VerificationOutcome::Error
    } else if warning_count > 0 {
        VerificationOutcome::Warning
    } else {
        VerificationOutcome::Ok
    };

    let details = if stderr.trim().is_empty() {
        None
    } else {
        Some(truncate(&stderr, 16 * 1024))
    };

    Ok(VerificationRecord::new(
        Uuid::new_v4(),
        file_id,
        started,
        VerificationMode::Thorough,
        outcome,
        error_count,
        warning_count,
        None,
        details,
    ))
}

/// Classify `ffmpeg -v error` stderr.  At loglevel `error` ffmpeg emits one
/// line per problem. We treat every non-empty line as an error; `-v error`
/// already filters out warnings, so the warning count is zero unless the
/// caller bumps verbosity. (Kept for symmetry with the schema.)
fn classify_lines(stderr: &str) -> (u32, u32) {
    let errors: u32 =
        u32::try_from(stderr.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(u32::MAX);
    (errors, 0)
}

/// Compute a thorough-mode timeout from a known duration.
/// Falls back to the floor if `duration` is None or zero.
#[must_use]
pub fn timeout_from_duration(duration: Option<f64>, multiplier: f32, floor_secs: u64) -> Duration {
    let candidate = duration
        .filter(|d| *d > 0.0)
        .map(|d| {
            // Saturate at u64::MAX rather than wrapping
            let scaled = d * f64::from(multiplier);
            if scaled.is_finite() && scaled >= 0.0 && scaled < u64::MAX as f64 {
                scaled as u64
            } else {
                u64::MAX
            }
        })
        .unwrap_or(0);
    Duration::from_secs(candidate.max(floor_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_counts_lines() {
        let (e, w) = classify_lines("a\nb\nc\n");
        assert_eq!(e, 3);
        assert_eq!(w, 0);
    }

    #[test]
    fn classify_ignores_blank_lines() {
        let (e, _) = classify_lines("\n\n");
        assert_eq!(e, 0);
    }

    #[test]
    fn timeout_uses_floor_when_no_duration() {
        let t = timeout_from_duration(None, 4.0, 60);
        assert_eq!(t, Duration::from_secs(60));
    }

    #[test]
    fn timeout_scales_with_duration() {
        let t = timeout_from_duration(Some(120.0), 4.0, 60);
        assert_eq!(t, Duration::from_secs(480));
    }
}
