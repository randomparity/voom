use std::collections::HashMap;

use crate::events::ExecutorCapabilitiesEvent;

/// Aggregated view of executor capabilities across all registered executors.
///
/// Built from `ExecutorCapabilitiesEvent`s emitted during plugin init.
/// Used by the policy evaluator to validate plans against real capabilities.
#[derive(Debug, Clone, Default)]
pub struct CapabilityMap {
    executors: HashMap<String, ExecutorCapabilitiesEvent>,
}

impl CapabilityMap {
    #[must_use]
    pub fn new() -> Self {
        Self {
            executors: HashMap::new(),
        }
    }

    /// Register capabilities from an executor's init-time probe.
    pub fn register(&mut self, event: ExecutorCapabilitiesEvent) {
        self.executors.insert(event.plugin_name.clone(), event);
    }

    /// Returns `true` if any registered executor can encode this codec.
    #[must_use]
    pub fn has_encoder(&self, codec: &str) -> bool {
        self.executors
            .values()
            .any(|e| e.codecs.encoders.iter().any(|c| c == codec))
    }

    /// Returns `true` if any registered executor supports this format.
    #[must_use]
    pub fn has_format(&self, format: &str) -> bool {
        self.executors
            .values()
            .any(|e| e.formats.iter().any(|f| f == format))
    }

    /// Returns the names of executors that can encode the given codec.
    #[must_use]
    pub fn encoders_for(&self, codec: &str) -> Vec<&str> {
        self.executors
            .iter()
            .filter(|(_, e)| e.codecs.encoders.iter().any(|c| c == codec))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Returns `true` if no executor capabilities have been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.executors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::CodecCapabilities;

    #[test]
    fn test_empty_map_queries() {
        let map = CapabilityMap::new();
        assert!(map.is_empty());
        assert!(!map.has_encoder("libx264"));
        assert!(!map.has_format("matroska"));
        assert!(map.encoders_for("libx264").is_empty());
    }

    #[test]
    fn test_register_and_query_encoder() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(
                vec!["h264".into(), "hevc".into()],
                vec!["libx264".into(), "libx265".into(), "aac".into()],
            ),
            vec!["matroska".into(), "mp4".into()],
            vec![],
        ));

        assert!(!map.is_empty());
        assert!(map.has_encoder("libx264"));
        assert!(map.has_encoder("aac"));
        assert!(!map.has_encoder("opus"));
    }

    #[test]
    fn test_register_and_query_format() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::empty(),
            vec!["matroska".into(), "mp4".into()],
            vec![],
        ));

        assert!(map.has_format("matroska"));
        assert!(map.has_format("mp4"));
        assert!(!map.has_format("webm"));
    }

    #[test]
    fn test_multiple_executors_overlapping_codecs() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(vec![], vec!["libx264".into(), "aac".into()]),
            vec!["matroska".into()],
            vec![],
        ));
        map.register(ExecutorCapabilitiesEvent::new(
            "mkvtoolnix-executor",
            CodecCapabilities::empty(),
            vec!["matroska".into()],
            vec![],
        ));

        assert!(map.has_encoder("libx264"));
        let encoders = map.encoders_for("libx264");
        assert_eq!(encoders, vec!["ffmpeg-executor"]);

        let encoders = map.encoders_for("aac");
        assert_eq!(encoders, vec!["ffmpeg-executor"]);

        // Both support matroska
        assert!(map.has_format("matroska"));
    }

    #[test]
    fn test_encoders_for_returns_all_matching() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "executor-a",
            CodecCapabilities::new(vec![], vec!["aac".into()]),
            vec![],
            vec![],
        ));
        map.register(ExecutorCapabilitiesEvent::new(
            "executor-b",
            CodecCapabilities::new(vec![], vec!["aac".into(), "opus".into()]),
            vec![],
            vec![],
        ));

        let mut encoders = map.encoders_for("aac");
        encoders.sort();
        assert_eq!(encoders, vec!["executor-a", "executor-b"]);
    }
}
