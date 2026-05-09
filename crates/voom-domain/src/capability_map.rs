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

    #[must_use]
    pub fn parallel_limit(&self, resource: &str) -> Option<usize> {
        self.executors
            .values()
            .flat_map(|event| &event.parallel_limits)
            .filter(|limit| limit.resource == resource && limit.max_parallel > 0)
            .map(|limit| limit.max_parallel)
            .min()
    }

    /// Deduplicated list of hardware acceleration backends reported by executors.
    #[must_use]
    pub fn hw_accels(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for event in self.executors.values() {
            for raw in &event.hw_accels {
                let name = raw.to_ascii_lowercase();
                if seen.insert(name.clone()) {
                    result.push(name);
                }
            }
        }
        result
    }

    /// Returns `true` if any registered executor supports the named HW backend.
    #[must_use]
    pub fn has_hwaccel(&self, backend: &str) -> bool {
        let backend = backend.to_ascii_lowercase();
        self.hw_accels().iter().any(|name| name == &backend)
    }

    #[must_use]
    pub fn default_parallel_resource(&self) -> Option<&str> {
        self.executors
            .values()
            .find_map(|event| event.default_parallel_resource.as_deref())
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
    fn test_parallel_limit_lookup_uses_lowest_positive_limit() {
        let mut map = CapabilityMap::new();
        map.register(
            ExecutorCapabilitiesEvent::new(
                "exec-a",
                CodecCapabilities::empty(),
                vec![],
                vec!["cuda".into()],
            )
            .with_parallel_limits(vec![crate::events::ExecutorParallelLimit::new(
                "hw:nvenc", 4,
            )]),
        );
        map.register(
            ExecutorCapabilitiesEvent::new(
                "exec-b",
                CodecCapabilities::empty(),
                vec![],
                vec!["cuda".into()],
            )
            .with_parallel_limits(vec![crate::events::ExecutorParallelLimit::new(
                "hw:nvenc", 2,
            )]),
        );

        assert_eq!(map.parallel_limit("hw:nvenc"), Some(2));
        assert_eq!(map.parallel_limit("hw:qsv"), None);
    }

    #[test]
    fn test_hw_accels_lowercased_and_deduped() {
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "exec-a",
            CodecCapabilities::empty(),
            vec![],
            vec!["CUDA".into(), "nvdec".into(), "vaapi".into()],
        ));
        map.register(ExecutorCapabilitiesEvent::new(
            "exec-b",
            CodecCapabilities::empty(),
            vec![],
            vec!["cuda".into(), "qsv".into()],
        ));
        let mut accels = map.hw_accels();
        accels.sort_unstable();
        assert_eq!(accels, vec!["cuda", "nvdec", "qsv", "vaapi"]);
        assert!(map.has_hwaccel("CUDA"));
        assert!(!map.has_hwaccel("nvenc"));
    }

    #[test]
    fn test_default_parallel_resource_uses_executor_advertisement() {
        let mut map = CapabilityMap::new();
        map.register(
            ExecutorCapabilitiesEvent::new(
                "exec",
                CodecCapabilities::empty(),
                vec![],
                vec!["cuda".into()],
            )
            .with_default_parallel_resource("hw:nvenc"),
        );

        assert_eq!(map.default_parallel_resource(), Some("hw:nvenc"));
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
        encoders.sort_unstable();
        assert_eq!(encoders, vec!["executor-a", "executor-b"]);
    }
}
