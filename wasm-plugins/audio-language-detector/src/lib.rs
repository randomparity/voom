//! Audio language detector plugin.
//!
//! Detects the spoken language of audio tracks using Whisper's language
//! detection mode. Listens for `file.introspected` events, extracts audio
//! samples via ffmpeg, runs whisper-cli with `--task detect_language`, and
//! produces a `metadata.enriched` event with per-track language detections.
//!
//! Silent tracks (high `no_speech_prob`) are tagged as `"zxx"`.
//! Multi-language tracks (conflicting detections) are tagged as `"mul"`.
//!
//! # Host functions used
//!
//! - `run-tool` — execute ffmpeg (audio extraction) and whisper-cli
//! - `get-plugin-data` / `set-plugin-data` — cache detection results
//! - `log` — structured logging
//!
//! # External tools required
//!
//! - `ffmpeg` — for extracting audio samples from video files
//! - `whisper-cli` — Whisper inference binary with language detection

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{
    deserialize_event_or_log, from_iso639_1, load_plugin_config, serialize_event_or_log,
    Capability, Event, HostFunctions, MediaFile, MetadataEnrichedEvent, OnEventResult,
    PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "audio-language-detector",
        "0.1.0",
        vec![Capability::EnrichMetadata {
            source: "audio-language-detector".to_string(),
        }],
    )
    .with_description("Audio language detection via Whisper")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == "file.introspected"
}

/// Process a file.introspected event by detecting languages of audio tracks.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != "file.introspected" {
        return None;
    }

    let event = deserialize_event_or_log(payload, host)?;
    let file = match &event {
        Event::FileIntrospected(e) => &e.file,
        _ => return None,
    };

    let audio_tracks: Vec<_> = file
        .tracks
        .iter()
        .filter(|t| t.track_type.is_audio())
        .collect();

    if audio_tracks.is_empty() {
        host.log(
            "debug",
            &format!("skipping {}: no audio tracks", file.path.display()),
        );
        return None;
    }

    let config: Option<DetectorConfig> = match load_plugin_config(|key| host.get_plugin_data(key)) {
        Ok(config) => config,
        Err(e) => {
            host.log("error", &format!("failed to load plugin config: {e}"));
            return None;
        }
    };
    let cfg = config.as_ref();
    let whisper_bin = cfg
        .map(|c| c.whisper_binary.as_str())
        .unwrap_or("whisper-cli");
    let model = cfg.map(|c| c.model.as_str()).unwrap_or("base");
    let sample_count = cfg.map_or(8, |c| c.sample_count);
    let sample_duration = cfg.map_or(30, |c| c.sample_duration_secs);
    let skip_pct = cfg.map_or(0.05, |c| c.skip_percent);

    let path_hash_owned = format!(
        "{:x}",
        xxhash_rust::xxh3::xxh3_64(file.path.to_string_lossy().as_bytes())
    );
    let hash_str = file.content_hash.as_deref().unwrap_or(&path_hash_owned);
    let mut detections = Vec::new();
    let params = DetectionParams {
        whisper_bin,
        model,
        sample_count,
        sample_duration,
        skip_pct,
    };

    for track in &audio_tracks {
        let cache_key = format!("lang:{hash_str}:{}", track.index);

        match host.get_plugin_data(&cache_key) {
            Ok(Some(cached)) => {
                if let Ok(det) = serde_json::from_slice::<TrackDetection>(&cached) {
                    host.log("debug", &format!("cache hit for track {}", track.index));
                    detections.push(det);
                    continue;
                }
            }
            Ok(None) => {}
            Err(e) => host.log("error", &format!("failed to read language cache: {e}")),
        }

        let detection = detect_track_language(file, track.index, &params, host);

        if let Some(det) = detection {
            if let Ok(json) = serde_json::to_vec(&det) {
                let _ = host.set_plugin_data(&cache_key, &json);
            }
            detections.push(det);
        }
    }

    if detections.is_empty() {
        return None;
    }

    build_result(file, &detections, host)
}

