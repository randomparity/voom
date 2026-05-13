#![cfg(feature = "integration")]

use std::path::{Path, PathBuf};
use std::process::Command;

use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{
    ActionParams, OperationType, Plan, PlannedAction, SampleStrategy, TranscodeSettings,
};
use voom_domain::storage::{FileStorage, TranscodeOutcomeFilters, TranscodeOutcomeStorage};
use voom_ffmpeg_executor::executor::execute_plan_with_outcomes;
use voom_ffmpeg_executor::hwaccel::HwAccelConfig;
use voom_sqlite_store::store::SqliteStore;

const TARGET_TOLERANCE: f32 = 2.0;
const MAX_ITERATIONS: u32 = 5;

#[derive(Clone, Copy)]
struct CorpusCase {
    name: &'static str,
    target_vmafs: &'static [u32],
}

const CORPUS_CASES: &[CorpusCase] = &[
    CorpusCase {
        name: "clean",
        target_vmafs: &[92, 95],
    },
    CorpusCase {
        name: "noisy",
        target_vmafs: &[92, 95],
    },
    CorpusCase {
        name: "animated",
        target_vmafs: &[88, 92, 95],
    },
    CorpusCase {
        name: "4k-downscale",
        target_vmafs: &[92, 95],
    },
    CorpusCase {
        name: "mixed-motion",
        target_vmafs: &[92, 95],
    },
    CorpusCase {
        name: "low-motion",
        target_vmafs: &[92, 95],
    },
];

fn ffmpeg_supports_libvmaf() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("libvmaf")
        })
}

fn generate_corpus(out_dir: &Path) {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = repo_root.join("tests/vmaf_corpus/generate.sh");
    let output = Command::new(script)
        .args(["--duration", "3", "--size", "320x180"])
        .arg(out_dir)
        .output()
        .expect("run VMAF corpus generator");
    assert!(
        output.status.success(),
        "corpus generation failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn media_file(path: &Path) -> MediaFile {
    let mut file = MediaFile::new(path.to_path_buf());
    file.container = Container::Mkv;
    file.size = std::fs::metadata(path)
        .expect("generated clip metadata")
        .len();
    file.tracks = vec![{
        let mut track = Track::new(0, TrackType::Video, "h264".to_string());
        track.width = Some(320);
        track.height = Some(180);
        track
    }];
    file
}

fn vmaf_plan(file: MediaFile, target_vmaf: u32) -> Plan {
    let settings = TranscodeSettings::default()
        .with_target_vmaf(Some(target_vmaf))
        .with_sample_strategy(Some(SampleStrategy::Full));
    Plan::new(file, "vmaf-e2e", "transcode-video").with_action(PlannedAction::track_op(
        OperationType::TranscodeVideo,
        0,
        ActionParams::Transcode {
            codec: "h264".to_string(),
            settings,
        },
        format!("transcode video to target VMAF {target_vmaf}"),
    ))
}

#[test]
fn vmaf_guided_transcodes_hit_targets_and_persist_outcomes() {
    if !ffmpeg_supports_libvmaf() {
        eprintln!("skipping: ffmpeg libvmaf filter not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let corpus_dir = dir.path().join("corpus");
    generate_corpus(&corpus_dir);

    let store = SqliteStore::open(&dir.path().join("voom.db")).expect("sqlite store");
    for case in CORPUS_CASES {
        for target_vmaf in case.target_vmafs {
            let source = corpus_dir.join(format!("{}.mkv", case.name));
            let clip = dir
                .path()
                .join(format!("{}-target-{}.mkv", case.name, target_vmaf));
            std::fs::copy(&source, &clip).expect("copy generated clip");
            let file = media_file(&clip);
            store.upsert_file(&file).expect("persist source file");

            let execution = execute_plan_with_outcomes(
                &vmaf_plan(file.clone(), *target_vmaf),
                &HwAccelConfig::new(),
                None,
            )
            .expect("execute VMAF-guided transcode");
            assert!(
                execution.action_results.iter().all(|result| result.success),
                "transcode failed for {} target {}",
                case.name,
                target_vmaf
            );
            assert_eq!(execution.transcode_outcomes.len(), 1);

            let outcome = &execution.transcode_outcomes[0];
            assert!(
                !outcome.fallback_used,
                "fallback used for {} target {}; outcome: {:?}",
                case.name, target_vmaf, outcome
            );
            store
                .insert_transcode_outcome(outcome)
                .expect("persist transcode outcome");

            let achieved = outcome.achieved_vmaf.expect("achieved VMAF");
            assert!(
                (achieved - *target_vmaf as f32).abs() <= TARGET_TOLERANCE,
                "{} target {} achieved {}",
                case.name,
                target_vmaf,
                achieved
            );
            assert!(outcome.iterations <= MAX_ITERATIONS);

            let mut filters = TranscodeOutcomeFilters::default();
            filters.file_id = Some(file.id.to_string());
            filters.limit = Some(1);
            let rows = store
                .list_transcode_outcomes(&filters)
                .expect("list transcode outcomes");
            let row = rows.first().expect("persisted outcome row");
            assert_eq!(row.target_vmaf, Some(*target_vmaf));
            assert_eq!(row.achieved_vmaf, Some(achieved));
            assert_eq!(row.iterations, outcome.iterations);
            assert_eq!(row.sample_strategy, SampleStrategy::Full);
            assert!(!row.fallback_used);
        }
    }
}
