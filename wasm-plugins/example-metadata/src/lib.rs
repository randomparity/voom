//! Example VOOM WASM plugin — metadata enrichment.
//!
//! This plugin demonstrates how to build a WASM plugin for VOOM using the
//! plugin SDK. It listens for `file.introspected` events and enriches
//! file metadata with additional information.
//!
//! # Building
//!
//! ```sh
//! # Install the wasm32 target
//! rustup target add wasm32-wasip1
//!
//! # Build the plugin
//! cargo build --target wasm32-wasip1 --release
//!
//! # The output .wasm file goes to:
//! # target/wasm32-wasip1/release/example_metadata.wasm
//! ```
//!
//! # Manifest
//!
//! Place a `example-metadata.toml` file alongside the `.wasm` file:
//!
//! ```toml
//! name = "example-metadata"
//! version = "0.1.0"
//! description = "Example metadata enrichment plugin"
//! handles_events = ["file.introspected"]
//!
//! [[capabilities]]
//! [capabilities.EnrichMetadata]
//! source = "example"
//! ```
//!
//! # How it works
//!
//! When a `file.introspected` event is received, this plugin:
//! 1. Deserializes the event payload
//! 2. Examines the file's tracks
//! 3. Produces a `metadata.enriched` event with additional info
//!
//! In a real plugin, you might call an external API (via host HTTP functions)
//! to look up movie/TV metadata from services like Radarr, Sonarr, or TMDb.

use voom_plugin_sdk::{
    deserialize_event, serialize_event, Capability, Event, HostFunctions, MetadataEnrichedEvent,
    OnEventResult, PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "example-metadata",
        "0.1.0",
        vec![Capability::EnrichMetadata {
            source: "example".to_string(),
        }],
    )
    .with_description("Example metadata enrichment plugin")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == Event::FILE_INTROSPECTED
}

pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != Event::FILE_INTROSPECTED {
        return None;
    }

    let event = deserialize_event(payload).map_err(|e| {
        host.log("error", &format!("failed to deserialize event: {e}"));
    }).ok()?;

    match &event {
        Event::FileIntrospected(introspected) => {
            let file = &introspected.file;

            // Count tracks by type in a single pass.
            let (mut video_count, mut audio_count, mut sub_count, mut has_hdr) =
                (0usize, 0usize, 0usize, false);
            for t in &file.tracks {
                if t.track_type.is_video() {
                    video_count += 1;
                }
                if t.track_type.is_audio() {
                    audio_count += 1;
                }
                if t.track_type.is_subtitle() {
                    sub_count += 1;
                }
                has_hdr |= t.is_hdr;
            }

            let metadata = serde_json::json!({
                "source": "example-metadata",
                "track_summary": {
                    "video_tracks": video_count,
                    "audio_tracks": audio_count,
                    "subtitle_tracks": sub_count,
                    "total_tracks": file.tracks.len(),
                },
                "container": format!("{:?}", file.container),
                "has_hdr": has_hdr,
            });

            let enriched_event = Event::MetadataEnriched(MetadataEnrichedEvent::new(
                file.path.clone(),
                "example-metadata".to_string(),
                metadata,
            ));

            let produced_payload = serialize_event(&enriched_event).map_err(|e| {
                host.log("error", &format!("failed to serialize event: {e}"));
            }).ok()?;

            Some(OnEventResult::new(
                "example-metadata",
                vec![(enriched_event.event_type().to_string(), produced_payload)],
                None,
            ))
        }
        _ => None,
    }
}

// NOTE: In a real WASM plugin, you would use wit_bindgen::generate! to
// generate the Guest trait and export! macro, then implement the trait:
//
// wit_bindgen::generate!({
//     world: "voom-plugin",
//     path: "../../crates/voom-wit/wit",
// });
//
// struct ExampleMetadata;
//
// impl Guest for ExampleMetadata {
//     fn get_info() -> PluginInfo { ... }
//     fn handles(event_type: String) -> bool { ... }
//     fn on_event(event: EventData) -> Option<EventResult> { ... }
// }
//
// export!(ExampleMetadata);

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    struct NoopHost;
    impl HostFunctions for NoopHost {
        fn get_plugin_data(&self, _key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn set_plugin_data(&self, _key: &str, _value: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn log(&self, _level: &str, _message: &str) {}
    }

    fn make_test_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/movies/test.mkv"));
        file.size = 5_000_000_000;
        file.content_hash = Some("abc123".to_string());
        file.container = Container::Mkv;
        file.duration = 7200.0;
        file.bitrate = Some(8_000_000);
        let mut video = Track::new(0, TrackType::Video, "hevc".into());
        video.is_default = true;
        video.width = Some(3840);
        video.height = Some(2160);
        video.frame_rate = Some(23.976);
        video.is_hdr = true;
        video.hdr_format = Some("HDR10".into());
        video.pixel_format = Some("yuv420p10le".into());

        let mut audio = Track::new(1, TrackType::AudioMain, "truehd".into());
        audio.language = "eng".into();
        audio.title = "TrueHD Atmos 7.1".into();
        audio.is_default = true;
        audio.channels = Some(8);
        audio.channel_layout = Some("7.1".into());
        audio.sample_rate = Some(48000);
        audio.bit_depth = Some(24);

        let mut sub = Track::new(2, TrackType::SubtitleMain, "subrip".into());
        sub.language = "eng".into();
        sub.title = "English".into();
        sub.is_default = true;

        file.tracks = vec![video, audio, sub];
        file
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "example-metadata");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "enrich_metadata");
    }

    #[test]
    fn test_handles() {
        assert!(handles("file.introspected"));
        assert!(!handles("file.discovered"));
        assert!(!handles("plan.created"));
    }

    #[test]
    fn test_on_event_file_introspected() {
        let file = make_test_file();
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));

        let payload = serialize_event(&event).unwrap();
        let result = on_event("file.introspected", &payload, &NoopHost);

        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "example-metadata");
        assert_eq!(result.produced_events.len(), 1);
        assert_eq!(result.produced_events[0].0, "metadata.enriched");

        // Deserialize the produced event and check the metadata.
        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(enriched) => {
                assert_eq!(enriched.source, "example-metadata");
                assert_eq!(enriched.metadata["track_summary"]["video_tracks"], 1);
                assert_eq!(enriched.metadata["track_summary"]["audio_tracks"], 1);
                assert_eq!(enriched.metadata["track_summary"]["subtitle_tracks"], 1);
                assert_eq!(enriched.metadata["has_hdr"], true);
            }
            _ => panic!("expected MetadataEnriched event"),
        }
    }

    #[test]
    fn test_on_event_wrong_type() {
        let result = on_event("file.discovered", &[], &NoopHost);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_invalid_payload() {
        let result = on_event("file.introspected", &[0xFF, 0xFE], &NoopHost);
        assert!(result.is_none());
    }
}
