//! Common type aliases and helpers for plugin authors.

/// Plugin information returned by `get_info()`.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<String>,
    pub handled_events: Vec<String>,
}

impl PluginInfo {
    /// Create a new `PluginInfo` builder.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: String::new(),
            capabilities: Vec::new(),
            handled_events: Vec::new(),
        }
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Add a capability string (e.g., "`enrich_metadata:radarr`").
    #[must_use]
    pub fn capability(mut self, cap: impl Into<String>) -> Self {
        self.capabilities.push(cap.into());
        self
    }

    /// Add a handled event type (e.g., "file.introspected").
    #[must_use]
    pub fn handles(mut self, event_type: impl Into<String>) -> Self {
        self.handled_events.push(event_type.into());
        self
    }
}

/// Lightweight plugin info data for the WIT boundary.
///
/// Unlike [`PluginInfo`], this mirrors the WIT `plugin-info` record exactly
/// (name, version, capabilities) without the builder pattern extras.
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
    fn test_plugin_info_builder() {
        let info = PluginInfo::new("test-plugin", "0.1.0")
            .description("A test plugin")
            .capability("enrich_metadata:test")
            .handles("file.introspected")
            .handles("metadata.enriched");

        assert_eq!(info.name, "test-plugin");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.description, "A test plugin");
        assert_eq!(info.capabilities, vec!["enrich_metadata:test"]);
        assert_eq!(
            info.handled_events,
            vec!["file.introspected", "metadata.enriched"]
        );
    }
}
