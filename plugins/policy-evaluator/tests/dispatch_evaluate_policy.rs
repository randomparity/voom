//! End-to-end parity test: dispatching `Call::EvaluatePolicy` through a real
//! `Kernel` must produce the same `Plan`s as calling the free function
//! `evaluate_with_capabilities` directly. Locks in the Phase 4 contract: the
//! plugin is an instrumentation wrapper around the existing logic, not a
//! divergent implementation.

use std::path::PathBuf;
use std::sync::Arc;

use voom_domain::call::{Call, CallResponse};
use voom_domain::capabilities::{Capability, CapabilityQuery};
use voom_domain::capability_map::CapabilityMap;
use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_kernel::{Kernel, PluginContext};
use voom_policy_evaluator::{PolicyEvaluatorPlugin, evaluate_with_capabilities};

/// Strip per-evaluation, non-deterministic metadata (`id`, `evaluated_at`)
/// from each plan's JSON form so two evaluations of the same `(policy, file)`
/// can be compared for plan-content equality. The kernel dispatch path and
/// the free-function path stamp their own UUIDs and timestamps; everything
/// else must be byte-identical.
///
/// Strips only the top-level fields on each `Plan`; deliberately does NOT
/// recurse, because nested types (e.g. `Plan.file: MediaFile`) carry their
/// own `id` fields that are meaningful and must be compared, not erased.
fn canonicalize_plans(plans_json: &serde_json::Value) -> serde_json::Value {
    let mut owned = plans_json.clone();
    if let Some(arr) = owned.as_array_mut() {
        for plan in arr {
            if let Some(obj) = plan.as_object_mut() {
                obj.remove("id");
                obj.remove("evaluated_at");
            }
        }
    }
    owned
}

fn fixture_file() -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from("/media/Movie.mkv"));
    file.container = Container::Mkv;
    let mut video = Track::new(0, TrackType::Video, "hevc".into());
    video.width = Some(1920);
    video.height = Some(1080);
    let mut audio_eng = Track::new(1, TrackType::AudioMain, "ac3".into());
    audio_eng.language = "eng".into();
    audio_eng.channels = Some(6);
    audio_eng.is_default = true;
    let mut audio_jpn = Track::new(2, TrackType::AudioAlternate, "aac".into());
    audio_jpn.language = "jpn".into();
    audio_jpn.channels = Some(2);
    let mut sub_eng = Track::new(3, TrackType::SubtitleMain, "srt".into());
    sub_eng.language = "eng".into();
    file.tracks = vec![video, audio_eng, audio_jpn, sub_eng];
    file
}

fn fixture_caps() -> CapabilityMap {
    let mut map = CapabilityMap::new();
    map.register(ExecutorCapabilitiesEvent::new(
        "ffmpeg-executor",
        CodecCapabilities::new(
            vec!["h264".into(), "hevc".into(), "aac".into()],
            vec!["libx264".into(), "libx265".into(), "aac".into()],
        ),
        vec!["matroska".into(), "mp4".into()],
        vec![],
    ));
    map
}

fn kernel_with_evaluator() -> Kernel {
    let mut kernel = Kernel::new();
    let ctx = PluginContext::new(serde_json::json!({}), std::env::temp_dir());
    // Priority mirrors app.rs::PRIORITY_POLICY_EVALUATOR — keep in sync.
    kernel
        .init_and_register(Arc::new(PolicyEvaluatorPlugin::for_bootstrap()), 36, &ctx)
        .expect("init_and_register policy-evaluator");
    kernel
}

#[test]
fn dispatch_produces_same_plans_as_direct_call_for_simple_policy() {
    let policy = voom_dsl::compile_policy(
        r#"policy "demo" {
            phase containerize { container mkv }
            phase normalize {
                depends_on: [containerize]
                keep audio where lang in [eng]
            }
        }"#,
    )
    .unwrap();
    let file = fixture_file();
    let caps = fixture_caps();

    let direct = evaluate_with_capabilities(&policy, &file, &caps);

    let kernel = kernel_with_evaluator();
    let call = Call::EvaluatePolicy {
        policy: Box::new(policy),
        file: Box::new(file),
        phase: None,
        phase_outputs: None,
        phase_outcomes: None,
        capabilities_override: Some(caps),
    };
    let response = kernel
        .dispatch_to_capability(
            CapabilityQuery::Exclusive {
                kind: Capability::EvaluatePolicy.kind().to_string(),
            },
            call,
        )
        .expect("dispatch should succeed");
    let CallResponse::EvaluatePolicy(via_dispatch) = response else {
        panic!("expected EvaluatePolicy response; got {response:?}");
    };

    let direct_json = canonicalize_plans(&serde_json::to_value(&direct.plans).unwrap());
    let dispatch_json = canonicalize_plans(&serde_json::to_value(&via_dispatch.plans).unwrap());
    assert_eq!(
        direct_json, dispatch_json,
        "free-function and dispatch paths must produce identical plans"
    );
}

#[test]
fn dispatch_produces_same_plans_as_direct_call_for_transcode_policy() {
    let policy = voom_dsl::compile_policy(
        r#"policy "demo" {
            phase tc {
                transcode video to h264 { crf: 20 }
            }
        }"#,
    )
    .unwrap();
    let file = fixture_file();
    let caps = fixture_caps();

    let direct = evaluate_with_capabilities(&policy, &file, &caps);

    let kernel = kernel_with_evaluator();
    let call = Call::EvaluatePolicy {
        policy: Box::new(policy),
        file: Box::new(file),
        phase: None,
        phase_outputs: None,
        phase_outcomes: None,
        capabilities_override: Some(caps),
    };
    let CallResponse::EvaluatePolicy(via_dispatch) = kernel
        .dispatch_to_capability(
            CapabilityQuery::Exclusive {
                kind: Capability::EvaluatePolicy.kind().to_string(),
            },
            call,
        )
        .unwrap()
    else {
        panic!("expected EvaluatePolicy response");
    };

    let direct_json = canonicalize_plans(&serde_json::to_value(&direct.plans).unwrap());
    let dispatch_json = canonicalize_plans(&serde_json::to_value(&via_dispatch.plans).unwrap());
    assert_eq!(direct_json, dispatch_json);
}

#[test]
fn dispatch_records_plugin_stats_row_for_each_call() {
    use std::sync::Mutex;
    use voom_domain::plugin_stats::PluginStatRecord;
    use voom_kernel::stats_sink::StatsSink;

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<PluginStatRecord>>);
    impl StatsSink for RecordingSink {
        fn record(&self, record: PluginStatRecord) {
            self.0.lock().unwrap().push(record);
        }
    }

    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let kernel = kernel_with_evaluator();
    kernel.set_stats_sink(sink.clone() as Arc<dyn StatsSink>);

    let policy =
        voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
    let file = fixture_file();
    let call = Call::EvaluatePolicy {
        policy: Box::new(policy),
        file: Box::new(file),
        phase: None,
        phase_outputs: None,
        phase_outcomes: None,
        capabilities_override: None,
    };

    kernel
        .dispatch_to_capability(
            CapabilityQuery::Exclusive {
                kind: Capability::EvaluatePolicy.kind().to_string(),
            },
            call,
        )
        .unwrap();

    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 1, "exactly one stats row per dispatch");
    assert_eq!(records[0].plugin_id, "policy-evaluator");
    assert_eq!(records[0].event_type, "call.evaluate_policy");
    assert!(
        matches!(
            records[0].outcome,
            voom_domain::plugin_stats::PluginInvocationOutcome::Ok
        ),
        "successful dispatch must record Ok outcome; got {:?}",
        records[0].outcome
    );
}
