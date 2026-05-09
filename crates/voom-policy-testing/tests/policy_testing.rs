use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};
use voom_policy_testing::{
    all_capabilities, assert_audio_tracks_kept, assert_audio_tracks_synthesized,
    assert_no_warnings, assert_phases_run, assert_phases_skipped, assert_subtitle_tracks_kept,
    assert_video_codec, Assertions, Fixture, TestCase, TestSuite,
};

fn track(index: u32, track_type: TrackType, codec: &str) -> Track {
    Track::new(index, track_type, codec.to_string())
}

fn file() -> MediaFile {
    MediaFile::new(PathBuf::from("/media/movie.mp4"))
        .with_container(Container::Mp4)
        .with_duration(120.0)
        .with_tracks(vec![
            track(0, TrackType::Video, "h264"),
            track(1, TrackType::AudioMain, "aac"),
            track(2, TrackType::AudioCommentary, "aac"),
            track(3, TrackType::SubtitleMain, "srt"),
        ])
}

fn plan(phase: &str, actions: Vec<PlannedAction>) -> Plan {
    let mut plan = Plan::new(file(), "test", phase);
    plan.actions = actions;
    plan
}

fn remove_track(index: u32, track_type: TrackType) -> PlannedAction {
    PlannedAction::track_op(
        OperationType::RemoveTrack,
        index,
        ActionParams::RemoveTrack {
            reason: "not wanted".to_string(),
            track_type,
        },
        "remove track",
    )
}

#[test]
fn fixture_round_trips_through_json_without_losing_fields() {
    let fixture = Fixture {
        path: PathBuf::from("/media/movie.mkv"),
        container: Container::Mkv,
        duration: 61.5,
        size: 42,
        tracks: vec![track(0, TrackType::Video, "hevc")],
        capabilities: None,
    };

    let encoded = serde_json::to_string(&fixture).unwrap();
    let decoded: Fixture = serde_json::from_str(&encoded).unwrap();
    let media_file = decoded.to_media_file();

    assert_eq!(decoded.path, PathBuf::from("/media/movie.mkv"));
    assert_eq!(decoded.container, Container::Mkv);
    assert_eq!(decoded.duration, 61.5);
    assert_eq!(decoded.size, 42);
    assert_eq!(decoded.tracks[0].codec, "hevc");
    assert_eq!(media_file.path, decoded.path);
    assert_eq!(media_file.container, decoded.container);
    assert_eq!(media_file.duration, decoded.duration);
    assert_eq!(media_file.size, decoded.size);
    assert_eq!(media_file.tracks.len(), 1);
}

#[test]
fn test_suite_deserializes_flat_assertions() {
    let json = r#"{
        "policy": "docs/examples/minimal.voom",
        "cases": [{
            "name": "minimal",
            "fixture": "fixtures/movie.json",
            "expect": {
                "phases_run": ["init"],
                "phases_skipped": {"verify": "dependency"},
                "audio_tracks_kept": 1,
                "audio_tracks_synthesized": 0,
                "subtitle_tracks_kept": 1,
                "video_codec": "hevc",
                "no_warnings": true
            }
        }]
    }"#;

    let suite: TestSuite = serde_json::from_str(json).unwrap();
    let case: &TestCase = &suite.cases[0];

    assert_eq!(suite.policy, PathBuf::from("docs/examples/minimal.voom"));
    assert_eq!(case.name, "minimal");
    assert_eq!(case.fixture, PathBuf::from("fixtures/movie.json"));
    assert_eq!(case.expect.phases_run, Some(vec!["init".to_string()]));
    assert_eq!(
        case.expect.phases_skipped.as_ref().unwrap().get("verify"),
        Some(&"dependency".to_string())
    );
    assert_eq!(case.expect.video_codec, Some("hevc".to_string()));
    assert!(case.expect.no_warnings.unwrap());
}

#[test]
fn default_capabilities_include_standard_encoders_and_formats() {
    let caps = all_capabilities();

    assert!(caps.has_encoder("libx264"));
    assert!(caps.has_encoder("aac"));
    assert!(caps.has_encoder("libopus"));
    assert!(caps.has_format("matroska"));
    assert!(caps.has_format("mp4"));
}

#[test]
fn phases_run_assertion_passes_and_fails() {
    let plans = vec![plan("init", vec![]), plan("normalize", vec![])];

    assert!(assert_phases_run(&plans, &["init".to_string()]).is_ok());
    let failure = assert_phases_run(&plans, &["missing".to_string()]).unwrap_err();
    assert!(failure.to_string().contains("missing"));
}

