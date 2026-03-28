//! Conversion functions between domain types and WASM boundary representations.
//!
//! At the WASM boundary, events are serialized as `MessagePack` bytes inside
//! `EventData { event_type: String, payload: Vec<u8> }` structs.

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult};

/// Serialize a domain Event into the WASM boundary format: (`event_type`, `payload_bytes`).
pub fn event_to_wasm(event: &Event) -> Result<(String, Vec<u8>)> {
    let event_type = event.event_type().to_string();
    let payload = rmp_serde::to_vec(event)
        .map_err(|e| VoomError::Wasm(format!("failed to serialize event: {e}")))?;
    Ok((event_type, payload))
}

/// Deserialize a domain Event from WASM boundary format.
pub fn event_from_wasm(event_type: &str, payload: &[u8]) -> Result<Event> {
    let _ = event_type; // event_type is encoded in the enum variant
    rmp_serde::from_slice(payload)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize event: {e}")))
}

/// Convert a WASM event result back into a domain `EventResult`.
///
/// The `produced_events` are MessagePack-encoded event payloads,
/// and `data` is JSON-encoded optional data.
pub fn event_result_from_wasm(
    plugin_name: String,
    produced_events: Vec<(String, Vec<u8>)>,
    data: Option<Vec<u8>>,
) -> Result<EventResult> {
    let events = produced_events
        .into_iter()
        .map(|(evt_type, payload)| event_from_wasm(&evt_type, &payload))
        .collect::<Result<Vec<Event>>>()?;

    let json_data = data
        .map(|d| serde_json::from_slice(&d))
        .transpose()
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize JSON: {e}")))?;

    let mut result = EventResult::new(plugin_name);
    result.produced_events = events;
    result.data = json_data;
    Ok(result)
}

/// Serialized form of an `EventResult` for the WASM boundary.
pub type WasmEventResult = (String, Vec<(String, Vec<u8>)>, Option<Vec<u8>>);

/// Serialize a domain `EventResult` into WASM boundary format.
pub fn event_result_to_wasm(result: &EventResult) -> Result<WasmEventResult> {
    let produced = result
        .produced_events
        .iter()
        .map(event_to_wasm)
        .collect::<Result<Vec<_>>>()?;

    let data = result
        .data
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|e| VoomError::Wasm(format!("failed to serialize JSON: {e}")))?;

    Ok((result.plugin_name.clone(), produced, data))
}

/// Convert a WIT capability string (e.g., "discover:file,smb") to a domain Capability.
#[must_use]
pub fn capability_from_wit(cap_str: &str) -> Option<Capability> {
    let (kind, params) = cap_str.split_once(':').unwrap_or((cap_str, ""));

    match kind {
        "discover" => Some(Capability::Discover {
            schemes: split_comma_list(params),
        }),
        "introspect" => Some(Capability::Introspect {
            formats: split_comma_list(params),
        }),
        "evaluate" => Some(Capability::Evaluate),
        "execute" => {
            // Format: "execute:op1+op2:fmt1,fmt2"
            let mut parts = params.splitn(2, ':');
            let ops_part = parts.next().unwrap_or("");
            let fmts_part = parts.next().unwrap_or("");
            let operations = if ops_part.is_empty() {
                vec![]
            } else {
                ops_part
                    .split('+')
                    .filter_map(|s| voom_domain::plan::OperationType::parse(s.trim()))
                    .collect()
            };
            Some(Capability::Execute {
                operations,
                formats: split_comma_list(fmts_part),
            })
        }
        "store" => Some(Capability::Store {
            backend: params.to_string(),
        }),
        "detect_tools" => Some(Capability::DetectTools),
        "manage_jobs" => Some(Capability::ManageJobs),
        "serve_http" => Some(Capability::ServeHttp),
        "plan" => Some(Capability::Plan),
        "backup" => Some(Capability::Backup),
        "enrich_metadata" | "enrich-metadata" => Some(Capability::EnrichMetadata {
            source: params.to_string(),
        }),
        "transcribe" => Some(Capability::Transcribe),
        "synthesize" => Some(Capability::Synthesize),
        _ => None,
    }
}

