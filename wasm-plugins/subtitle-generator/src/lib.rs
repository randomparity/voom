//! Subtitle generator plugin.
//!
//! Generates forced subtitle (SRT) files from multi-language transcripts
//! produced by the whisper-transcriber plugin. Listens for `metadata.enriched`
//! events, filters for foreign-language segments, and writes an SRT file
//! alongside the original media file.
//!
//! # Host functions used
//!
//! - `run-tool` -- write the generated SRT file to disk via shell
//! - `get-plugin-data` -- load plugin configuration
//! - `log` -- structured logging

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{
    deserialize_event, load_plugin_config, serialize_event, Event, HostFunctions, OnEventResult,
    PluginInfoData, SubtitleGeneratedEvent,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "subtitle-generator",
        "0.1.0",
        vec!["generate_subtitle".to_string()],
    )
    .with_description("Generate forced subtitle files from multi-language transcripts")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == "metadata.enriched"
}

/// Process a `metadata.enriched` event by extracting foreign-language
/// segments and writing a forced SRT file.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != "metadata.enriched" {
        return None;
    }

    let event = deserialize_event(payload)
        .map_err(|e| {
            host.log("error", &format!("failed to deserialize event: {e}"));
        })
        .ok()?;

    let enriched = match &event {
        Event::MetadataEnriched(e) => e,
        _ => return None,
    };

    if enriched.source != "whisper-transcriber" {
        host.log(
            "debug",
            &format!("ignoring metadata from source: {}", enriched.source),
        );
        return None;
    }

    let config: Option<SubtitleGeneratorConfig> =
        load_plugin_config(|key| host.get_plugin_data(key));
    let primary_language = config
        .as_ref()
        .map(|c| c.primary_language.as_str())
        .unwrap_or("en");

    let transcript = &enriched.metadata["transcript"];
    let detected_language = transcript
        .get("language")
        .and_then(|l| l.as_str())
        .unwrap_or(primary_language);

    let segments = match transcript.get("segments").and_then(|s| s.as_array()) {
        Some(arr) => arr,
        None => {
            host.log("debug", "no segments in transcript");
            return None;
        }
    };

    let foreign_segments: Vec<SrtSegment> = segments
        .iter()
        .filter_map(|seg| {
            let lang = seg.get("language").and_then(|l| l.as_str())?;
            let text = seg.get("text").and_then(|t| t.as_str())?;
            if text.is_empty() || lang == detected_language {
                return None;
            }
            let start = seg.get("start")?.as_f64()?;
            let end = seg.get("end")?.as_f64()?;
            Some(SrtSegment {
                start,
                end,
                text: text.to_string(),
            })
        })
        .collect();

    if foreign_segments.is_empty() {
        host.log(
            "debug",
            &format!("no foreign segments in {}", enriched.path.display()),
        );
        return None;
    }

    let srt_content = format_srt(&foreign_segments);

    let media_path = &enriched.path;
    let stem = media_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let parent = media_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let srt_path = parent.join(format!("{stem}.forced-eng.srt"));
    let srt_path_str = srt_path.to_string_lossy().to_string();

    let escaped = srt_content.replace('\'', "'\\''");
    let cmd = format!("printf '%s' '{escaped}' > '{srt_path_str}'");
    match host.run_tool("sh", &["-c".to_string(), cmd], 10_000) {
        Err(e) => {
            host.log("error", &format!("failed to write SRT file: {e}"));
            return None;
        }
        Ok(o) if o.exit_code != 0 => {
            host.log(
                "error",
                &format!(
                    "sh exited with code {}: {}",
                    o.exit_code,
                    String::from_utf8_lossy(&o.stderr)
                ),
            );
            return None;
        }
        Ok(_) => {}
    }

    host.log(
        "info",
        &format!(
            "wrote {} foreign segments to {}",
            foreign_segments.len(),
            srt_path.display()
        ),
    );

    let subtitle_event = Event::SubtitleGenerated(SubtitleGeneratedEvent::new(
        media_path.clone(),
        srt_path,
        "eng",
        true,
    ));

    let produced_payload = serialize_event(&subtitle_event)
        .map_err(|e| {
            host.log("error", &format!("failed to serialize event: {e}"));
        })
        .ok()?;

    Some(OnEventResult::new(
        "subtitle-generator",
        vec![(subtitle_event.event_type().to_string(), produced_payload)],
        None,
    ))
}

// --- SRT formatting ---

pub struct SrtSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub fn format_srt(segments: &[SrtSegment]) -> String {
    let mut output = String::new();
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }
        output.push_str(&format!(
            "{}\n{} --> {}\n{}\n",
            i + 1,
            format_timestamp(seg.start),
            format_timestamp(seg.end),
            seg.text,
        ));
    }
    output
}

fn format_timestamp(seconds: f64) -> String {
    let total_ms = (seconds * 1000.0) as u64;
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms % 3_600_000) / 60_000;
    let secs = (total_ms % 60_000) / 1_000;
    let ms = total_ms % 1_000;
    format!("{hours:02}:{minutes:02}:{secs:02},{ms:03}")
}

// --- Config ---

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SubtitleGeneratorConfig {
    #[serde(default = "default_primary_language")]
    pub primary_language: String,
}