#[test]
fn phases_skipped_assertion_passes_and_fails() {
    let mut skipped = plan("verify", vec![]);
    skipped.skip_reason = Some("dependency 'normalize' not yet executed".to_string());
    let plans = vec![skipped];
    let mut expected = HashMap::new();
    expected.insert("verify".to_string(), "dependency".to_string());

    assert!(assert_phases_skipped(&plans, &expected).is_ok());
    expected.insert("verify".to_string(), "run_if".to_string());
    let failure = assert_phases_skipped(&plans, &expected).unwrap_err();
    assert!(failure.to_string().contains("run_if"));
}

#[test]
fn audio_tracks_kept_assertion_passes_and_fails() {
    let plans = vec![plan(
        "normalize",
        vec![remove_track(2, TrackType::AudioCommentary)],
    )];

    assert!(assert_audio_tracks_kept(&plans, 1).is_ok());
    let failure = assert_audio_tracks_kept(&plans, 2).unwrap_err();
    assert!(failure.to_string().contains("expected 2"));
}

#[test]
fn synthesized_audio_assertion_passes_and_fails() {
    let plans = vec![plan(
        "synth",
        vec![PlannedAction::file_op(
            OperationType::SynthesizeAudio,
            ActionParams::Synthesize {
                name: "commentary".to_string(),
                language: Some("eng".to_string()),
                codec: Some("aac".to_string()),
                text: None,
                bitrate: None,
                channels: None,
                title: None,
                position: None,
                source_track: None,
            },
            "synthesize",
        )],
    )];

    assert!(assert_audio_tracks_synthesized(&plans, 1).is_ok());
    let failure = assert_audio_tracks_synthesized(&plans, 0).unwrap_err();
    assert!(failure.to_string().contains("expected 0"));
}

#[test]
fn subtitle_tracks_kept_assertion_passes_and_fails() {
    let plans = vec![plan(
        "normalize",
        vec![remove_track(3, TrackType::SubtitleMain)],
    )];

    assert!(assert_subtitle_tracks_kept(&plans, 0).is_ok());
    let failure = assert_subtitle_tracks_kept(&plans, 1).unwrap_err();
    assert!(failure.to_string().contains("expected 1"));
}

#[test]
fn video_codec_assertion_passes_and_fails() {
    let plans = vec![plan(
        "normalize",
        vec![PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".to_string(),
                settings: Default::default(),
            },
            "transcode video",
        )],
    )];

    assert!(assert_video_codec(&plans, "hevc").is_ok());
    let failure = assert_video_codec(&plans, "av1").unwrap_err();
    assert!(failure.to_string().contains("av1"));
}

#[test]
fn no_warnings_assertion_passes_and_fails() {
    let clean = vec![plan("init", vec![])];
    let noisy = vec![plan("init", vec![]).with_warning("missing language")];

    assert!(assert_no_warnings(&clean).is_ok());
    let failure = assert_no_warnings(&noisy).unwrap_err();
    assert!(failure.to_string().contains("missing language"));
}

#[test]
fn assertions_run_individual_checks() {
    let plans = vec![plan("init", vec![])];
    let assertions = Assertions {
        phases_run: Some(vec!["init".to_string()]),
        no_warnings: Some(true),
        ..Default::default()
    };

    assert!(assertions.check(&plans).is_ok());
}

#[test]
fn end_to_end_loads_fixture_compiles_minimal_policy_and_asserts_phase() {
    let dir = tempfile::tempdir().unwrap();
    let fixture_path = dir.path().join("movie.json");
    let fixture = Fixture {
        path: PathBuf::from("/media/movie.mp4"),
        container: Container::Mp4,
        duration: 120.0,
        size: 99,
        tracks: vec![track(0, TrackType::Video, "h264")],
        capabilities: None,
    };
    fs::write(&fixture_path, serde_json::to_string(&fixture).unwrap()).unwrap();

    let fixture = Fixture::load(&fixture_path).unwrap();
    let policy =
        voom_dsl::compile_policy(include_str!("../../../docs/examples/minimal.voom")).unwrap();
    let result = voom_policy_evaluator::evaluate_with_capabilities(
        &policy,
        &fixture.to_media_file(),
        &all_capabilities(),
    );

    assert!(assert_phases_run(&result.plans, &["containerize".to_string()]).is_ok());
}