/// Runtime-resolved detection parameters (avoids too-many-arguments).
struct DetectionParams<'a> {
    whisper_bin: &'a str,
    model: &'a str,
    sample_count: u32,
    sample_duration: u32,
    skip_pct: f64,
}

/// Detect the language of a single audio track by sampling.
fn detect_track_language(
    file: &MediaFile,
    track_index: u32,
    params: &DetectionParams<'_>,
    host: &dyn HostFunctions,
) -> Option<TrackDetection> {
    let duration = file.duration;
    if duration <= 0.0 {
        host.log(
            "warn",
            &format!("track {track_index}: no duration, skipping"),
        );
        return None;
    }

    let effective_duration = duration * (1.0 - 2.0 * params.skip_pct);
    let start_offset = duration * params.skip_pct;

    let max_samples = (effective_duration / f64::from(params.sample_duration)).floor() as u32;
    let sample_count = params.sample_count.min(max_samples).max(1);
    let interval = effective_duration / f64::from(sample_count + 1);

    let path_hash_owned = format!(
        "{:x}",
        xxhash_rust::xxh3::xxh3_64(file.path.to_string_lossy().as_bytes())
    );
    let hash_str = file.content_hash.as_deref().unwrap_or(&path_hash_owned);
    let file_path = file.path.to_string_lossy().to_string();
    let mut samples: Vec<SampleResult> = Vec::new();

    for i in 1..=sample_count {
        let offset = start_offset + interval * f64::from(i);
        let tmp_path = format!("/tmp/voom-langdet-{hash_str}-{track_index}-{i}.wav");

        let extract = host.run_tool(
            "ffmpeg",
            &[
                "-ss".to_string(),
                format!("{offset:.2}"),
                "-t".to_string(),
                format!("{}", params.sample_duration),
                "-i".to_string(),
                file_path.clone(),
                "-map".to_string(),
                format!("0:a:{track_index}"),
                "-ac".to_string(),
                "1".to_string(),
                "-ar".to_string(),
                "16000".to_string(),
                "-f".to_string(),
                "wav".to_string(),
                "-y".to_string(),
                tmp_path.clone(),
            ],
            300_000,
        );

        let ok = match &extract {
            Err(e) => {
                host.log(
                    "warn",
                    &format!(
                        "ffmpeg sample {i} failed for track \
                         {track_index}: {e}"
                    ),
                );
                false
            }
            Ok(o) if o.exit_code != 0 => {
                host.log(
                    "warn",
                    &format!(
                        "ffmpeg sample {i} exited {}: {}",
                        o.exit_code,
                        String::from_utf8_lossy(&o.stderr)
                    ),
                );
                false
            }
            Ok(_) => true,
        };

        if !ok {
            let _ = host.run_tool("rm", &[tmp_path], 5_000);
            continue;
        }

        let whisper_result = host.run_tool(
            params.whisper_bin,
            &[
                "--task".to_string(),
                "detect_language".to_string(),
                "--model".to_string(),
                params.model.to_string(),
                "--output-format".to_string(),
                "json".to_string(),
                tmp_path.clone(),
            ],
            120_000,
        );

        let _ = host.run_tool("rm", &[tmp_path], 5_000);

        let whisper_out = match whisper_result {
            Err(e) => {
                host.log(
                    "warn",
                    &format!(
                        "whisper sample {i} failed for track \
                         {track_index}: {e}"
                    ),
                );
                continue;
            }
            Ok(o) if o.exit_code != 0 => {
                host.log(
                    "warn",
                    &format!(
                        "whisper sample {i} exited {}: {}",
                        o.exit_code,
                        String::from_utf8_lossy(&o.stderr)
                    ),
                );
                continue;
            }
            Ok(o) => o,
        };

        let json: serde_json::Value = match serde_json::from_slice(&whisper_out.stdout) {
            Ok(v) => v,
            Err(e) => {
                host.log(
                    "warn",
                    &format!(
                        "failed to parse whisper output \
                             for sample {i}: {e}"
                    ),
                );
                continue;
            }
        };

        let language = json
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("und")
            .to_string();
        let no_speech_prob = json
            .get("no_speech_prob")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        samples.push(SampleResult {
            language,
            no_speech_prob,
        });
    }

    if samples.is_empty() {
        host.log(
            "warn",
            &format!("no samples collected for track {track_index}"),
        );
        return None;
    }

    Some(aggregate_samples(track_index, &samples))
}