fn split_comma_list(s: &str) -> Vec<String> {
    if s.is_empty() {
        vec![]
    } else {
        s.split(',').map(|p| p.trim().to_string()).collect()
    }
}

/// Convert a domain Capability to a WIT capability string.
#[must_use]
pub fn capability_to_wit(cap: &Capability) -> String {
    match cap {
        Capability::Discover { schemes } => {
            if schemes.is_empty() {
                "discover".to_string()
            } else {
                format!("discover:{}", schemes.join(","))
            }
        }
        Capability::Introspect { formats } => {
            if formats.is_empty() {
                "introspect".to_string()
            } else {
                format!("introspect:{}", formats.join(","))
            }
        }
        Capability::Evaluate => "evaluate".to_string(),
        Capability::Execute {
            operations,
            formats,
        } => {
            let ops = operations
                .iter()
                .map(|op| op.as_str())
                .collect::<Vec<_>>()
                .join("+");
            let fmts = formats.join(",");
            if ops.is_empty() && fmts.is_empty() {
                "execute".to_string()
            } else if fmts.is_empty() {
                format!("execute:{ops}")
            } else {
                format!("execute:{ops}:{fmts}")
            }
        }
        Capability::Store { backend } => format!("store:{backend}"),
        Capability::DetectTools => "detect_tools".to_string(),
        Capability::ManageJobs => "manage_jobs".to_string(),
        Capability::ServeHttp => "serve_http".to_string(),
        Capability::Plan => "plan".to_string(),
        Capability::Backup => "backup".to_string(),
        Capability::EnrichMetadata { source } => format!("enrich_metadata:{source}"),
        Capability::Transcribe => "transcribe".to_string(),
        Capability::Synthesize => "synthesize".to_string(),
        _ => unreachable!("all Capability variants must be handled"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::*;

    #[test]
    fn test_event_roundtrip_msgpack() {
        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/media/movies/test.mkv"),
            1_500_000_000,
            "abc123def456".into(),
        ));

        let (event_type, payload) = event_to_wasm(&event).unwrap();
        assert_eq!(event_type, "file.discovered");
        assert!(!payload.is_empty());

        let restored = event_from_wasm(&event_type, &payload).unwrap();
        assert_eq!(restored.event_type(), "file.discovered");
    }

    #[test]
    fn test_event_result_roundtrip() {
        let mut result = EventResult::new("test-plugin");
        result.produced_events = vec![Event::ToolDetected(ToolDetectedEvent::new(
            "ffprobe",
            "6.1",
            PathBuf::from("/usr/bin/ffprobe"),
        ))];
        result.data = Some(serde_json::json!({"status": "ok"}));

        let (name, produced, data) = event_result_to_wasm(&result).unwrap();
        assert_eq!(name, "test-plugin");
        assert_eq!(produced.len(), 1);
        assert!(data.is_some());

        let restored = event_result_from_wasm(name, produced, data).unwrap();
        assert_eq!(restored.plugin_name, "test-plugin");
        assert_eq!(restored.produced_events.len(), 1);
        assert_eq!(restored.data.unwrap()["status"].as_str().unwrap(), "ok");
    }

    #[test]
    fn test_capability_roundtrip_discover() {
        let cap = Capability::Discover {
            schemes: vec!["file".into(), "smb".into()],
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "discover:file,smb");
        let restored = capability_from_wit(&s).unwrap();
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_evaluate() {
        let cap = Capability::Evaluate;
        let s = capability_to_wit(&cap);
        assert_eq!(s, "evaluate");
        let restored = capability_from_wit(&s).unwrap();
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_execute() {
        use voom_domain::plan::OperationType;
        let cap = Capability::Execute {
            operations: vec![
                OperationType::TranscodeVideo,
                OperationType::ConvertContainer,
            ],
            formats: vec!["mkv".into(), "mp4".into()],
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "execute:transcode_video+convert_container:mkv,mp4");
        let restored = capability_from_wit(&s).unwrap();
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_enrich() {
        let cap = Capability::EnrichMetadata {
            source: "radarr".into(),
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "enrich_metadata:radarr");
        let restored = capability_from_wit(&s).unwrap();
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_store() {
        let cap = Capability::Store {
            backend: "sqlite".into(),
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "store:sqlite");
        let restored = capability_from_wit(&s).unwrap();
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_paramless_variants() {
        for (cap, expected_str) in [
            (Capability::DetectTools, "detect_tools"),
            (Capability::ManageJobs, "manage_jobs"),
            (Capability::ServeHttp, "serve_http"),
            (Capability::Plan, "plan"),
            (Capability::Backup, "backup"),
            (Capability::Transcribe, "transcribe"),
            (Capability::Synthesize, "synthesize"),
            (Capability::Evaluate, "evaluate"),
        ] {
            let s = capability_to_wit(&cap);
            assert_eq!(s, expected_str);
            let restored = capability_from_wit(&s).unwrap();
            assert_eq!(restored, cap);
        }
    }

    #[test]
    fn test_capability_from_wit_unknown() {
        assert!(capability_from_wit("unknown_cap").is_none());
    }

    #[test]
    fn test_event_to_wasm_all_event_types() {
        use voom_domain::plan::{OperationType, Plan, PlannedAction};

        let mut plan = Plan::new(
            voom_domain::media::MediaFile::new(PathBuf::from("/test.mkv")),
            "test",
            "normalize",
        );
        plan.actions = vec![PlannedAction::track_op(
            OperationType::SetDefault,
            0,
            voom_domain::plan::ActionParams::Empty,
            "set default",
        )];
        let events = vec![
            Event::FileDiscovered(FileDiscoveredEvent::new(
                PathBuf::from("/test.mkv"),
                100,
                "hash".into(),
            )),
            Event::JobStarted(JobStartedEvent::new(uuid::Uuid::new_v4(), "test job")),
            Event::JobProgress(JobProgressEvent::new(uuid::Uuid::new_v4(), 0.5)),
            Event::JobCompleted({
                let mut e = JobCompletedEvent::new(uuid::Uuid::new_v4(), true);
                e.message = Some("done".into());
                e
            }),
            Event::PlanCreated(PlanCreatedEvent::new(plan)),
            Event::ExecutorCapabilities(ExecutorCapabilitiesEvent::new(
                "test",
                CodecCapabilities::empty(),
                vec![],
                vec![],
            )),
        ];

        for event in &events {
            let (evt_type, payload) = event_to_wasm(event).unwrap();
            let restored = event_from_wasm(&evt_type, &payload).unwrap();
            assert_eq!(restored.event_type(), event.event_type());
        }
    }

    #[test]
    fn test_executor_capabilities_event_roundtrip() {
        let event = Event::ExecutorCapabilities(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(
                vec!["h264".into(), "hevc".into(), "aac".into()],
                vec!["libx264".into(), "libx265".into(), "aac".into()],
            ),
            vec!["matroska".into(), "mp4".into(), "avi".into()],
            vec!["videotoolbox".into(), "cuda".into()],
        ));

        let (event_type, payload) = event_to_wasm(&event).unwrap();
        assert_eq!(event_type, "executor.capabilities");
        assert!(!payload.is_empty());

        let restored = event_from_wasm(&event_type, &payload).unwrap();
        assert_eq!(restored.event_type(), "executor.capabilities");
    }

    #[test]
    fn test_event_result_from_wasm_empty() {
        let result = event_result_from_wasm("empty-plugin".into(), vec![], None).unwrap();
        assert_eq!(result.plugin_name, "empty-plugin");
        assert!(result.produced_events.is_empty());
        assert!(result.data.is_none());
    }
}
