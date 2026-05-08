use std::path::{Path, PathBuf};

use voom_ffmpeg_executor::vmaf::{
    compute_vmaf, pick_model_for_resolution, FullSample, SampleExtractor, SceneSample,
    UniformSample, VmafError, VmafModel,
};

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ffmpeg_reports_libvmaf() -> bool {
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output();
    output.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().any(|token| token == "libvmaf"))
    })
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vmaf")
        .join(name)
}

fn fixture_pair_available() -> bool {
    fixture_path("reference.mkv").is_file() && fixture_path("distorted.mkv").is_file()
}

#[test]
fn pick_model_for_resolution_selects_common_models() {
    assert_eq!(pick_model_for_resolution(1920, 1080), VmafModel::V061);
    assert_eq!(pick_model_for_resolution(3840, 2160), VmafModel::V4k);
    assert_eq!(pick_model_for_resolution(720, 1280), VmafModel::Phone);
}

#[test]
fn full_sample_copies_source_to_destination() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let dest = dir.path().join("dest.mkv");
    std::fs::write(&source, b"sample bytes").unwrap();

    FullSample.extract(&source, &dest).unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), b"sample bytes");
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn uniform_sample_is_a_stub_for_later_sampling_work() {
    let sample = UniformSample {
        count: 3,
        duration_secs: 5,
    };
    sample
        .extract(Path::new("source.mkv"), Path::new("dest.mkv"))
        .unwrap();
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn scene_sample_is_a_stub_for_later_sampling_work() {
    let sample = SceneSample {
        count: 3,
        duration_secs: 5,
    };
    sample
        .extract(Path::new("source.mkv"), Path::new("dest.mkv"))
        .unwrap();
}

#[test]
fn compute_vmaf_returns_plausible_score_for_synthetic_fixture_pair() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not found on PATH");
        return;
    }
    if !ffmpeg_reports_libvmaf() {
        eprintln!("skipping: ffmpeg does not report libvmaf support");
        return;
    }
    if !fixture_pair_available() {
        eprintln!("skipping: VMAF fixture pair not present");
        return;
    }

    let score = compute_vmaf(
        &fixture_path("reference.mkv"),
        &fixture_path("distorted.mkv"),
        VmafModel::V061,
    )
    .unwrap();

    assert!(score > 0.0);
    assert!(score < 100.0);
}

#[test]
fn compute_vmaf_reports_unavailable_when_ffmpeg_lacks_libvmaf() {
    if !ffmpeg_available() || !fixture_pair_available() {
        eprintln!("skipping: ffmpeg or fixture pair unavailable");
        return;
    }
    if ffmpeg_reports_libvmaf() {
        eprintln!("skipping: local ffmpeg reports libvmaf support");
        return;
    }

    let err = compute_vmaf(
        &fixture_path("reference.mkv"),
        &fixture_path("distorted.mkv"),
        VmafModel::V061,
    )
    .unwrap_err();

    assert!(matches!(err, VmafError::LibvmafUnavailable));
}
