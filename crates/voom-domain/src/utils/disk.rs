//! Disk space utilities.

#[cfg(unix)]
use std::ffi::CString;
use std::path::Path;

use crate::errors::{Result, VoomError};
use crate::plan::{ActionParams, OperationType, Plan};

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
/// - `MuxSubtitle` operations append an external subtitle file into the
///   container, producing output larger than the input. The size of each
///   referenced subtitle file is added on top of the multiplier-based base.
///   If a subtitle file cannot be stat'd (missing or unreadable), it
///   contributes zero, a warning is logged via `tracing`, the executor will
///   surface the real error, and the reserve absorbs small misses.
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

    let subtitle_bytes: u64 = plan
        .actions
        .iter()
        .filter_map(|a| match &a.parameters {
            ActionParams::MuxSubtitle { subtitle_path, .. } => {
                match std::fs::metadata(subtitle_path) {
                    Ok(m) => Some(m.len()),
                    Err(e) => {
                        tracing::warn!(
                            path = %subtitle_path.display(),
                            error = %e,
                            "cannot stat subtitle file; contributing 0 to disk space estimate"
                        );
                        None
                    }
                }
            }
            _ => None,
        })
        .fold(0u64, u64::saturating_add);

    let multiplier: u64 = if needs_extra { 2 } else { 1 };
    file_size
        .saturating_mul(multiplier)
        .saturating_add(subtitle_bytes)
        .saturating_add(MINIMUM_RESERVE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Container, MediaFile, TrackType};
    use crate::plan::{ActionParams, OperationType, Plan, PlannedAction, TranscodeSettings};
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
                .push(PlannedAction::track_op(*op, 0, params_for_op(*op), "test"));
        }
        plan
    }

    fn params_for_op(op: OperationType) -> ActionParams {
        match op {
            OperationType::ConvertContainer => ActionParams::Container {
                container: Container::Mp4,
            },
            OperationType::RemoveTrack => ActionParams::RemoveTrack {
                reason: "test removal".into(),
                track_type: TrackType::AudioCommentary,
            },
            OperationType::TranscodeVideo | OperationType::TranscodeAudio => {
                ActionParams::Transcode {
                    codec: "h264".into(),
                    settings: TranscodeSettings::default(),
                }
            }
            OperationType::SynthesizeAudio => ActionParams::Synthesize {
                name: "descriptive audio".into(),
                language: Some("eng".into()),
                codec: Some("aac".into()),
                text: None,
                bitrate: None,
                channels: None,
                title: None,
                position: None,
                source_track: None,
            },
            OperationType::SetDefault
            | OperationType::ClearDefault
            | OperationType::SetForced
            | OperationType::ClearForced => ActionParams::Empty,
            _ => panic!("missing disk test params for {}", op.as_str()),
        }
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

    fn write_temp_subtitle(bytes: usize) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::Builder::new()
            .suffix(".srt")
            .tempfile()
            .expect("create temp subtitle");
        f.write_all(&vec![b'a'; bytes])
            .expect("write subtitle bytes");
        f.flush().expect("flush subtitle");
        f
    }

    fn mux_subtitle_action(path: PathBuf) -> PlannedAction {
        PlannedAction::file_op(
            OperationType::MuxSubtitle,
            ActionParams::MuxSubtitle {
                subtitle_path: path,
                language: "eng".into(),
                forced: false,
                title: None,
            },
            "mux subtitle",
        )
    }

    #[test]
    fn test_estimate_mux_subtitle_adds_subtitle_size() {
        let file = test_file(1_000_000_000);
        let sub = write_temp_subtitle(123_456);
        let mut plan = Plan::new(file.clone(), "test-policy", "test-phase");
        plan.actions
            .push(mux_subtitle_action(sub.path().to_path_buf()));

        let required = estimate_required_space(&plan, file.size);
        assert_eq!(
            required,
            1_000_000_000 + 123_456 + MINIMUM_RESERVE_BYTES,
            "MuxSubtitle should add the subtitle file size on top of the 1x base"
        );
    }

    #[test]
    fn test_estimate_mux_subtitle_missing_file_falls_back() {
        let file = test_file(1_000_000_000);
        let mut plan = Plan::new(file.clone(), "test-policy", "test-phase");
        plan.actions.push(mux_subtitle_action(PathBuf::from(
            "/nonexistent/path/does/not/exist.srt",
        )));

        let required = estimate_required_space(&plan, file.size);
        // Missing subtitle contributes zero — base 1x estimate + reserve.
        assert_eq!(required, 1_000_000_000 + MINIMUM_RESERVE_BYTES);
    }

    #[test]
    fn test_estimate_mux_subtitle_multiple_sums() {
        let file = test_file(500_000_000);
        let sub1 = write_temp_subtitle(40_000);
        let sub2 = write_temp_subtitle(60_000);
        let mut plan = Plan::new(file.clone(), "test-policy", "test-phase");
        plan.actions
            .push(mux_subtitle_action(sub1.path().to_path_buf()));
        plan.actions
            .push(mux_subtitle_action(sub2.path().to_path_buf()));

        let required = estimate_required_space(&plan, file.size);
        assert_eq!(
            required,
            500_000_000 + 40_000 + 60_000 + MINIMUM_RESERVE_BYTES
        );
    }

    #[test]
    fn test_estimate_mux_subtitle_with_transcode() {
        let file = test_file(1_000_000_000);
        let sub = write_temp_subtitle(100_000);
        let mut plan = Plan::new(file.clone(), "test-policy", "test-phase");
        plan.actions.push(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            params_for_op(OperationType::TranscodeVideo),
            "transcode",
        ));
        plan.actions
            .push(mux_subtitle_action(sub.path().to_path_buf()));

        let required = estimate_required_space(&plan, file.size);
        // 2x base from transcode + subtitle bytes + reserve.
        assert_eq!(
            required,
            2_000_000_000 + 100_000 + MINIMUM_RESERVE_BYTES,
            "subtitle size should add on top of the 2x transcode multiplier"
        );
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
