//! Common type aliases and helpers for plugin authors.

/// Plugin information for the WIT boundary.
///
/// Mirrors the WIT `plugin-info` record exactly. Used by WASM plugins
/// in their `get_info()` implementation.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PluginInfoData {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub homepage: String,
    pub capabilities: Vec<String>,
}

impl PluginInfoData {
    /// Create a new `PluginInfoData` with only the required fields.
    ///
    /// Description, author, license, and homepage default to empty
    /// strings. Use the builder-style setters to populate them.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            homepage: String::new(),
            capabilities,
        }
    }

    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    #[must_use]
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    #[must_use]
    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = license.into();
        self
    }

    #[must_use]
    pub fn with_homepage(mut self, url: impl Into<String>) -> Self {
        self.homepage = url.into();
        self
    }
}

/// Result from processing an event, mirroring the WIT `event-result` record.
#[non_exhaustive]
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
        let info = PluginInfoData::new("test-plugin", "0.1.0", vec!["enrich_metadata:test".into()]);
        assert_eq!(info.name, "test-plugin");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.description, "");
        assert_eq!(info.author, "");
        assert_eq!(info.license, "");
        assert_eq!(info.homepage, "");
        assert_eq!(info.capabilities, vec!["enrich_metadata:test"]);
    }

    #[test]
    fn test_plugin_info_data_builder() {
        let info = PluginInfoData::new("my-plugin", "1.0.0", vec!["execute:ffmpeg".into()])
            .with_description("A cool plugin")
            .with_author("VOOM Contributors")
            .with_license("MIT")
            .with_homepage("https://example.com");

        assert_eq!(info.description, "A cool plugin");
        assert_eq!(info.author, "VOOM Contributors");
        assert_eq!(info.license, "MIT");
        assert_eq!(info.homepage, "https://example.com");
    }
}