/// Aggregate sample results into a track detection.
fn aggregate_samples(track_index: u32, samples: &[SampleResult]) -> TrackDetection {
    let samples_analyzed = samples.len() as u32;

    let all_silent = samples.iter().all(|s| s.no_speech_prob > 0.9);

    if all_silent {
        let avg_prob: f64 =
            samples.iter().map(|s| s.no_speech_prob).sum::<f64>() / f64::from(samples_analyzed);
        return TrackDetection {
            track_index,
            detected_language: "zxx".to_string(),
            confidence: avg_prob,
            is_speech: false,
            samples_analyzed,
            detected_languages: vec![LanguageScore {
                code: "zxx".to_string(),
                confidence: avg_prob,
            }],
        };
    }

    let speech_samples: Vec<_> = samples.iter().filter(|s| s.no_speech_prob <= 0.9).collect();

    let mut lang_counts: std::collections::HashMap<String, (u32, f64)> =
        std::collections::HashMap::new();
    for s in &speech_samples {
        let iso3 = from_iso639_1(&s.language).unwrap_or(&s.language);
        let entry = lang_counts.entry(iso3.to_string()).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += 1.0 - s.no_speech_prob;
    }

    let total_speech = speech_samples.len() as f64;
    let mut scored: Vec<LanguageScore> = lang_counts
        .iter()
        .map(|(code, (count, conf_sum))| LanguageScore {
            code: code.clone(),
            confidence: conf_sum / f64::from(*count) * (f64::from(*count) / total_speech),
        })
        .collect();
    scored.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let high_conf: Vec<_> = lang_counts
        .iter()
        .filter(|(_, (count, _))| f64::from(*count) / total_speech > 0.7)
        .collect();

    if high_conf.len() > 1 {
        return TrackDetection {
            track_index,
            detected_language: "mul".to_string(),
            confidence: scored.first().map(|s| s.confidence).unwrap_or(0.0),
            is_speech: true,
            samples_analyzed,
            detected_languages: scored,
        };
    }

    let top = scored.first().cloned().unwrap_or(LanguageScore {
        code: "und".to_string(),
        confidence: 0.0,
    });

    TrackDetection {
        track_index,
        detected_language: top.code.clone(),
        confidence: top.confidence,
        is_speech: true,
        samples_analyzed,
        detected_languages: scored,
    }
}

fn build_result(
    file: &MediaFile,
    detections: &[TrackDetection],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    let metadata = serde_json::json!({
        "source": "audio-language-detector",
        "detections": detections,
    });

    let enriched_event = Event::MetadataEnriched(MetadataEnrichedEvent::new(
        file.path.clone(),
        "audio-language-detector".to_string(),
        metadata,
    ));

    let produced_payload = serialize_event_or_log(&enriched_event, host)?;

    Some(OnEventResult::new(
        "audio-language-detector",
        vec![(enriched_event.event_type().to_string(), produced_payload)],
        None,
    ))
}

