use std::path::{Path, PathBuf};
use std::process::Command;

use voom_ffmpeg_executor::vmaf::{
    FullSample, SampleExtractor, SceneSample, UniformSample, VmafError, VmafModel, compute_vmaf,
    pick_model_for_resolution,
};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ffmpeg_reports_libvmaf() -> bool {
    let output = Command::new("ffmpeg")
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

fn ffmpeg_sampling_available() -> bool {
    ffmpeg_available() && ffprobe_available()
}

fn synthetic_video(path: &Path, duration_secs: u32) {
    let duration = duration_secs.to_string();
    let source = format!("testsrc2=size=160x90:rate=12:duration={duration}");
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &source,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffmpeg fixture generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn video_duration(path: &Path) -> f64 {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe duration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .unwrap()
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
fn uniform_sample_extracts_evenly_spaced_segments() {
    if !ffmpeg_sampling_available() {
        eprintln!("skipping: ffmpeg or ffprobe not found on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let dest = dir.path().join("uniform.mkv");
    synthetic_video(&source, 300);
    let sample = UniformSample {
        count: 6,
        duration_secs: 10,
    };

    sample.extract(&source, &dest).unwrap();

    let duration = video_duration(&dest);
    assert!(
        (58.0..=62.0).contains(&duration),
        "expected about 60s, got {duration}"
    );
}

#[test]
fn scene_sample_extracts_no_more_than_requested_duration() {
    if !ffmpeg_sampling_available() {
        eprintln!("skipping: ffmpeg or ffprobe not found on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let dest = dir.path().join("scene.mkv");
    synthetic_video(&source, 30);
    let sample = SceneSample {
        count: 3,
        duration_secs: 5,
    };

    sample.extract(&source, &dest).unwrap();

    let duration = video_duration(&dest);
    assert!(duration > 0.0);
    assert!(duration <= 16.0, "expected at most 15s, got {duration}");
}

#[test]
fn uniform_sample_copies_short_sources_instead_of_padding() {
    if !ffmpeg_sampling_available() {
        eprintln!("skipping: ffmpeg or ffprobe not found on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let dest = dir.path().join("short.mkv");
    synthetic_video(&source, 4);
    let sample = UniformSample {
        count: 6,
        duration_secs: 10,
    };

    sample.extract(&source, &dest).unwrap();

    let duration = video_duration(&dest);
    assert!(
        (3.0..=5.0).contains(&duration),
        "expected about 4s, got {duration}"
    );
}

#[test]
fn uniform_sample_count_one_extracts_one_segment() {
    if !ffmpeg_sampling_available() {
        eprintln!("skipping: ffmpeg or ffprobe not found on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let dest = dir.path().join("one.mkv");
    synthetic_video(&source, 20);
    let sample = UniformSample {
        count: 1,
        duration_secs: 5,
    };

    sample.extract(&source, &dest).unwrap();

    let duration = video_duration(&dest);
    assert!(
        (4.0..=6.0).contains(&duration),
        "expected about 5s, got {duration}"
    );
}

#[test]
fn uniform_sample_count_zero_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let dest = dir.path().join("zero.mkv");
    std::fs::write(&source, b"not a video").unwrap();
    let sample = UniformSample {
        count: 0,
        duration_secs: 5,
    };

    let err = sample.extract(&source, &dest).unwrap_err();

    assert!(err.to_string().contains("count must be greater than zero"));
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
