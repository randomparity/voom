//! Common type aliases and helpers for plugin authors.

/// Plugin information for the WIT boundary.
///
/// Mirrors the WIT `plugin-info` record exactly. Used by WASM plugins
/// in their `get_info()` implementation.
#[derive(Debug, Clone)]
pub struct PluginInfoData {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

/// Result from processing an event, mirroring the WIT `event-result` record.
#[derive(Debug, Clone)]
pub struct OnEventResult {
    pub plugin_name: String,
    pub produced_events: Vec<(String, Vec<u8>)>,
    pub data: Option<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_info_data_fields() {
        let info = PluginInfoData {
            name: "test-plugin".into(),
            version: "0.1.0".into(),
            capabilities: vec!["enrich_metadata:test".into()],
        };
        assert_eq!(info.name, "test-plugin");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.capabilities, vec!["enrich_metadata:test"]);
    }
}
