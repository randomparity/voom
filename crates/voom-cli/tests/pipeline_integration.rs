//! Cross-crate pipeline integration test.
//!
//! Exercises the full DSL parse → compile → evaluate → plan flow,
//! verifying that a `.voom` policy string produces the expected
//! [`Plan`] actions when evaluated against a sample [`MediaFile`].

use std::path::PathBuf;

use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::OperationType;
use voom_dsl::compile_policy as compile;
use voom_policy_evaluator::evaluator::evaluate;

/// Build a realistic media file with multiple track types.
fn sample_file() -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from("/media/Movie.mkv"));
    file.container = Container::Mkv;
    file.tracks = vec![
        // 0: video – HEVC 1080p
        {
            let mut t = Track::new(0, TrackType::Video, "hevc".into());
            t.width = Some(1920);
            t.height = Some(1080);
            t
        },
        // 1: primary English DTS audio
        {
            let mut t = Track::new(1, TrackType::AudioMain, "dts".into());
            t.language = "eng".into();
            t.channels = Some(6);
            t.is_default = true;
            t
        },
        // 2: Japanese AAC audio (should be removed by eng-only keep)
        {
            let mut t = Track::new(2, TrackType::AudioAlternate, "aac".into());
            t.language = "jpn".into();
            t.channels = Some(2);
            t
        },
        // 3: English SRT subtitle
        {
            let mut t = Track::new(3, TrackType::SubtitleMain, "srt".into());
            t.language = "eng".into();
            t.is_default = true;
            t
        },
        // 4: French subtitle (should be removed by eng-only keep)
        {
            let mut t = Track::new(4, TrackType::SubtitleForced, "srt".into());
            t.language = "fra".into();
            t
        },
    ];
    file
}

/// Full pipeline: parse → compile → evaluate a multi-phase policy and verify
/// that the resulting plans contain exactly the expected actions.
#[test]
fn pipeline_parse_compile_evaluate_produces_correct_plans() {
    let source = r#"
        policy "integration-test" {
            phase normalize {
                container mkv
                keep audio where lang in [eng]
                keep subtitles where lang in [eng]
            }

            phase transcode {
                depends_on: [normalize]
                transcode audio to aac {
                    channels: 2
                    bitrate: "128k"
                }
            }
        }
    "#;

    // Step 1+2: parse + compile (voom_dsl::compile_policy does both)
    let compiled = compile(source).expect("policy should compile");
    assert_eq!(compiled.name, "integration-test");
    assert_eq!(compiled.phases.len(), 2);
    assert_eq!(compiled.phase_order, vec!["normalize", "transcode"]);

    // Step 3: build a sample media file
    let file = sample_file();

    // Step 4: evaluate the compiled policy against the file
    let result = evaluate(&compiled, &file);
    assert_eq!(result.plans.len(), 2, "expected one plan per phase");

    // ---- Phase 1: normalize ----
    let normalize_plan = &result.plans[0];
    assert_eq!(normalize_plan.phase_name, "normalize");
    assert!(
        normalize_plan.skip_reason.is_none(),
        "normalize should not be skipped"
    );

    // Container is already MKV, so no ConvertContainer action expected.
    let container_actions: Vec<_> = normalize_plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::ConvertContainer)
        .collect();
    assert!(
        container_actions.is_empty(),
        "container is already mkv, no conversion needed"
    );

    // "keep audio where lang in [eng]" should remove track 2 (jpn audio)
    let audio_removes: Vec<_> = normalize_plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::RemoveTrack && a.track_index == Some(2))
        .collect();
    assert_eq!(
        audio_removes.len(),
        1,
        "Japanese audio track should be removed"
    );

    // "keep subtitles where lang in [eng]" should remove track 4 (fra subtitle)
    let sub_removes: Vec<_> = normalize_plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::RemoveTrack && a.track_index == Some(4))
        .collect();
    assert_eq!(
        sub_removes.len(),
        1,
        "French subtitle track should be removed"
    );

    // English tracks (1, 3) should NOT be removed
    let wrongly_removed: Vec<_> = normalize_plan
        .actions
        .iter()
        .filter(|a| {
            a.operation == OperationType::RemoveTrack
                && (a.track_index == Some(1) || a.track_index == Some(3))
        })
        .collect();
    assert!(
        wrongly_removed.is_empty(),
        "English tracks should be kept, not removed"
    );

    // ---- Phase 2: transcode ----
    let transcode_plan = &result.plans[1];
    assert_eq!(transcode_plan.phase_name, "transcode");
    assert!(
        transcode_plan.skip_reason.is_none(),
        "transcode should not be skipped (normalize produced modifications)"
    );

    // "transcode audio to aac" should produce TranscodeAudio actions
    let transcode_actions: Vec<_> = transcode_plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::TranscodeAudio)
        .collect();
    assert!(
        !transcode_actions.is_empty(),
        "transcode phase should produce TranscodeAudio actions"
    );
}

/// Verify that skip_when correctly prevents phase execution.
#[test]
fn pipeline_skip_when_prevents_phase_execution() {
    // skip when the video codec is hevc (which our sample file has)
    let source = r#"
        policy "skip-test" {
            phase maybe_transcode {
                skip when video.codec == "hevc"
                transcode video to av1 { crf: 30 }
            }
        }
    "#;

    let compiled = compile(source).expect("policy should compile");
    let file = sample_file(); // video codec is hevc

    let result = evaluate(&compiled, &file);
    assert_eq!(result.plans.len(), 1);

    let plan = &result.plans[0];
    assert!(
        plan.skip_reason.is_some(),
        "phase should be skipped because video codec is hevc"
    );
    assert!(
        plan.actions.is_empty(),
        "skipped phase should produce no actions"
    );
}

/// Verify that a policy with no matching operations produces empty plans.
#[test]
fn pipeline_no_changes_needed_produces_empty_plan() {
    let source = r#"
        policy "noop" {
            phase check {
                container mkv
                keep audio where lang in [eng, jpn]
                keep subtitles where lang in [eng, fra]
            }
        }
    "#;

    let compiled = compile(source).expect("policy should compile");
    let file = sample_file();

    let result = evaluate(&compiled, &file);
    assert_eq!(result.plans.len(), 1);

    let plan = &result.plans[0];
    assert!(
        plan.actions.is_empty(),
        "all tracks match the keep filter — no removals expected"
    );
}

/// Verify that run_if modified skips a dependent phase when its
/// dependency produced no modifications.
#[test]
fn pipeline_run_if_modified_skips_when_no_changes() {
    let source = r#"
        policy "dep-test" {
            phase normalize {
                container mkv
                keep audio where lang in [eng, jpn]
                keep subtitles where lang in [eng, fra]
            }

            phase post {
                depends_on: [normalize]
                run_if normalize.modified
                container mp4
            }
        }
    "#;

    let compiled = compile(source).expect("policy should compile");
    let file = sample_file(); // already MKV, all languages kept

    let result = evaluate(&compiled, &file);
    assert_eq!(result.plans.len(), 2);

    // normalize should produce no actions (everything matches)
    assert!(
        result.plans[0].actions.is_empty(),
        "normalize should produce no changes"
    );

    // post should be skipped because normalize didn't modify anything
    assert!(
        result.plans[1].skip_reason.is_some(),
        "post phase should be skipped since normalize produced no modifications"
    );
}
