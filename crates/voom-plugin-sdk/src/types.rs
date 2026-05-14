//! Common type aliases and helpers for plugin authors.

use voom_domain::capabilities::Capability;

/// Plugin information for the WIT boundary.
///
/// Mirrors the WIT `plugin-info` record. Used by WASM plugins
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
    pub capabilities: Vec<Capability>,
}

impl PluginInfoData {
    /// Create a new `PluginInfoData` with only the required fields.
    ///
    /// Use the builder-style setters to populate optional metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_plugin_sdk::PluginInfoData;
    /// use voom_domain::capabilities::Capability;
    ///
    /// let info = PluginInfoData::new(
    ///     "my-plugin",
    ///     "1.0.0",
    ///     vec![Capability::EvaluatePolicy],
    /// )
    /// .with_description("Evaluates policies")
    /// .with_author("VOOM Contributors")
    /// .with_license("MIT");
    ///
    /// assert_eq!(info.name, "my-plugin");
    /// assert_eq!(info.description, "Evaluates policies");
    /// assert_eq!(info.license, "MIT");
    /// ```
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        capabilities: Vec<Capability>,
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
    pub claimed: bool,
    pub execution_error: Option<String>,
    pub execution_detail: Option<Vec<u8>>,
}

impl OnEventResult {
    #[must_use]
    pub fn new(
        plugin_name: impl Into<String>,
        produced_events: Vec<(String, Vec<u8>)>,
        data: Option<Vec<u8>>,
    ) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events,
            data,
            claimed: false,
            execution_error: None,
            execution_detail: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_info_data_structured_capabilities() {
        let info = PluginInfoData::new(
            "test-plugin",
            "0.1.0",
            vec![Capability::EnrichMetadata {
                source: "test".to_string(),
            }],
        );
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "enrich_metadata");
    }

    #[test]
    fn test_plugin_info_data_fields() {
        let info = PluginInfoData::new(
            "test-plugin",
            "0.1.0",
            vec![Capability::EnrichMetadata {
                source: "test".to_string(),
            }],
        );
        assert_eq!(info.name, "test-plugin");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.description, "");
        assert_eq!(info.author, "");
        assert_eq!(info.license, "");
        assert_eq!(info.homepage, "");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "enrich_metadata");
    }

    #[test]
    fn test_plugin_info_data_builder() {
        let info = PluginInfoData::new(
            "my-plugin",
            "1.0.0",
            vec![Capability::Execute {
                operations: vec![],
                formats: vec!["mkv".to_string()],
            }],
        )
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
