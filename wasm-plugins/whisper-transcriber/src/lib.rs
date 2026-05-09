//! Whisper transcriber plugin.
//!
//! Transcribes audio tracks from media files using OpenAI's Whisper model
//! via the host tool runner. Listens for `file.introspected` events, extracts
//! audio via ffmpeg, runs whisper-cli for transcription, and produces a
//! `metadata.enriched` event with the transcript.
//!
//! # Host functions used
//!
//! - `run-tool` — execute ffmpeg (audio extraction) and whisper-cli (transcription)
//! - `get-plugin-data` / `set-plugin-data` — cache transcription results
//! - `log` — structured logging
//!
//! # External tools required
//!
//! - `ffmpeg` — for extracting audio from video files
//! - `whisper-cli` (or `whisper`) — Whisper inference binary
//!
//! # Manifest
//!
//! ```toml
//! name = "whisper-transcriber"
//! version = "0.1.0"
//! description = "Audio transcription via Whisper"
//! handles_events = ["file.introspected"]
//!
//! [[capabilities]]
//! [capabilities.Transcribe]
//! ```

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{
    deserialize_event, load_plugin_config, serialize_event, Capability, Event, HostFunctions,
    MediaFile, MetadataEnrichedEvent, OnEventResult, PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "whisper-transcriber",
        "0.1.0",
        vec![Capability::Transcribe],
    )
    .with_description("Audio transcription via Whisper")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == Event::FILE_INTROSPECTED
}

/// Process a file.introspected event by transcribing its primary audio track.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != Event::FILE_INTROSPECTED {
        return None;
    }

    let event = deserialize_event(payload)
        .map_err(|e| {
            host.log("error", &format!("failed to deserialize event: {e}"));
        })
        .ok()?;
    let file = match &event {
        Event::FileIntrospected(e) => &e.file,
        _ => return None,
    };

    // Only transcribe files that have audio tracks.
    let has_audio = file.tracks.iter().any(|t| t.track_type.is_audio());
    if !has_audio {
        host.log(
            "debug",
            &format!("skipping {}: no audio tracks", file.path.display()),
        );
        return None;
    }

    // Check cache first.
    let path_hash_owned = format!(
        "{:x}",
        xxhash_rust::xxh3::xxh3_64(file.path.to_string_lossy().as_bytes())
    );
    let hash_str = file
        .content_hash
        .as_deref()
        .unwrap_or(&path_hash_owned);
    let cache_key = format!("transcript:{hash_str}");
    match host.get_plugin_data(&cache_key) {
        Ok(Some(cached)) => {
            host.log("debug", "using cached transcript");
            return build_result(file, &cached, host);
        }
        Ok(None) => {}
        Err(e) => host.log("error", &format!("failed to read transcript cache: {e}")),
    }

    let config: Option<WhisperConfig> = match load_plugin_config(|key| host.get_plugin_data(key)) {
        Ok(config) => config,
        Err(e) => {
            host.log("error", &format!("failed to load Whisper config: {e}"));
            return None;
        }
    };

    // Step 1: Extract audio to a temp WAV file via ffmpeg.
    let file_path = file.path.to_string_lossy().to_string();
    let audio_path = format!("/tmp/voom-whisper-{hash_str}.wav");
    let extract_result = host.run_tool(
        "ffmpeg",
        &[
            "-i".to_string(),
            file_path.clone(),
            "-vn".to_string(),
            "-ac".to_string(),
            "1".to_string(),
            "-ar".to_string(),
            "16000".to_string(),
            "-f".to_string(),
            "wav".to_string(),
            "-y".to_string(),
            audio_path.clone(),
        ],
        300_000, // 5 minute timeout for extraction
    );

    let _extract_output = match extract_result {
        Err(e) => {
            host.log("error", &format!("ffmpeg audio extraction failed: {e}"));
            return None;
        }
        Ok(o) if o.exit_code != 0 => {
            host.log(
                "error",
                &format!(
                    "ffmpeg exited with code {}: {}",
                    o.exit_code,
                    String::from_utf8_lossy(&o.stderr)
                ),
            );
            return None;
        }
        Ok(o) => o,
    };

    // Step 2: Run whisper on the extracted audio.
    let cfg = config.as_ref();
    let whisper_bin = cfg
        .map(|c| c.whisper_binary.as_str())
        .unwrap_or("whisper-cli");
    let model = cfg.map(|c| c.model.as_str()).unwrap_or("base");
    let language = cfg.and_then(|c| c.language.as_deref());

    let per_segment = cfg.is_some_and(|c| c.per_segment_language);
    let mut whisper_args = vec![
        audio_path.clone(),
        "--model".to_string(),
        model.to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    if !per_segment {
        if let Some(lang) = language {
            whisper_args.push("--language".to_string());
            whisper_args.push(lang.to_string());
        }
    }

    let whisper_result = host.run_tool(whisper_bin, &whisper_args, 600_000); // 10 min timeout

    // Clean up temp audio file.
    let _ = host.run_tool("rm", &[audio_path], 5_000);

    let whisper_output = match whisper_result {
        Err(e) => {
            host.log("error", &format!("whisper failed: {e}"));
            return None;
        }
        Ok(o) if o.exit_code != 0 => {
            host.log(
                "error",
                &format!(
                    "whisper exited with code {}: {}",
                    o.exit_code,
                    String::from_utf8_lossy(&o.stderr)
                ),
            );
            return None;
        }
        Ok(o) => o,
    };

    // Cache the result.
    let _ = host.set_plugin_data(&cache_key, &whisper_output.stdout);

    build_result(file, &whisper_output.stdout, host)
}

