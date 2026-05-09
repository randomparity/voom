//! HandBrake executor plugin.
//!
//! Executes video transcoding operations using HandBrakeCLI via the host tool
//! runner. Similar to the native ffmpeg-executor plugin but uses HandBrake's
//! preset-based approach for encoding.
//!
//! # Host functions used
//!
//! - `run-tool` — execute HandBrakeCLI
//! - `get-plugin-data` / `set-plugin-data` — store preset configurations
//! - `log` — structured logging
//!
//! # External tools required
//!
//! - `HandBrakeCLI` — HandBrake command-line interface
//!
//! # Manifest
//!
//! ```toml
//! name = "handbrake-executor"
//! version = "0.1.0"
//! description = "Video transcoding via HandBrakeCLI"
//! handles_events = ["plan.created"]
//!
//! [[capabilities]]
//! [capabilities.Execute]
//! operations = ["transcode_video", "transcode_audio"]
//! formats = ["mkv", "mp4"]
//! ```

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{
    deserialize_event, load_plugin_config, serialize_event, ActionParams, Capability, Event,
    HostFunctions, OnEventResult, OperationType, PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "handbrake-executor",
        "0.1.0",
        vec![Capability::Execute {
            operations: vec![
                OperationType::TranscodeVideo,
                OperationType::TranscodeAudio,
            ],
            formats: vec!["mkv".to_string(), "mp4".to_string()],
        }],
    )
    .with_description("Video transcoding via HandBrakeCLI")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == Event::PLAN_CREATED
}

