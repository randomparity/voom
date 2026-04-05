//! Disk space utilities.

use std::path::Path;

use crate::errors::{Result, VoomError};
use crate::plan::{OperationType, Plan};

/// Get available disk space for a path (bytes).
///
/// Walks up to the nearest existing ancestor if the path doesn't exist yet.
#[cfg(unix)]
pub fn available_space(path: &Path) -> Result<u64> {
    let mut check = path.to_path_buf();
    while !check.exists() {
        match check.parent() {
            Some(p) => check = p.to_path_buf(),
            None => break,
        }
    }

    use std::ffi::CString;
    let c_path = CString::new(check.to_string_lossy().as_bytes())
        .map_err(|e| VoomError::Validation(format!("invalid path for statvfs: {e}")))?;

    // SAFETY: `c_path` is a valid NUL-terminated C string (from CString::new
    // above), and `stat` is passed as an out-pointer that statvfs will fully
    // initialize on success.
    unsafe {
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) == 0 {
            let stat = stat.assume_init();
            #[allow(clippy::unnecessary_cast)]
            let avail = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
            Ok(avail)
        } else {
            Err(VoomError::Io(std::io::Error::last_os_error()))
        }
    }
}

/// On non-Unix platforms, return a large value to avoid blocking.
#[cfg(not(unix))]
pub fn available_space(_path: &Path) -> Result<u64> {
    Ok(u64::MAX)
}

/// Minimum free space reserve (50 MB) to avoid filling the disk to zero.
pub const MINIMUM_RESERVE_BYTES: u64 = 50 * 1024 * 1024;

/// Estimate the disk space required to execute a plan.
///
/// - Transcode/convert/synthesize operations produce a new file alongside the
///   original, so we estimate 2x the input file size.
/// - Remux operations (track manipulation) also write a new file, but the
///   output is typically smaller; we estimate 1x as a conservative baseline.
/// - A fixed reserve ([`MINIMUM_RESERVE_BYTES`]) is always added.
#[must_use]
pub fn estimate_required_space(plan: &Plan, file_size: u64) -> u64 {
    let needs_extra = plan.actions.iter().any(|a| {
        matches!(
            a.operation,
            OperationType::TranscodeVideo
                | OperationType::TranscodeAudio
                | OperationType::ConvertContainer
                | OperationType::SynthesizeAudio
        )
    });

    let multiplier: u64 = if needs_extra { 2 } else { 1 };
    file_size
        .saturating_mul(multiplier)
        .saturating_add(MINIMUM_RESERVE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaFile;
    use crate::plan::{ActionParams, OperationType, Plan, PlannedAction};
    use std::path::PathBuf;

    fn test_file(size: u64) -> MediaFile {
        let mut f = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        f.size = size;
        f
    }

    fn plan_with_ops(file: &MediaFile, ops: &[OperationType]) -> Plan {
        let mut plan = Plan::new(file.clone(), "test-policy", "test-phase");
        for op in ops {
            plan.actions
                .push(PlannedAction::track_op(*op, 0, ActionParams::Empty, "test"));
        }
        plan
    }

    #[test]
    fn test_estimate_remux_plan() {
        let file = test_file(1_000_000_000); // 1 GB
        let plan = plan_with_ops(&file, &[OperationType::RemoveTrack]);
        let required = estimate_required_space(&plan, file.size);
        // Remux: 1x file size + 50 MB reserve
        assert_eq!(required, 1_000_000_000 + MINIMUM_RESERVE_BYTES);
    }

    #[test]
    fn test_estimate_transcode_plan() {
        let file = test_file(1_000_000_000);
        let plan = plan_with_ops(&file, &[OperationType::TranscodeVideo]);
        let required = estimate_required_space(&plan, file.size);
        // Transcode: 2x file size + 50 MB reserve
        assert_eq!(required, 2_000_000_000 + MINIMUM_RESERVE_BYTES);
    }

    #[test]
    fn test_estimate_convert_container_plan() {
        let file = test_file(500_000_000);
        let plan = plan_with_ops(&file, &[OperationType::ConvertContainer]);
        let required = estimate_required_space(&plan, file.size);
        // Container conversion: 2x file size + 50 MB reserve
        assert_eq!(required, 1_000_000_000 + MINIMUM_RESERVE_BYTES);
    }

    #[test]
    fn test_estimate_synthesize_audio_plan() {
        let file = test_file(500_000_000);
        let plan = plan_with_ops(&file, &[OperationType::SynthesizeAudio]);
        let required = estimate_required_space(&plan, file.size);
        // Synthesize: 2x file size + 50 MB reserve
        assert_eq!(required, 1_000_000_000 + MINIMUM_RESERVE_BYTES);
    }

    #[test]
    fn test_estimate_mixed_plan_uses_highest_multiplier() {
        let file = test_file(1_000_000_000);
        let plan = plan_with_ops(
            &file,
            &[OperationType::SetDefault, OperationType::TranscodeAudio],
        );
        let required = estimate_required_space(&plan, file.size);
        // TranscodeAudio bumps to 2x
        assert_eq!(required, 2_000_000_000 + MINIMUM_RESERVE_BYTES);
    }

    #[test]
    fn test_estimate_empty_plan() {
        let file = test_file(1_000_000_000);
        let plan = Plan::new(file.clone(), "test-policy", "test-phase");
        let required = estimate_required_space(&plan, file.size);
        // Empty plan: 1x file size + reserve (still need space for remux)
        assert_eq!(required, 1_000_000_000 + MINIMUM_RESERVE_BYTES);
    }
}