/// Returns true when the transcript contains segments in more than one language.
pub fn detect_multi_language(transcript: &serde_json::Value) -> bool {
    let segments = match transcript.get("segments").and_then(|s| s.as_array()) {
        Some(arr) => arr,
        None => return false,
    };
    let mut languages = std::collections::HashSet::new();
    for seg in segments {
        if let Some(lang) = seg.get("language").and_then(|l| l.as_str()) {
            languages.insert(lang);
            if languages.len() > 1 {
                return true;
            }
        }
    }
    false
}

fn build_result(
    file: &MediaFile,
    transcript_data: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    let transcript_json: serde_json::Value = serde_json::from_slice(transcript_data)
        .map_err(|e| {
            host.log("error", &format!("failed to parse transcript JSON: {e}"));
        })
        .ok()?;

    let metadata = serde_json::json!({
        "source": "whisper-transcriber",
        "transcript": transcript_json,
        "multi_language_detected": detect_multi_language(&transcript_json),
    });

    let enriched_event = Event::MetadataEnriched(MetadataEnrichedEvent::new(
        file.path.clone(),
        "whisper-transcriber".to_string(),
        metadata,
    ));

    let produced_payload = serialize_event(&enriched_event)
        .map_err(|e| {
            host.log("error", &format!("failed to serialize event: {e}"));
        })
        .ok()?;

    Some(OnEventResult::new(
        "whisper-transcriber",
        vec![(enriched_event.event_type().to_string(), produced_payload)],
        None,
    ))
}