fn default_primary_language() -> String {
    "en".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    struct MockHost {
        tool_calls: RefCell<Vec<(String, Vec<String>)>>,
        config: Option<SubtitleGeneratorConfig>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                tool_calls: RefCell::new(Vec::new()),
                config: None,
            }
        }
    }

    impl HostFunctions for MockHost {
        fn run_tool(
            &self,
            tool: &str,
            args: &[String],
            _timeout_ms: u64,
        ) -> Result<ToolOutput, String> {
            self.tool_calls
                .borrow_mut()
                .push((tool.to_string(), args.to_vec()));
            Ok(ToolOutput::new(0, vec![], vec![]))
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

    fn make_enriched_event(
        source: &str,
        primary_lang: &str,
        segments: serde_json::Value,
    ) -> Vec<u8> {
        let metadata = serde_json::json!({
            "source": source,
            "transcript": {
                "language": primary_lang,
                "segments": segments,
            },
        });
        let event = Event::MetadataEnriched(MetadataEnrichedEvent::new(
            PathBuf::from("/media/movies/test.mkv"),
            source.to_string(),
            metadata,
        ));
        serialize_event(&event).unwrap()
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "subtitle-generator");
        assert_eq!(info.capabilities, vec!["generate_subtitle"]);
    }

    #[test]
    fn test_handles() {
        assert!(handles("metadata.enriched"));
        assert!(!handles("file.introspected"));
        assert!(!handles("plan.created"));
    }

    #[test]
    fn test_multi_language_produces_subtitle_event() {
        let host = MockHost::new();
        let segments = serde_json::json!([
            {
                "start": 0.0, "end": 2.5,
                "text": "Hello there", "language": "en"
            },
            {
                "start": 5.0, "end": 8.0,
                "text": "Bonjour le monde", "language": "fr"
            },
            {
                "start": 10.0, "end": 12.5,
                "text": "Hola amigos", "language": "es"
            },
        ]);
        let payload = make_enriched_event("whisper-transcriber", "en", segments);

        let result = on_event("metadata.enriched", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "subtitle-generator");
        assert_eq!(result.produced_events.len(), 1);

        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        match produced {
            Event::SubtitleGenerated(e) => {
                assert_eq!(e.path, PathBuf::from("/media/movies/test.mkv"));
                assert_eq!(
                    e.subtitle_path,
                    PathBuf::from("/media/movies/test.forced-eng.srt")
                );
                assert_eq!(e.language, "eng");
                assert!(e.forced);
            }
            _ => panic!("expected SubtitleGenerated"),
        }

        // Verify the SRT content was written via shell
        let calls = host.tool_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "sh");
        let cmd = &calls[0].1[1];
        assert!(cmd.contains("Bonjour le monde"));
        assert!(cmd.contains("Hola amigos"));
        // Primary language segments should NOT appear
        assert!(!cmd.contains("Hello there"));
    }

    #[test]
    fn test_same_language_returns_none() {
        let host = MockHost::new();
        let segments = serde_json::json!([
            {
                "start": 0.0, "end": 2.5,
                "text": "Hello", "language": "en"
            },
            {
                "start": 3.0, "end": 5.0,
                "text": "World", "language": "en"
            },
        ]);
        let payload = make_enriched_event("whisper-transcriber", "en", segments);

        let result = on_event("metadata.enriched", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_non_whisper_source_returns_none() {
        let host = MockHost::new();
        let segments = serde_json::json!([
            {
                "start": 0.0, "end": 2.5,
                "text": "Bonjour", "language": "fr"
            },
        ]);
        let payload = make_enriched_event("some-other-plugin", "en", segments);

        let result = on_event("metadata.enriched", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_format_srt() {
        let segments = vec![
            SrtSegment {
                start: 0.0,
                end: 2.5,
                text: "Hello".to_string(),
            },
            SrtSegment {
                start: 5.0,
                end: 7.5,
                text: "World".to_string(),
            },
        ];
        let srt = format_srt(&segments);
        assert!(srt.contains("1\n00:00:00,000 --> 00:00:02,500\nHello"));
        assert!(srt.contains("2\n00:00:05,000 --> 00:00:07,500\nWorld"));
    }

    #[test]
    fn test_format_timestamp() {
        assert_eq!(format_timestamp(0.0), "00:00:00,000");
        assert_eq!(format_timestamp(61.5), "00:01:01,500");
        assert_eq!(format_timestamp(3661.123), "01:01:01,123");
    }

    #[test]
    fn test_empty_segments_returns_none() {
        let host = MockHost::new();
        let segments = serde_json::json!([]);
        let payload = make_enriched_event("whisper-transcriber", "en", segments);

        let result = on_event("metadata.enriched", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_config_default_primary_language() {
        let config: SubtitleGeneratorConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.primary_language, "en");
    }

    #[test]
    fn test_config_custom_primary_language() {
        let config: SubtitleGeneratorConfig =
            serde_json::from_str(r#"{"primary_language":"ja"}"#).unwrap();
        assert_eq!(config.primary_language, "ja");
    }

    #[test]
    fn test_wrong_event_type_returns_none() {
        let host = MockHost::new();
        let result = on_event("file.introspected", &[], &host);
        assert!(result.is_none());
    }
}
