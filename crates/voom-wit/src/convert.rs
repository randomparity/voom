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
///
/// Validates that the declared `event_type` matches the actual
/// deserialized event variant to prevent mismatched payloads.
pub fn event_from_wasm(event_type: &str, payload: &[u8]) -> Result<Event> {
    let event: Event = rmp_serde::from_slice(payload)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize event: {e}")))?;

    let actual_type = event.event_type();
    if actual_type != event_type {
        return Err(VoomError::Wasm(format!(
            "event type mismatch: declared='{event_type}' actual='{actual_type}'"
        )));
    }

    Ok(event)
}

/// Serialized form of an `EventResult` for the WASM boundary.
pub struct WasmEventResult {
    pub plugin_name: String,
    pub produced_events: Vec<(String, Vec<u8>)>,
    pub data: Option<Vec<u8>>,
    pub claimed: bool,
    pub execution_error: Option<String>,
    pub execution_detail: Option<Vec<u8>>,
}

/// Convert a WASM event result back into a domain `EventResult`.
///
/// The `produced_events` are MessagePack-encoded event payloads,
/// and `data` is JSON-encoded optional data.
pub fn event_result_from_wasm(wasm_result: WasmEventResult) -> Result<EventResult> {
    let events = wasm_result
        .produced_events
        .into_iter()
        .map(|(evt_type, payload)| event_from_wasm(&evt_type, &payload))
        .collect::<Result<Vec<Event>>>()?;

    let json_data = wasm_result
        .data
        .map(|d| serde_json::from_slice(&d))
        .transpose()
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize JSON: {e}")))?;

    let mut result = EventResult::new(wasm_result.plugin_name);
    result.produced_events = events;
    result.data = json_data;
    result.claimed = wasm_result.claimed;
    result.execution_error = wasm_result.execution_error;
    result.execution_detail = wasm_result
        .execution_detail
        .map(|d| serde_json::from_slice(&d))
        .transpose()
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize execution detail: {e}")))?;
    Ok(result)
}

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

    Ok(WasmEventResult {
        plugin_name: result.plugin_name.clone(),
        produced_events: produced,
        data,
        claimed: result.claimed,
        execution_error: result.execution_error.clone(),
        execution_detail: result
            .execution_detail
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| VoomError::Wasm(format!("failed to serialize execution detail: {e}")))?,
    })
}

/// Convert a WIT capability string (e.g., "discover:file,smb") to a domain Capability.
///
/// # Errors
/// Returns an error when a known capability kind contains malformed operation
/// or verification-mode parameters.
pub fn capability_from_wit(cap_str: &str) -> Result<Option<Capability>> {
    let (kind, params) = cap_str.split_once(':').unwrap_or((cap_str, ""));

    match kind {
        "discover" => Ok(Some(Capability::Discover {
            schemes: split_comma_list(params),
        })),
        "introspect" => Ok(Some(Capability::Introspect {
            formats: split_comma_list(params),
        })),
        "evaluate" => Ok(Some(Capability::Evaluate)),
        "execute" => {
            // Format: "execute:op1+op2:fmt1,fmt2"
            let mut parts = params.splitn(2, ':');
            let ops_part = parts.next().unwrap_or("");
            let fmts_part = parts.next().unwrap_or("");
            let operations = parse_execute_operations(ops_part)?;
            Ok(Some(Capability::Execute {
                operations,
                formats: split_comma_list(fmts_part),
            }))
        }
        "store" => Ok(Some(Capability::Store {
            backend: params.to_string(),
        })),
        "detect_tools" => Ok(Some(Capability::DetectTools)),
        "manage_jobs" => Ok(Some(Capability::ManageJobs)),
        "serve_http" => Ok(Some(Capability::ServeHttp)),
        "plan" => Ok(Some(Capability::Plan)),
        "backup" => Ok(Some(Capability::Backup)),
        "enrich_metadata" | "enrich-metadata" => Ok(Some(Capability::EnrichMetadata {
            source: params.to_string(),
        })),
        "transcribe" => Ok(Some(Capability::Transcribe)),
        "synthesize" => Ok(Some(Capability::Synthesize)),
        "generate_subtitle" => Ok(Some(Capability::GenerateSubtitle)),
        "health_check" => Ok(Some(Capability::HealthCheck)),
        "verify" => {
            let modes = parse_verify_modes(params)?;
            Ok(Some(Capability::Verify { modes }))
        }
        _ => Ok(None),
    }
}

fn parse_execute_operations(params: &str) -> Result<Vec<voom_domain::OperationType>> {
    if params.is_empty() {
        return Ok(vec![]);
    }

    params
        .split('+')
        .map(|value| {
            let value = value.trim();
            voom_domain::plan::OperationType::parse(value).ok_or_else(|| {
                VoomError::Wasm(format!("unknown execute operation in capability: {value}"))
            })
        })
        .collect()
}