// --- Config ---

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WhisperConfig {
    /// Path or name of the whisper binary (default: "whisper-cli").
    pub whisper_binary: String,
    /// Model name (default: "base"). Options: tiny, base, small, medium, large.
    pub model: String,
    /// Force a specific language (None = auto-detect).
    pub language: Option<String>,
    /// When true, skip `--language` so whisper auto-detects per segment.
    #[serde(default)]
    pub per_segment_language: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    struct MockHost {
        config: Option<WhisperConfig>,
        tool_results: HashMap<String, ToolOutput>,
        cached: std::cell::RefCell<HashMap<String, Vec<u8>>>,
    }

    impl MockHost {
        fn new() -> Self {
            let transcript = serde_json::json!({
                "text": "Hello, this is a test transcript.",
                "language": "en",
                "segments": [
                    {"start": 0.0, "end": 2.5, "text": "Hello,"},
                    {"start": 2.5, "end": 5.0, "text": " this is a test transcript."},
                ],
            });

            let mut tool_results = HashMap::new();
            tool_results.insert(
                "ffmpeg".to_string(),
                ToolOutput::new(0, vec![], b"audio extracted".to_vec()),
            );
            tool_results.insert(
                "whisper-cli".to_string(),
                ToolOutput::new(0, serde_json::to_vec(&transcript).unwrap(), vec![]),
            );
            tool_results.insert("rm".to_string(), ToolOutput::new(0, vec![], vec![]));

            Self {
                config: None,
                tool_results,
                cached: std::cell::RefCell::new(HashMap::new()),
            }
        }

        fn with_per_segment_config() -> Self {
            let mut host = Self::new();
            host.config = Some(WhisperConfig {
                whisper_binary: "whisper-cli".to_string(),
                model: "large".to_string(),
                language: Some("en".to_string()),
                per_segment_language: true,
            });
            host
        }

        fn with_failing_ffmpeg() -> Self {
            let mut host = Self::new();
            host.tool_results.insert(
                "ffmpeg".to_string(),
                ToolOutput::new(1, vec![], b"error: no such file".to_vec()),
            );
            host
        }
    }

    impl HostFunctions for MockHost {
        fn run_tool(
            &self,
            tool: &str,
            _args: &[String],
            _timeout_ms: u64,
        ) -> Result<ToolOutput, String> {
            self.tool_results
                .get(tool)
                .map(|o| ToolOutput::new(o.exit_code, o.stdout.clone(), o.stderr.clone()))
                .ok_or_else(|| format!("tool not found: {tool}"))
        }

        fn get_plugin_data(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
            let data = if key == "config" {
                self.config.as_ref().map(|c| serde_json::to_vec(c).unwrap())
            } else {
                self.cached.borrow().get(key).cloned()
            };
            Ok(data)
        }

        fn set_plugin_data(&self, key: &str, value: &[u8]) -> Result<(), String> {
            self.cached
                .borrow_mut()
                .insert(key.to_string(), value.to_vec());
            Ok(())
        }

        fn log(&self, _level: &str, _message: &str) {}
    }

    fn make_audio_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/movies/test.mkv"));
        file.content_hash = Some("testhash123".to_string());
        let mut audio = Track::new(0, TrackType::AudioMain, "aac".into());
        audio.language = "eng".into();
        audio.is_default = true;
        audio.channels = Some(2);
        audio.channel_layout = Some("stereo".into());
        audio.sample_rate = Some(48000);
        file.tracks = vec![audio];
        file
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "whisper-transcriber");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "transcribe");
    }

    #[test]
    fn test_handles() {
        assert!(handles("file.introspected"));
        assert!(!handles("plan.created"));
    }

    #[test]
    fn test_on_event_transcription_success() {
        let host = MockHost::new();
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "whisper-transcriber");

        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                assert_eq!(e.source, "whisper-transcriber");
                assert_eq!(
                    e.metadata["transcript"]["text"],
                    "Hello, this is a test transcript."
                );
                assert_eq!(e.metadata["transcript"]["language"], "en");
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_on_event_no_audio_tracks() {
        let host = MockHost::new();
        let file = MediaFile::new(PathBuf::from("/media/test.mkv")); // no tracks
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_ffmpeg_failure() {
        let host = MockHost::with_failing_ffmpeg();
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_cached_transcript() {
        let host = MockHost::new();
        // Pre-populate the cache.
        let transcript = serde_json::json!({
            "text": "Cached transcript.",
            "language": "en",
        });
        host.cached.borrow_mut().insert(
            "transcript:testhash123".to_string(),
            serde_json::to_vec(&transcript).unwrap(),
        );

        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let produced: Event = deserialize_event(&result.unwrap().produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                assert_eq!(e.metadata["transcript"]["text"], "Cached transcript.");
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_on_event_wrong_type() {
        let host = MockHost::new();
        let result = on_event("plan.created", &[], &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_whisper_config_per_segment_language() {
        let config: WhisperConfig = serde_json::from_str(
            r#"{"whisper_binary":"whisper","model":"large","per_segment_language":true}"#,
        )
        .unwrap();
        assert!(config.per_segment_language);
    }

    #[test]
    fn test_whisper_config_per_segment_language_default_false() {
        let config: WhisperConfig =
            serde_json::from_str(r#"{"whisper_binary":"whisper","model":"base"}"#).unwrap();
        assert!(!config.per_segment_language);
    }

    #[test]
    fn test_detect_multi_language_true() {
        let transcript = serde_json::json!({
            "segments": [
                {"start": 0.0, "end": 2.0, "text": "Hello", "language": "en"},
                {"start": 2.0, "end": 4.0, "text": "Bonjour", "language": "fr"},
            ]
        });
        assert!(detect_multi_language(&transcript));
    }

    #[test]
    fn test_detect_multi_language_false_single() {
        let transcript = serde_json::json!({
            "segments": [
                {"start": 0.0, "end": 2.0, "text": "Hello", "language": "en"},
                {"start": 2.0, "end": 4.0, "text": "World", "language": "en"},
            ]
        });
        assert!(!detect_multi_language(&transcript));
    }

    #[test]
    fn test_detect_multi_language_missing_segments() {
        let transcript = serde_json::json!({"text": "Hello"});
        assert!(!detect_multi_language(&transcript));
    }

    #[test]
    fn test_on_event_includes_multi_language_detected() {
        let host = MockHost::new();
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();
        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let produced: Event = deserialize_event(&result.unwrap().produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                assert!(e.metadata.get("multi_language_detected").is_some());
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_per_segment_language_omits_language_flag() {
        // We can't directly inspect args passed to run_tool with the current mock,
        // but we can verify the config round-trips correctly and the function
        // succeeds with per_segment_language enabled
        let host = MockHost::with_per_segment_config();
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();
        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
    }

    #[test]
    fn test_whisper_config_serde() {
        let config = WhisperConfig {
            whisper_binary: "whisper".to_string(),
            model: "large".to_string(),
            language: Some("en".to_string()),
            per_segment_language: false,
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let restored: WhisperConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.model, "large");
        assert_eq!(restored.language, Some("en".to_string()));
    }
}
