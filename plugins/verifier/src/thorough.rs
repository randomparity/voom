//! Thorough mode: full ffmpeg decode pass.
//!
//! Runs `ffmpeg -v error -i <file> -f null -` and counts error/warning lines
//! on stderr. Detects truncated streams, packet errors, decode failures.
//!
//! Optionally accepts a resolved HW decode backend (NVDEC/VAAPI/QSV/
//! VideoToolbox) which prepends `-hwaccel <name>` before `-i`. When HW
//! decode is in use, vendor diagnostic lines are filtered out before
//! counting errors so transparent CPU fallback inside ffmpeg doesn't show
//! up as decode failures.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::verification::{
    VerificationMode, VerificationOutcome, VerificationRecord, VerificationRecordInput,
};

use crate::hwaccel::{self, HwAccelMode};
use crate::util::truncate;

/// Run thorough verification on `path`. `timeout` is the absolute kill-after.
///
/// `hw_accel` pins a resolved decode backend (`None` ⇒ CPU). The caller is
/// responsible for resolving the backend against `ffmpeg -hwaccels`; this
/// function trusts the input and only emits the corresponding `-hwaccel`
/// arg.
///
/// # Errors
/// Returns an error if the tool cannot be invoked or times out.
pub fn run_thorough(
    file_id: &str,
    path: &Path,
    ffmpeg_path: &str,
    timeout: Duration,
    hw_accel: Option<HwAccelMode>,
) -> Result<VerificationRecord> {
    let path_str = path.to_str().ok_or_else(|| VoomError::ToolExecution {
        tool: "ffmpeg".into(),
        message: format!("path is not valid UTF-8: {}", path.display()),
    })?;

    let mut args: Vec<std::ffi::OsString> = Vec::with_capacity(8);
    args.push("-v".into());
    args.push("error".into());
    for arg in hwaccel::input_args(hw_accel) {
        args.push(arg.into());
    }
    args.push("-i".into());
    args.push(path_str.into());
    args.push("-f".into());
    args.push("null".into());
    args.push("-".into());

    let started = Utc::now();
    let output = voom_process::run_with_timeout(ffmpeg_path, &args, timeout).map_err(|e| {
        VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("thorough verify: {e}"),
        }
    })?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let hw_active = hw_accel.is_some();
    let (error_count, warning_count) = classify_lines(&stderr, hw_active);
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

    Ok(VerificationRecord::new(VerificationRecordInput {
        id: Uuid::new_v4(),
        file_id: file_id.to_string(),
        verified_at: started,
        mode: VerificationMode::Thorough,
        outcome,
        error_count,
        warning_count,
        content_hash: None,
        details,
    }))
}

/// Classify `ffmpeg -v error` stderr.  At loglevel `error` ffmpeg emits one
/// line per problem. We treat every non-empty line as an error; `-v error`
/// already filters out warnings, so the warning count is zero unless the
/// caller bumps verbosity. (Kept for symmetry with the schema.)
///
/// When `hw_active` is true, lines matching known HW-vendor diagnostic
/// patterns (init churn, transparent CPU fallback notices) are dropped
/// before counting so a successful HW-accelerated decode isn't reported
/// as an error.
fn classify_lines(stderr: &str, hw_active: bool) -> (u32, u32) {
    let errors = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !(hw_active && hwaccel::is_hwaccel_noise(l)))
        .count();
    (u32::try_from(errors).unwrap_or(u32::MAX), 0)
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
        let (e, w) = classify_lines("a\nb\nc\n", false);
        assert_eq!(e, 3);
        assert_eq!(w, 0);
    }

    #[test]
    fn classify_ignores_blank_lines() {
        let (e, _) = classify_lines("\n\n", false);
        assert_eq!(e, 0);
    }

    #[test]
    fn classify_filters_hw_noise_when_active() {
        let stderr = "Failed setup for format cuda: hwaccel initialisation returned error\n\
                      [matroska,webm @ 0x55] Truncated packet\n";
        let (e_hw, _) = classify_lines(stderr, true);
        let (e_cpu, _) = classify_lines(stderr, false);
        assert_eq!(e_hw, 1, "hw-active mode drops the init-error line");
        assert_eq!(e_cpu, 2, "cpu mode counts both lines");
    }

    #[test]
    fn classify_keeps_real_errors_under_hw() {
        let stderr = "[h264 @ 0x42] error while decoding MB 100 50, bytestream\n";
        let (e_hw, _) = classify_lines(stderr, true);
        assert_eq!(e_hw, 1);
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