fn parse_verify_modes(params: &str) -> Result<Vec<voom_domain::verification::VerificationMode>> {
    if params.is_empty() {
        return Ok(vec![]);
    }

    params
        .split(',')
        .map(|value| {
            let value = value.trim();
            voom_domain::verification::VerificationMode::parse(value).ok_or_else(|| {
                VoomError::Wasm(format!("unknown verify mode in capability: {value}"))
            })
        })
        .collect()
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
                .map(voom_domain::OperationType::as_str)
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
        Capability::GenerateSubtitle => "generate_subtitle".to_string(),
        Capability::HealthCheck => "health_check".to_string(),
        Capability::Verify { modes } => {
            if modes.is_empty() {
                "verify".to_string()
            } else {
                let parts = modes
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                format!("verify:{parts}")
            }
        }
        other => other.kind().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::*;

    fn parse_capability(capability: &str) -> Capability {
        capability_from_wit(capability).unwrap().unwrap()
    }

    #[test]
    fn test_event_roundtrip_msgpack() {
        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/media/movies/test.mkv"),
            1_500_000_000,
            Some("abc123def456".into()),
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

        let wasm_result = event_result_to_wasm(&result).unwrap();
        assert_eq!(wasm_result.plugin_name, "test-plugin");
        assert_eq!(wasm_result.produced_events.len(), 1);
        assert!(wasm_result.data.is_some());

        let restored = event_result_from_wasm(wasm_result).unwrap();
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
        let restored = parse_capability(&s);
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_evaluate() {
        let cap = Capability::Evaluate;
        let s = capability_to_wit(&cap);
        assert_eq!(s, "evaluate");
        let restored = parse_capability(&s);
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
        let restored = parse_capability(&s);
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_enrich() {
        let cap = Capability::EnrichMetadata {
            source: "radarr".into(),
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "enrich_metadata:radarr");
        let restored = parse_capability(&s);
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_store() {
        let cap = Capability::Store {
            backend: "sqlite".into(),
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "store:sqlite");
        let restored = parse_capability(&s);
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_verify() {
        use voom_domain::verification::VerificationMode;
        let cap = Capability::Verify {
            modes: vec![VerificationMode::Quick, VerificationMode::Thorough],
        };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "verify:quick,thorough");
        let restored = parse_capability(&s);
        assert_eq!(restored, cap);
    }

    #[test]
    fn test_capability_roundtrip_verify_empty_modes() {
        let cap = Capability::Verify { modes: vec![] };
        let s = capability_to_wit(&cap);
        assert_eq!(s, "verify");
        let restored = parse_capability(&s);
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
            (Capability::HealthCheck, "health_check"),
            (Capability::Evaluate, "evaluate"),
        ] {
            let s = capability_to_wit(&cap);
            assert_eq!(s, expected_str);
            let restored = parse_capability(&s);
            assert_eq!(restored, cap);
        }
    }

    #[test]
    fn test_capability_from_wit_unknown() {
        assert!(capability_from_wit("unknown_cap").unwrap().is_none());
    }

    #[test]
    fn test_capability_from_wit_rejects_unknown_execute_operation() {
        let err = capability_from_wit("execute:transcode_video+bogus:mkv").unwrap_err();
        assert!(err.to_string().contains("unknown execute operation"));
    }

    #[test]
    fn test_capability_from_wit_rejects_unknown_verify_mode() {
        let err = capability_from_wit("verify:quick,deep").unwrap_err();
        assert!(err.to_string().contains("unknown verify mode"));
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
                Some("hash".into()),
            )),
            Event::JobStarted(JobStartedEvent::new(uuid::Uuid::new_v4(), "test job")),
            Event::JobProgress(JobProgressEvent::new(uuid::Uuid::new_v4(), 0.5)),
            Event::JobCompleted({
                let mut e = JobCompletedEvent::new(uuid::Uuid::new_v4(), true);
                e.message = Some("done".into());
                e
            }),
            Event::PlanCreated(PlanCreatedEvent::new(plan)),
            Event::ExecutorCapabilities(
                ExecutorCapabilitiesEvent::new("test", CodecCapabilities::empty(), vec![], vec![])
                    .with_parallel_limits(vec![ExecutorParallelLimit::new("hw:nvenc", 4)]),
            ),
        ];

        for event in &events {
            let (evt_type, payload) = event_to_wasm(event).unwrap();
            let restored = event_from_wasm(&evt_type, &payload).unwrap();
            assert_eq!(restored.event_type(), event.event_type());
        }
    }

    #[test]
    fn test_executor_capabilities_event_roundtrip() {
        let event = Event::ExecutorCapabilities(
            ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                CodecCapabilities::new(
                    vec!["h264".into(), "hevc".into(), "aac".into()],
                    vec!["libx264".into(), "libx265".into(), "aac".into()],
                ),
                vec!["matroska".into(), "mp4".into(), "avi".into()],
                vec!["videotoolbox".into(), "cuda".into()],
            )
            .with_parallel_limits(vec![ExecutorParallelLimit::new("hw:nvenc", 4)]),
        );

        let (event_type, payload) = event_to_wasm(&event).unwrap();
        assert_eq!(event_type, "executor.capabilities");
        assert!(!payload.is_empty());

        let restored = event_from_wasm(&event_type, &payload).unwrap();
        assert_eq!(restored.event_type(), "executor.capabilities");

        let Event::ExecutorCapabilities(restored) = restored else {
            panic!("expected executor capabilities");
        };
        assert_eq!(restored.parallel_limits.len(), 1);
        assert_eq!(restored.parallel_limits[0].resource, "hw:nvenc");
        assert_eq!(restored.parallel_limits[0].max_parallel, 4);
    }

    #[test]
    fn test_health_status_event_roundtrip() {
        let event = Event::HealthStatus(HealthStatusEvent::new(
            "data_dir_exists",
            true,
            Some("/data/voom".into()),
        ));

        let (event_type, payload) = event_to_wasm(&event).unwrap();
        assert_eq!(event_type, "health.status");
        assert!(!payload.is_empty());

        let restored = event_from_wasm(&event_type, &payload).unwrap();
        assert_eq!(restored.event_type(), "health.status");
    }

    #[test]
    fn test_job_enqueue_requested_event_roundtrip() {
        use voom_domain::job::JobType;

        let event = Event::JobEnqueueRequested(voom_domain::events::JobEnqueueRequestedEvent::new(
            JobType::Introspect,
            50,
            Some(serde_json::json!({"path": "/media/test.mkv"})),
            "ffprobe-introspector",
        ));

        let (event_type, payload) = event_to_wasm(&event).unwrap();
        assert_eq!(event_type, "job.enqueue_requested");
        assert!(!payload.is_empty());

        let restored = event_from_wasm(&event_type, &payload).unwrap();
        assert_eq!(restored.event_type(), "job.enqueue_requested");
    }

    #[test]
    fn test_event_result_from_wasm_empty() {
        let result = event_result_from_wasm(WasmEventResult {
            plugin_name: "empty-plugin".into(),
            produced_events: vec![],
            data: None,
            claimed: false,
            execution_error: None,
            execution_detail: None,
        })
        .unwrap();
        assert_eq!(result.plugin_name, "empty-plugin");
        assert!(result.produced_events.is_empty());
        assert!(result.data.is_none());
    }

    #[test]
    fn test_event_result_roundtrip_preserves_execution_lifecycle_fields() {
        let detail = voom_domain::plan::ExecutionDetail {
            command: "handbrake --input movie.mkv".to_string(),
            exit_code: Some(0),
            stderr_tail: String::new(),
            duration_ms: 25,
        };
        let mut result = EventResult::plan_succeeded("executor", None);
        result.execution_detail = Some(detail.clone());

        let wasm_result = event_result_to_wasm(&result).unwrap();
        let restored = event_result_from_wasm(wasm_result).unwrap();

        assert!(restored.claimed);
        assert_eq!(restored.execution_error, None);
        assert_eq!(restored.execution_detail.unwrap().command, detail.command);
    }

    #[test]
    fn test_event_from_wasm_type_mismatch_rejected() {
        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/test.mkv"),
            100,
            Some("hash".into()),
        ));

        let (_correct_type, payload) = event_to_wasm(&event).unwrap();

        let err = event_from_wasm("job.started", &payload).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("event type mismatch"),
            "expected mismatch error, got: {msg}"
        );
        assert!(msg.contains("declared='job.started'"));
        assert!(msg.contains("actual='file.discovered'"));
    }

    #[test]
    fn test_event_result_from_wasm_propagates_mismatch() {
        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/test.mkv"),
            100,
            Some("hash".into()),
        ));
        let (_correct_type, payload) = event_to_wasm(&event).unwrap();

        let err = event_result_from_wasm(WasmEventResult {
            plugin_name: "test-plugin".into(),
            produced_events: vec![("wrong.type".into(), payload)],
            data: None,
            claimed: false,
            execution_error: None,
            execution_detail: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("event type mismatch"));
    }

    #[test]
    fn test_event_result_from_wasm_rejects_invalid_messagepack_event() {
        let err = event_result_from_wasm(WasmEventResult {
            plugin_name: "test-plugin".into(),
            produced_events: vec![("file.discovered".into(), vec![0xff, 0xfe])],
            data: None,
            claimed: false,
            execution_error: None,
            execution_detail: None,
        })
        .unwrap_err();

        assert!(err.to_string().contains("failed to deserialize event"));
    }

    #[test]
    fn test_event_result_from_wasm_rejects_invalid_json_data() {
        let err = event_result_from_wasm(WasmEventResult {
            plugin_name: "test-plugin".into(),
            produced_events: vec![],
            data: Some(br#"{"unterminated":"#.to_vec()),
            claimed: false,
            execution_error: None,
            execution_detail: None,
        })
        .unwrap_err();

        assert!(err.to_string().contains("failed to deserialize JSON"));
    }

    #[test]
    fn test_event_result_from_wasm_rejects_invalid_execution_detail() {
        let err = event_result_from_wasm(WasmEventResult {
            plugin_name: "test-plugin".into(),
            produced_events: vec![],
            data: None,
            claimed: true,
            execution_error: None,
            execution_detail: Some(br#"{"command":5}"#.to_vec()),
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("failed to deserialize execution detail")
        );
    }
}
