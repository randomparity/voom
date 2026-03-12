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
    deserialize_event, load_plugin_config, serialize_event, Event, OnEventResult, OperationType,
    PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData {
        name: "handbrake-executor".to_string(),
        version: "0.1.0".to_string(),
        capabilities: vec!["execute:transcode_video+transcode_audio:mkv,mp4".to_string()],
    }
}

pub fn handles(event_type: &str) -> bool {
    event_type == "plan.created"
}

/// Process a plan.created event, executing transcode actions via HandBrakeCLI.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != "plan.created" {
        return None;
    }

    let event = deserialize_event(payload).ok()?;
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

    let config = load_config(host);
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
                a.parameters.get("container").and_then(|v| v.as_str())
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
                voom_plugin_sdk::voom_domain::events::PlanCompletedEvent {
                    plan_id: plan.id,
                    path: plan.file.path.clone(),
                    phase_name: plan.phase_name.clone(),
                    actions_applied: transcode_actions.len(),
                },
            );
            let produced_payload = serialize_event(&completed_event).ok()?;

            let data = serde_json::json!({
                "plugin": "handbrake-executor",
                "output_path": output_path,
                "actions_executed": transcode_actions.len(),
            });

            Some(OnEventResult {
                plugin_name: "handbrake-executor".to_string(),
                produced_events: vec![(
                    completed_event.event_type().to_string(),
                    produced_payload,
                )],
                data: Some(serde_json::to_vec(&data).ok()?),
            })
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
    actions: &[&voom_plugin_sdk::PlannedAction],
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

    // Apply per-action parameters.
    for action in actions {
        let params = &action.parameters;

        match action.operation {
            OperationType::TranscodeVideo => {
                if let Some(encoder) = params.get("encoder").and_then(|v| v.as_str()) {
                    args.push("--encoder".to_string());
                    args.push(encoder.to_string());
                }
                if let Some(quality) = params.get("quality").and_then(|v| v.as_f64()) {
                    args.push("--quality".to_string());
                    args.push(quality.to_string());
                }
                if let Some(bitrate) = params.get("bitrate").and_then(|v| v.as_u64()) {
                    args.push("--vb".to_string());
                    args.push(bitrate.to_string());
                }
            }
            OperationType::TranscodeAudio => {
                if let Some(encoder) = params.get("encoder").and_then(|v| v.as_str()) {
                    args.push("--aencoder".to_string());
                    args.push(encoder.to_string());
                }
                if let Some(bitrate) = params.get("bitrate").and_then(|v| v.as_u64()) {
                    args.push("--ab".to_string());
                    args.push(bitrate.to_string());
                }
                if let Some(mixdown) = params.get("mixdown").and_then(|v| v.as_str()) {
                    args.push("--mixdown".to_string());
                    args.push(mixdown.to_string());
                }
            }
            _ => {}
        }
    }

    args
}

// --- Host function abstraction ---

pub trait HostFunctions {
    fn run_tool(&self, tool: &str, args: &[String], timeout_ms: u64) -> Result<ToolOutput, String>;
    fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>>;
    fn set_plugin_data(&self, key: &str, value: &[u8]) -> Result<(), String>;
    fn log(&self, level: &str, message: &str);
}

pub struct ToolOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

// --- Config ---

#[derive(Debug, Serialize, Deserialize)]
pub struct HandbrakeConfig {
    /// Path or name of the HandBrakeCLI binary.
    pub handbrake_binary: String,
    /// Default preset name (e.g., "Fast 1080p30").
    pub preset: Option<String>,
    /// Timeout in milliseconds for the transcode operation.
    pub timeout_ms: u64,
}

fn load_config(host: &dyn HostFunctions) -> Option<HandbrakeConfig> {
    load_plugin_config(|key| host.get_plugin_data(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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

        fn with_preset(preset: &str) -> Self {
            Self {
                config: Some(HandbrakeConfig {
                    handbrake_binary: "HandBrakeCLI".to_string(),
                    preset: Some(preset.to_string()),
                    timeout_ms: 1_800_000,
                }),
                exit_code: 0,
            }
        }
    }

    impl HostFunctions for MockHost {
        fn run_tool(&self, _tool: &str, _args: &[String], _timeout_ms: u64) -> Result<ToolOutput, String> {
            Ok(ToolOutput {
                exit_code: self.exit_code,
                stdout: b"encoded 100%".to_vec(),
                stderr: if self.exit_code != 0 {
                    b"encoding error".to_vec()
                } else {
                    vec![]
                },
            })
        }

        fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>> {
            if key == "config" {
                self.config.as_ref().map(|c| serde_json::to_vec(c).unwrap())
            } else {
                None
            }
        }

        fn set_plugin_data(&self, _key: &str, _value: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn log(&self, _level: &str, _message: &str) {}
    }

    fn make_transcode_plan() -> Plan {
        Plan {
            file: MediaFile::new(PathBuf::from("/media/movies/test.mkv")),
            policy_name: "compress".to_string(),
            phase_name: "transcode".to_string(),
            actions: vec![
                PlannedAction {
                    operation: OperationType::TranscodeVideo,
                    track_index: Some(0),
                    parameters: serde_json::json!({
                        "encoder": "x265",
                        "quality": 22.0,
                    }),
                    description: "Transcode video to HEVC CRF 22".to_string(),
                },
                PlannedAction {
                    operation: OperationType::TranscodeAudio,
                    track_index: Some(1),
                    parameters: serde_json::json!({
                        "encoder": "opus",
                        "bitrate": 128,
                    }),
                    description: "Transcode audio to Opus 128k".to_string(),
                },
            ],
            warnings: vec![],
            skip_reason: None,
            id: uuid::Uuid::new_v4(),
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "handbrake-executor");
        assert!(info.capabilities[0].starts_with("execute:"));
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
            voom_plugin_sdk::voom_domain::events::PlanCreatedEvent { plan },
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
            voom_plugin_sdk::voom_domain::events::PlanCreatedEvent { plan },
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_no_transcode_actions() {
        let host = MockHost::new();
        let plan = Plan {
            file: MediaFile::new(PathBuf::from("/media/test.mkv")),
            policy_name: "normalize".to_string(),
            phase_name: "metadata".to_string(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(0),
                parameters: serde_json::json!({}),
                description: "set default".to_string(),
            }],
            warnings: vec![],
            skip_reason: None,
            id: uuid::Uuid::new_v4(),
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        };
        let event = Event::PlanCreated(
            voom_plugin_sdk::voom_domain::events::PlanCreatedEvent { plan },
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
        let action = PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: serde_json::json!({"encoder": "x265", "quality": 20.0}),
            description: "transcode".to_string(),
        };
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
        let action = PlannedAction {
            operation: OperationType::TranscodeAudio,
            track_index: Some(1),
            parameters: serde_json::json!({"encoder": "opus", "bitrate": 128, "mixdown": "stereo"}),
            description: "transcode audio".to_string(),
        };
        let args = build_handbrake_args("/input.mkv", "/output.mkv", &[&action], &None);
        assert!(args.contains(&"--aencoder".to_string()));
        assert!(args.contains(&"opus".to_string()));
        assert!(args.contains(&"--ab".to_string()));
        assert!(args.contains(&"128".to_string()));
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
