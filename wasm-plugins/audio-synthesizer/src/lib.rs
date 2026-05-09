//! Audio synthesizer plugin.
//!
//! Synthesizes new audio tracks for media files using text-to-speech (TTS)
//! engines via the host tool runner. This is used when a policy's `synthesize`
//! block requests generating an audio track (e.g., an audio description track
//! or a dubbed audio track from a transcript).
//!
//! # Host functions used
//!
//! - `run-tool` — execute TTS engine (e.g., piper, espeak, coqui-tts)
//! - `get-plugin-data` / `set-plugin-data` — cache synthesis results
//! - `log` — structured logging
//!
//! # External tools required
//!
//! - `piper` (recommended) or `espeak-ng` — TTS engine
//! - `ffmpeg` — for audio encoding to the target codec
//!
//! # Manifest
//!
//! ```toml
//! name = "audio-synthesizer"
//! version = "0.1.0"
//! description = "Audio synthesis via TTS engines"
//! handles_events = ["plan.created"]
//!
//! [[capabilities]]
//! [capabilities.Synthesize]
//! ```

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{
    deserialize_event, load_plugin_config, ActionParams, Capability, Event, HostFunctions,
    OnEventResult, OperationType, PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "audio-synthesizer",
        "0.1.0",
        vec![Capability::Synthesize],
    )
    .with_description("Audio synthesis via TTS engines")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == Event::PLAN_CREATED
}

/// Process a plan.created event, looking for SynthesizeAudio actions.
/// For each synthesis action, run the TTS engine and produce the audio file.
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

    // Find synthesize-audio actions in this plan.
    let synth_actions: Vec<_> = plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::SynthesizeAudio)
        .collect();

    if synth_actions.is_empty() {
        return None;
    }

    let config: Option<SynthConfig> = match load_plugin_config(|key| host.get_plugin_data(key)) {
        Ok(config) => config,
        Err(e) => {
            host.log("error", &format!("failed to load synthesizer config: {e}"));
            return None;
        }
    };
    let cfg = config.as_ref();
    let tts_engine = cfg.map(|c| c.tts_engine.as_str()).unwrap_or("piper");
    let tts_model = cfg.map(|c| c.tts_model.as_str()).unwrap_or("en_US-lessac-medium");

    host.log("info", &format!(
        "synthesizing {} audio track(s) for {}",
        synth_actions.len(),
        plan.file.path.display()
    ));

    let mut results = Vec::new();
    for action in &synth_actions {
        // Extract synthesis parameters from the ActionParams::Synthesize variant.
        let (text, language, output_codec) = match &action.parameters {
            ActionParams::Synthesize { text, language, codec, .. } => (
                text.as_deref().unwrap_or(""),
                language.as_deref().unwrap_or("en"),
                codec.as_deref().unwrap_or("aac"),
            ),
            _ => ("", "en", "aac"),
        };

        if text.is_empty() {
            host.log("warn", "synthesis action has no text, skipping");
            continue;
        }

        let hash = simple_hash(text);
        let raw_path = format!("/tmp/voom-synth-{hash}.wav");
        let encoded_path = format!("/tmp/voom-synth-{hash}.{output_codec}");

        // Step 1: Run TTS to generate raw WAV audio.
        let tts_result = match tts_engine {
            "piper" => host.run_tool(
                "piper",
                &[
                    "--model".to_string(),
                    tts_model.to_string(),
                    "--output_file".to_string(),
                    raw_path.clone(),
                ],
                120_000,
            ),
            _ => host.run_tool(
                "espeak-ng",
                &[
                    "-v".to_string(),
                    language.to_string(),
                    "-w".to_string(),
                    raw_path.clone(),
                    text.to_string(),
                ],
                60_000,
            ),
        };

        let tts_output = match tts_result {
            Err(e) => {
                host.log("error", &format!("TTS failed: {e}"));
                continue;
            }
            Ok(o) if o.exit_code != 0 => {
                host.log("error", &format!("TTS exited with code {}", o.exit_code));
                continue;
            }
            Ok(o) => o,
        };
        let _ = tts_output; // consumed; we only need success confirmation

        // Step 2: Encode to target codec via ffmpeg.
        let encode_result = host.run_tool(
            "ffmpeg",
            &[
                "-i".to_string(),
                raw_path.clone(),
                "-c:a".to_string(),
                output_codec.to_string(),
                "-y".to_string(),
                encoded_path.clone(),
            ],
            120_000,
        );

        // Clean up raw WAV.
        let _ = host.run_tool("rm", &[raw_path], 5_000);

        match &encode_result {
            Err(e) => {
                host.log("error", &format!("ffmpeg encoding failed: {e}"));
                continue;
            }
            Ok(o) if o.exit_code != 0 => continue,
            Ok(_) => {}
        }

        results.push(serde_json::json!({
            "action": action.description,
            "output_path": encoded_path,
            "codec": output_codec,
            "language": language,
        }));
    }

    if results.is_empty() {
        return None;
    }

    let data = serde_json::json!({
        "plugin": "audio-synthesizer",
        "synthesized_tracks": results,
    });

    Some(OnEventResult::new(
        "audio-synthesizer",
        vec![],
        Some(serde_json::to_vec(&data).map_err(|e| {
            host.log("error", &format!("failed to serialize result data: {e}"));
        }).ok()?),
    ))
}