/// Process a plan.created event, executing transcode actions via HandBrakeCLI.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != Event::PLAN_CREATED {
        return None;
    }

    let event = deserialize_event(payload).map_err(|e| {
        host.log("error", &format!("failed to deserialize event: {e}"));
    }).ok()?;
    let plan = match &event {
        Event::PlanCreated(e) => &e.plan,
        _ => return None,
    };

    // Find transcode actions we can handle.
    let transcode_actions: Vec<_> = plan
        .actions
        .iter()
        .filter(|a| {
            matches!(
                a.operation,
                OperationType::TranscodeVideo | OperationType::TranscodeAudio
            )
        })
        .collect();

    if transcode_actions.is_empty() {
        return None;
    }

    let config: Option<HandbrakeConfig> = match load_plugin_config(|key| host.get_plugin_data(key)) {
        Ok(config) => config,
        Err(e) => {
            host.log("error", &format!("failed to load HandBrake config: {e}"));
            return None;
        }
    };
    let handbrake_bin = config
        .as_ref()
        .map(|c| c.handbrake_binary.as_str())
        .unwrap_or("HandBrakeCLI");

    let input_path = plan.file.path.to_string_lossy().to_string();
    let output_ext = plan
        .actions
        .iter()
        .find_map(|a| {
            if a.operation == OperationType::ConvertContainer {
                if let ActionParams::Container { container } = &a.parameters {
                    Some(container.as_str())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or("mkv");
    let output_path = format!(
        "{}.handbrake.{output_ext}",
        input_path.rsplit_once('.').map(|(base, _)| base).unwrap_or(&input_path)
    );

    host.log("info", &format!(
        "transcoding {} via HandBrake ({} action(s))",
        plan.file.path.display(),
        transcode_actions.len()
    ));

    // Build HandBrakeCLI arguments.
    let args = build_handbrake_args(
        &input_path,
        &output_path,
        &transcode_actions,
        &config,
    );

    let result = host.run_tool(
        handbrake_bin,
        &args,
        config.as_ref().map(|c| c.timeout_ms).unwrap_or(3_600_000), // 1 hour default
    );

    match result {
        Ok(output) if output.exit_code == 0 => {
            host.log("info", &format!("HandBrake transcode complete: {output_path}"));

            let completed_event = Event::PlanCompleted(
                voom_plugin_sdk::domain::PlanCompletedEvent::new(
                    plan.id,
                    plan.file.path.clone(),
                    plan.phase_name.clone(),
                    transcode_actions.len(),
                    false,
                ),
            );
            let produced_payload = serialize_event(&completed_event).map_err(|e| {
                host.log("error", &format!("failed to serialize event: {e}"));
            }).ok()?;

            let data = serde_json::json!({
                "plugin": "handbrake-executor",
                "output_path": output_path,
                "actions_executed": transcode_actions.len(),
            });

            Some(OnEventResult::new(
                "handbrake-executor",
                vec![(
                    completed_event.event_type().to_string(),
                    produced_payload,
                )],
                Some(serde_json::to_vec(&data).map_err(|e| {
                    host.log("error", &format!("failed to serialize result data: {e}"));
                }).ok()?),
            ))
        }
        Ok(output) => {
            host.log("error", &format!(
                "HandBrake exited with code {}: {}",
                output.exit_code,
                String::from_utf8_lossy(&output.stderr)
            ));
            None
        }
        Err(e) => {
            host.log("error", &format!("HandBrake execution failed: {e}"));
            None
        }
    }
}

/// Build HandBrakeCLI command-line arguments from transcode actions.
fn build_handbrake_args(
    input: &str,
    output: &str,
    actions: &[&voom_plugin_sdk::domain::PlannedAction],
    config: &Option<HandbrakeConfig>,
) -> Vec<String> {
    let mut args = vec![
        "-i".to_string(),
        input.to_string(),
        "-o".to_string(),
        output.to_string(),
    ];

    // Use preset if configured.
    if let Some(preset) = config.as_ref().and_then(|c| c.preset.as_deref()) {
        args.push("--preset".to_string());
        args.push(preset.to_string());
    }

    for action in actions {
        match action.operation {
            OperationType::TranscodeVideo => {
                apply_video_args(&mut args, action, config);
            }
            OperationType::TranscodeAudio => {
                apply_audio_args(&mut args, action);
            }
            _ => {}
        }
    }

    args
}

/// Append video encoder arguments from a `TranscodeVideo` action.
fn apply_video_args(
    args: &mut Vec<String>,
    action: &voom_plugin_sdk::domain::PlannedAction,
    config: &Option<HandbrakeConfig>,
) {
    let ActionParams::Transcode { codec, settings } = &action.parameters else {
        return;
    };
    args.push("--encoder".to_string());
    args.push(codec.clone());
    if let Some(q) = settings.crf {
        args.push("--quality".to_string());
        args.push(q.to_string());
    }
    if let Some(ref p) = settings.preset {
        // Only push preset from action params if not already set by config
        if config.as_ref().and_then(|c| c.preset.as_deref()).is_none() {
            args.push("--preset".to_string());
            args.push(p.clone());
        }
    }
    if let Some(ref b) = settings.bitrate {
        args.push("--vb".to_string());
        args.push(b.clone());
    }
}

/// Append audio encoder arguments from a `TranscodeAudio` action.
fn apply_audio_args(
    args: &mut Vec<String>,
    action: &voom_plugin_sdk::domain::PlannedAction,
) {
    let ActionParams::Transcode { codec, settings } = &action.parameters else {
        return;
    };
    args.push("--aencoder".to_string());
    args.push(codec.clone());
    if let Some(ref b) = settings.bitrate {
        args.push("--ab".to_string());
        args.push(b.clone());
    }
    if let Some(ref ch) = settings.channels {
        let mixdown = match ch.to_count() {
            Some(1) => "mono",
            Some(6) => "5point1",
            Some(8) => "7point1",
            _ => "stereo",
        };
        args.push("--mixdown".to_string());
        args.push(mixdown.to_string());
    }
}

// --- Config ---

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HandbrakeConfig {
    /// Path or name of the HandBrakeCLI binary.
    pub handbrake_binary: String,
    /// Default preset name (e.g., "Fast 1080p30").
    pub preset: Option<String>,
    /// Timeout in milliseconds for the transcode operation.
    pub timeout_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_plugin_sdk::domain::{MediaFile, Plan, PlannedAction};
    use voom_plugin_sdk::*;

    struct MockHost {
        config: Option<HandbrakeConfig>,
        exit_code: i32,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                config: None,
                exit_code: 0,
            }
        }

        fn with_failure() -> Self {
            Self {
                config: None,
                exit_code: 1,
            }
        }

    }

    impl HostFunctions for MockHost {
        fn run_tool(&self, _tool: &str, _args: &[String], _timeout_ms: u64) -> Result<ToolOutput, String> {
            Ok(ToolOutput::new(
                self.exit_code,
                b"encoded 100%".to_vec(),
                if self.exit_code != 0 {
                    b"encoding error".to_vec()
                } else {
                    vec![]
                },
            ))
        }

        fn get_plugin_data(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
            let data = if key == "config" {
                self.config.as_ref().map(|c| serde_json::to_vec(c).unwrap())
            } else {
                None
            };
            Ok(data)
        }

        fn set_plugin_data(&self, _key: &str, _value: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn log(&self, _level: &str, _message: &str) {}
    }

    fn make_transcode_plan() -> Plan {
        Plan::new(
            MediaFile::new(PathBuf::from("/media/movies/test.mkv")),
            "compress",
            "transcode",
        )
        .with_action(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "x265".to_string(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(22)),
            },
            "Transcode video to HEVC CRF 22",
        ))
        .with_action(PlannedAction::track_op(
            OperationType::TranscodeAudio,
            1,
            ActionParams::Transcode {
                codec: "opus".to_string(),
                settings: TranscodeSettings::default()
                    .with_bitrate(Some("128k".into())),
            },
            "Transcode audio to Opus 128k",
        ))
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "handbrake-executor");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "execute");
    }

    #[test]
    fn test_handles() {
        assert!(handles("plan.created"));
        assert!(!handles("file.introspected"));
    }

    #[test]
    fn test_on_event_transcode_success() {
        let host = MockHost::new();
        let plan = make_transcode_plan();
        let event = Event::PlanCreated(
            voom_plugin_sdk::domain::PlanCreatedEvent::new(plan),
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "handbrake-executor");
        assert_eq!(result.produced_events.len(), 1);

        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        assert_eq!(produced.event_type(), "plan.completed");

        let data: serde_json::Value = serde_json::from_slice(&result.data.unwrap()).unwrap();
        assert_eq!(data["actions_executed"], 2);
        assert!(data["output_path"].as_str().unwrap().ends_with(".mkv"));
    }

    #[test]
    fn test_on_event_transcode_failure() {
        let host = MockHost::with_failure();
        let plan = make_transcode_plan();
        let event = Event::PlanCreated(
            voom_plugin_sdk::domain::PlanCreatedEvent::new(plan),
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_no_transcode_actions() {
        let host = MockHost::new();
        let plan = Plan::new(
            MediaFile::new(PathBuf::from("/media/test.mkv")),
            "normalize",
            "metadata",
        )
        .with_action(PlannedAction::track_op(
            OperationType::SetDefault,
            0,
            ActionParams::Empty,
            "set default",
        ));
        let event = Event::PlanCreated(
            voom_plugin_sdk::domain::PlanCreatedEvent::new(plan),
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_wrong_type() {
        let host = MockHost::new();
        let result = on_event("file.discovered", &[], &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_args_basic() {
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "x265".to_string(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(20)),
            },
            "transcode",
        );
        let args = build_handbrake_args("/input.mkv", "/output.mkv", &[&action], &None);
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"/input.mkv".to_string()));
        assert!(args.contains(&"--encoder".to_string()));
        assert!(args.contains(&"x265".to_string()));
        assert!(args.contains(&"--quality".to_string()));
    }

    #[test]
    fn test_build_args_with_preset() {
        let config = Some(HandbrakeConfig {
            handbrake_binary: "HandBrakeCLI".to_string(),
            preset: Some("Fast 1080p30".to_string()),
            timeout_ms: 3_600_000,
        });
        let args = build_handbrake_args("/input.mkv", "/output.mkv", &[], &config);
        assert!(args.contains(&"--preset".to_string()));
        assert!(args.contains(&"Fast 1080p30".to_string()));
    }

    #[test]
    fn test_build_args_audio_transcode() {
        let action = PlannedAction::track_op(
            OperationType::TranscodeAudio,
            1,
            ActionParams::Transcode {
                codec: "opus".to_string(),
                settings: TranscodeSettings::default()
                    .with_bitrate(Some("128k".into()))
                    .with_channels(Some(TranscodeChannels::Count(2))),
            },
            "transcode audio",
        );
        let args = build_handbrake_args("/input.mkv", "/output.mkv", &[&action], &None);
        assert!(args.contains(&"--aencoder".to_string()));
        assert!(args.contains(&"opus".to_string()));
        assert!(args.contains(&"--ab".to_string()));
        assert!(args.contains(&"128k".to_string()));
        assert!(args.contains(&"--mixdown".to_string()));
        assert!(args.contains(&"stereo".to_string()));
    }

    #[test]
    fn test_handbrake_config_serde() {
        let config = HandbrakeConfig {
            handbrake_binary: "HandBrakeCLI".to_string(),
            preset: Some("H.265 MKV 1080p30".to_string()),
            timeout_ms: 7_200_000,
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let restored: HandbrakeConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.preset, Some("H.265 MKV 1080p30".to_string()));
        assert_eq!(restored.timeout_ms, 7_200_000);
    }
}
