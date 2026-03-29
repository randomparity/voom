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

    /// Direct access to an executor's capabilities by plugin name.
    #[must_use]
    pub fn executor_capabilities(&self, name: &str) -> Option<&ExecutorCapabilitiesEvent> {
        self.executors.get(name)
    }

    /// Deduplicated list of hardware acceleration backends across
    /// all executors, normalized to canonical names.
    #[must_use]
    pub fn hw_accels(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for event in self.executors.values() {
            for raw in &event.hw_accels {
                let name = normalize_hwaccel(raw);
                if seen.insert(name) {
                    result.push(name);
                }
            }
        }
        result
    }

    /// Returns the highest-priority hwaccel backend name, or `"none"`.
    ///
    /// Priority order matches `HwAccelConfig::from_probed()` in the
    /// ffmpeg-executor: nvenc > qsv > vaapi > videotoolbox.
    #[must_use]
    pub fn best_hwaccel(&self) -> &str {
        let accels = self.hw_accels();
        for candidate in &["nvenc", "qsv", "vaapi", "videotoolbox"] {
            if accels.contains(candidate) {
                return candidate;
            }
        }
        "none"
    }
}

/// Normalize raw ffmpeg hwaccel names to canonical backend names.
fn normalize_hwaccel(raw: &str) -> &'static str {
    // Compare case-insensitively without allocating
    let lower: String = raw.to_ascii_lowercase();
    match lower.as_str() {
        "cuda" | "nvdec" | "nvenc" => "nvenc",
        "qsv" => "qsv",
        "vaapi" => "vaapi",
        "videotoolbox" => "videotoolbox",
        _ => "unknown",
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
    fn test_executor_capabilities_lookup() {
        let mut map = CapabilityMap::new();
        assert!(map.executor_capabilities("ffmpeg-executor").is_none());
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::empty(),
            vec![],
            vec!["cuda".into()],
        ));
        let caps = map.executor_capabilities("ffmpeg-executor").unwrap();
        assert_eq!(caps.hw_accels, vec!["cuda"]);
        assert!(map.executor_capabilities("other").is_none());
    }

    #[test]
    fn test_hw_accels_normalized_and_deduped() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "exec-a",
            CodecCapabilities::empty(),
            vec![],
            vec!["cuda".into(), "nvdec".into(), "vaapi".into()],
        ));
        map.register(ExecutorCapabilitiesEvent::new(
            "exec-b",
            CodecCapabilities::empty(),
            vec![],
            vec!["cuda".into(), "qsv".into()],
        ));
        let mut accels = map.hw_accels();
        accels.sort();
        assert_eq!(accels, vec!["nvenc", "qsv", "vaapi"]);
    }

    #[test]
    fn test_best_hwaccel_priority() {
        let mut map = CapabilityMap::new();
        // vaapi + qsv → qsv wins
        map.register(ExecutorCapabilitiesEvent::new(
            "exec",
            CodecCapabilities::empty(),
            vec![],
            vec!["vaapi".into(), "qsv".into()],
        ));
        assert_eq!(map.best_hwaccel(), "qsv");
    }

    #[test]
    fn test_best_hwaccel_nvenc_highest() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "exec",
            CodecCapabilities::empty(),
            vec![],
            vec!["vaapi".into(), "cuda".into()],
        ));
        assert_eq!(map.best_hwaccel(), "nvenc");
    }

    #[test]
    fn test_best_hwaccel_none() {
        let map = CapabilityMap::new();
        assert_eq!(map.best_hwaccel(), "none");
    }

    #[test]
    fn test_best_hwaccel_no_recognized() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "exec",
            CodecCapabilities::empty(),
            vec![],
            vec!["drm".into()],
        ));
        assert_eq!(map.best_hwaccel(), "none");
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
