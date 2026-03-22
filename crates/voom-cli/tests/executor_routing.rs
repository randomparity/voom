//! Executor routing integration tests.
//!
//! Verifies that `PlanCreated` events are routed to the correct executor
//! based on the event bus priority ordering and each executor's `can_handle`
//! logic. Both mkvtoolnix-executor and ffmpeg-executor are registered with
//! their production priorities.

use std::path::PathBuf;
use std::sync::Arc;

use voom_domain::events::{Event, PlanCreatedEvent};
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};
use voom_kernel::Kernel;

fn make_kernel_with_both_executors() -> Kernel {
    let mut kernel = Kernel::new();
    // Register mkvtoolnix at priority 39 (same as production)
    kernel.register_plugin(
        Arc::new(voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new()),
        39,
    );
    // Register ffmpeg at priority 40 (same as production)
    kernel.register_plugin(
        Arc::new(voom_ffmpeg_executor::FfmpegExecutorPlugin::new()),
        40,
    );
    kernel
}

fn mkv_file() -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from("/media/movie.mkv"));
    file.container = Container::Mkv;
    file.tracks = vec![
        Track::new(0, TrackType::Video, "hevc".into()),
        Track::new(1, TrackType::AudioMain, "aac".into()),
    ];
    file
}

fn mp4_file() -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from("/media/movie.mp4"));
    file.container = Container::Mp4;
    file.tracks = vec![
        Track::new(0, TrackType::Video, "h264".into()),
        Track::new(1, TrackType::AudioMain, "aac".into()),
    ];
    file
}

fn make_plan(file: MediaFile, actions: Vec<PlannedAction>) -> Plan {
    Plan {
        id: uuid::Uuid::new_v4(),
        file,
        policy_name: "test".into(),
        phase_name: "process".into(),
        actions,
        warnings: vec![],
        skip_reason: None,
        policy_hash: None,
        evaluated_at: chrono::Utc::now(),
    }
}

/// Transcode plans should be routed to ffmpeg-executor (mkvtoolnix cannot transcode).
#[test]
fn test_transcode_routes_to_ffmpeg() {
    let kernel = make_kernel_with_both_executors();

    let plan = make_plan(
        mp4_file(),
        vec![PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: ActionParams::Transcode {
                codec: "hevc".into(),
                crf: Some(23),
                preset: None,
                bitrate: None,
                channels: None,
            },
            description: "Transcode to HEVC".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    // Should be claimed by ffmpeg-executor (file doesn't exist so execution
    // fails, but the claim itself tells us routing worked)
    assert_eq!(results.len(), 1);
    assert!(results[0].claimed);
    assert_eq!(results[0].plugin_name, "ffmpeg-executor");
}

/// MKV metadata-only plans should route to mkvtoolnix-executor.
#[test]
fn test_mkv_metadata_routes_to_mkvtoolnix() {
    let kernel = make_kernel_with_both_executors();

    let plan = make_plan(
        mkv_file(),
        vec![PlannedAction {
            operation: OperationType::SetDefault,
            track_index: Some(1),
            parameters: ActionParams::Empty,
            description: "Set default audio".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    assert_eq!(results.len(), 1);
    assert!(results[0].claimed);
    assert_eq!(results[0].plugin_name, "mkvtoolnix-executor");
}

/// Non-MKV metadata plans should route to ffmpeg-executor.
#[test]
fn test_non_mkv_metadata_routes_to_ffmpeg() {
    let kernel = make_kernel_with_both_executors();

    let plan = make_plan(
        mp4_file(),
        vec![PlannedAction {
            operation: OperationType::SetDefault,
            track_index: Some(1),
            parameters: ActionParams::Empty,
            description: "Set default audio".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    assert_eq!(results.len(), 1);
    assert!(results[0].claimed);
    assert_eq!(results[0].plugin_name, "ffmpeg-executor");
}

/// ConvertContainer to MKV should route to mkvtoolnix (higher priority, can handle).
#[test]
fn test_convert_to_mkv_routes_to_mkvtoolnix() {
    let kernel = make_kernel_with_both_executors();

    let plan = make_plan(
        mp4_file(),
        vec![PlannedAction {
            operation: OperationType::ConvertContainer,
            track_index: None,
            parameters: ActionParams::Container {
                container: Container::Mkv,
            },
            description: "Convert to MKV".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    // mkvtoolnix handles convert-to-MKV at priority 39 (before ffmpeg at 40)
    assert_eq!(results.len(), 1);
    assert!(results[0].claimed);
    assert_eq!(results[0].plugin_name, "mkvtoolnix-executor");
}

/// MKV transcode plans route to ffmpeg, not mkvtoolnix (mkvtoolnix can't transcode).
#[test]
fn test_mkv_transcode_routes_to_ffmpeg() {
    let kernel = make_kernel_with_both_executors();

    let plan = make_plan(
        mkv_file(),
        vec![PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: ActionParams::Transcode {
                codec: "h264".into(),
                crf: None,
                preset: None,
                bitrate: None,
                channels: None,
            },
            description: "Transcode MKV to H.264".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    assert_eq!(results.len(), 1);
    assert!(results[0].claimed);
    assert_eq!(results[0].plugin_name, "ffmpeg-executor");
}

/// Empty plans are not claimed by either executor.
#[test]
fn test_empty_plan_not_claimed() {
    let kernel = make_kernel_with_both_executors();

    let plan = make_plan(mkv_file(), vec![]);

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    // Neither executor claims an empty plan
    assert!(
        results.is_empty() || results.iter().all(|r| !r.claimed),
        "empty plan should not be claimed"
    );
}

/// Skipped plans are not claimed by either executor.
#[test]
fn test_skipped_plan_not_claimed() {
    let kernel = make_kernel_with_both_executors();

    let mut plan = make_plan(
        mkv_file(),
        vec![PlannedAction {
            operation: OperationType::SetDefault,
            track_index: Some(1),
            parameters: ActionParams::Empty,
            description: "Set default".into(),
        }],
    );
    plan.skip_reason = Some("Already correct".into());

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    assert!(
        results.is_empty() || results.iter().all(|r| !r.claimed),
        "skipped plan should not be claimed"
    );
}