/// Simple string hash for generating deterministic temp file names.
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for b in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    hash
}

// --- Config ---

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SynthConfig {
    /// TTS engine binary name (default: "piper").
    pub tts_engine: String,
    /// TTS model/voice name.
    pub tts_model: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    struct MockHost {
        config: Option<SynthConfig>,
    }

    impl MockHost {
        fn new() -> Self {
            Self { config: None }
        }
    }

    impl HostFunctions for MockHost {
        fn run_tool(&self, _tool: &str, _args: &[String], _timeout_ms: u64) -> Result<ToolOutput, String> {
            Ok(ToolOutput::new(0, vec![], vec![]))
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

    fn make_synth_plan() -> Plan {
        Plan::new(
            MediaFile::new(PathBuf::from("/media/test.mkv")),
            "normalize",
            "synthesize",
        )
        .with_action(PlannedAction::file_op(
            OperationType::SynthesizeAudio,
            ActionParams::Synthesize {
                name: "audio-description".to_string(),
                text: Some("This is a synthesized audio track.".to_string()),
                language: Some("en".to_string()),
                codec: Some("aac".to_string()),
                bitrate: None,
                channels: None,
                title: None,
                position: None,
                source_track: None,
            },
            "Synthesize English audio description",
        ))
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "audio-synthesizer");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "synthesize");
    }

    #[test]
    fn test_handles() {
        assert!(handles("plan.created"));
        assert!(!handles("file.introspected"));
    }

    #[test]
    fn test_on_event_synthesis() {
        let host = MockHost::new();
        let plan = make_synth_plan();
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "audio-synthesizer");
        assert!(result.data.is_some());

        let data: serde_json::Value = serde_json::from_slice(&result.data.unwrap()).unwrap();
        assert_eq!(data["synthesized_tracks"].as_array().unwrap().len(), 1);
        assert_eq!(data["synthesized_tracks"][0]["language"], "en");
        assert_eq!(data["synthesized_tracks"][0]["codec"], "aac");
    }

    #[test]
    fn test_on_event_no_synth_actions() {
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
            "Set default track",
        ));
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_empty_text_skipped() {
        let host = MockHost::new();
        let plan = Plan::new(
            MediaFile::new(PathBuf::from("/media/test.mkv")),
            "normalize",
            "synthesize",
        )
        .with_action(PlannedAction::file_op(
            OperationType::SynthesizeAudio,
            ActionParams::Synthesize {
                name: "empty".to_string(),
                text: Some("".to_string()),
                language: Some("en".to_string()),
                codec: None,
                bitrate: None,
                channels: None,
                title: None,
                position: None,
                source_track: None,
            },
            "empty synth",
        ));
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("plan.created", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_wrong_type() {
        let host = MockHost::new();
        let result = on_event("file.introspected", &[], &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_simple_hash_deterministic() {
        assert_eq!(simple_hash("hello"), simple_hash("hello"));
        assert_ne!(simple_hash("hello"), simple_hash("world"));
    }

    #[test]
    fn test_synth_config_serde() {
        let config = SynthConfig {
            tts_engine: "piper".to_string(),
            tts_model: "en_US-lessac-medium".to_string(),
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let restored: SynthConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.tts_engine, "piper");
    }
}