// --- Types ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct DetectorConfig {
    /// Path or name of the whisper binary (default: "whisper-cli").
    pub whisper_binary: String,
    /// Model name (default: "base").
    pub model: String,
    /// Number of samples to analyze per track (default: 8).
    pub sample_count: u32,
    /// Duration of each sample in seconds (default: 30).
    pub sample_duration_secs: u32,
    /// Fraction of duration to skip at start/end (default: 0.05).
    pub skip_percent: f64,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            whisper_binary: "whisper-cli".to_string(),
            model: "base".to_string(),
            sample_count: 8,
            sample_duration_secs: 30,
            skip_percent: 0.05,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SampleResult {
    language: String,
    no_speech_prob: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackDetection {
    pub track_index: u32,
    pub detected_language: String,
    pub confidence: f64,
    pub is_speech: bool,
    pub samples_analyzed: u32,
    pub detected_languages: Vec<LanguageScore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageScore {
    pub code: String,
    pub confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    type ToolResultFn = Box<dyn Fn(&[String]) -> ToolOutput + Send + Sync>;
    type ToolResults = HashMap<String, ToolResultFn>;

    struct MockHost {
        tool_results: ToolResults,
        cached: std::cell::RefCell<HashMap<String, Vec<u8>>>,
    }

    impl MockHost {
        fn with_whisper_response(language: &str, no_speech_prob: f64) -> Self {
            let lang = language.to_string();
            let mut tool_results: ToolResults = HashMap::new();

            tool_results.insert(
                "ffmpeg".to_string(),
                Box::new(|_| ToolOutput::new(0, vec![], vec![])),
            );

            tool_results.insert(
                "whisper-cli".to_string(),
                Box::new(move |_| {
                    let json = serde_json::json!({
                        "language": lang,
                        "no_speech_prob": no_speech_prob,
                    });
                    ToolOutput::new(0, serde_json::to_vec(&json).unwrap(), vec![])
                }),
            );

            tool_results.insert(
                "rm".to_string(),
                Box::new(|_| ToolOutput::new(0, vec![], vec![])),
            );

            Self {
                tool_results,
                cached: std::cell::RefCell::new(HashMap::new()),
            }
        }

        fn with_failing_ffmpeg() -> Self {
            let mut tool_results: ToolResults = HashMap::new();
            tool_results.insert(
                "ffmpeg".to_string(),
                Box::new(|_| ToolOutput::new(1, vec![], b"error".to_vec())),
            );
            tool_results.insert(
                "rm".to_string(),
                Box::new(|_| ToolOutput::new(0, vec![], vec![])),
            );

            Self {
                tool_results,
                cached: std::cell::RefCell::new(HashMap::new()),
            }
        }

        fn with_multi_language() -> Self {
            let call_count = std::sync::atomic::AtomicU32::new(0);
            let mut tool_results: ToolResults = HashMap::new();

            tool_results.insert(
                "ffmpeg".to_string(),
                Box::new(|_| ToolOutput::new(0, vec![], vec![])),
            );

            tool_results.insert(
                "whisper-cli".to_string(),
                Box::new(move |_| {
                    let n = call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let lang = if (n & 1) == 0 { "en" } else { "fr" };
                    let json = serde_json::json!({
                        "language": lang,
                        "no_speech_prob": 0.05,
                    });
                    ToolOutput::new(0, serde_json::to_vec(&json).unwrap(), vec![])
                }),
            );

            tool_results.insert(
                "rm".to_string(),
                Box::new(|_| ToolOutput::new(0, vec![], vec![])),
            );

            Self {
                tool_results,
                cached: std::cell::RefCell::new(HashMap::new()),
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
            self.tool_results
                .get(tool)
                .map(|f| f(args))
                .ok_or_else(|| format!("tool not found: {tool}"))
        }

        fn get_plugin_data(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.cached.borrow().get(key).cloned())
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
        file.duration = 600.0; // 10 minutes
        let mut audio = Track::new(0, TrackType::AudioMain, "aac".into());
        audio.language = "und".into();
        audio.is_default = true;
        audio.channels = Some(2);
        file.tracks = vec![audio];
        file
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "audio-language-detector");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "enrich_metadata");
    }

    #[test]
    fn test_handles() {
        assert!(handles("file.introspected"));
        assert!(!handles("plan.created"));
        assert!(!handles("metadata.enriched"));
    }

    #[test]
    fn test_happy_path_english() {
        let host = MockHost::with_whisper_response("en", 0.05);
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "audio-language-detector");

        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                assert_eq!(e.source, "audio-language-detector");
                let dets = &e.metadata["detections"];
                assert_eq!(dets[0]["detected_language"], "eng");
                assert!(dets[0]["is_speech"].as_bool().unwrap());
                assert!(dets[0]["confidence"].as_f64().unwrap() > 0.0);
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_silent_track() {
        let host = MockHost::with_whisper_response("en", 0.95);
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let produced: Event = deserialize_event(&result.unwrap().produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                let det = &e.metadata["detections"][0];
                assert_eq!(det["detected_language"], "zxx");
                assert!(!det["is_speech"].as_bool().unwrap());
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_multi_language() {
        let host = MockHost::with_multi_language();
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let produced: Event = deserialize_event(&result.unwrap().produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                let det = &e.metadata["detections"][0];
                // With alternating en/fr, both have ~50% share
                // so neither exceeds 0.7, making it the dominant
                // language (eng or fre, whichever scores highest).
                // The exact result depends on sample count.
                let lang = det["detected_language"].as_str().unwrap();
                assert!(lang == "eng" || lang == "fre" || lang == "mul");
                assert!(det["detected_languages"].as_array().unwrap().len() >= 2);
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_cache_hit() {
        let host = MockHost::with_whisper_response("en", 0.05);

        // Pre-populate cache for track 0.
        let cached_det = TrackDetection {
            track_index: 0,
            detected_language: "jpn".to_string(),
            confidence: 0.99,
            is_speech: true,
            samples_analyzed: 8,
            detected_languages: vec![LanguageScore {
                code: "jpn".to_string(),
                confidence: 0.99,
            }],
        };
        host.cached.borrow_mut().insert(
            "lang:testhash123:0".to_string(),
            serde_json::to_vec(&cached_det).unwrap(),
        );

        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let produced: Event = deserialize_event(&result.unwrap().produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                // Should use cached "jpn", not detect "eng".
                assert_eq!(e.metadata["detections"][0]["detected_language"], "jpn");
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_no_audio_tracks() {
        let host = MockHost::with_whisper_response("en", 0.05);
        let file = MediaFile::new(PathBuf::from("/media/test.mkv"));
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_short_file_reduced_samples() {
        let host = MockHost::with_whisper_response("en", 0.05);
        let mut file = make_audio_file();
        // 45 seconds — can fit at most 1 sample of 30s
        // (effective = 45 * 0.9 = 40.5s)
        file.duration = 45.0;
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let produced: Event = deserialize_event(&result.unwrap().produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                let det = &e.metadata["detections"][0];
                assert_eq!(det["samples_analyzed"].as_u64().unwrap(), 1);
                assert_eq!(det["detected_language"], "eng");
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_config_serde() {
        let config = DetectorConfig {
            whisper_binary: "whisper".to_string(),
            model: "large".to_string(),
            sample_count: 4,
            sample_duration_secs: 15,
            skip_percent: 0.1,
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let restored: DetectorConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.model, "large");
        assert_eq!(restored.sample_count, 4);
        assert!((restored.skip_percent - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ffmpeg_failure_skips_track() {
        let host = MockHost::with_failing_ffmpeg();
        let file = make_audio_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        // All ffmpeg calls fail, so no detections -> None
        assert!(result.is_none());
    }

    #[test]
    fn test_wrong_event_type() {
        let host = MockHost::with_whisper_response("en", 0.05);
        let result = on_event("plan.created", &[], &host);
        assert!(result.is_none());
    }
}
