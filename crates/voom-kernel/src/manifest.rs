use serde::{Deserialize, Serialize};
use voom_domain::capabilities::Capability;

/// Plugin manifest describing a plugin's identity and requirements.
/// For native plugins this is built in code; for WASM plugins it's loaded from a TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<Capability>,
    pub handles_events: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDependency {
    pub name: String,
    pub version_req: String,
}

impl PluginManifest {
    /// Validate that all required fields are present and well-formed.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.name.is_empty() {
            errors.push("plugin name cannot be empty".to_string());
        }
        if self.version.is_empty() {
            errors.push("plugin version cannot be empty".to_string());
        }
        if self.capabilities.is_empty() && self.handles_events.is_empty() {
            errors.push("plugin must declare at least one capability or handled event".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_manifest() {
        let manifest = PluginManifest {
            name: "test-plugin".into(),
            version: "0.1.0".into(),
            description: "A test plugin".into(),
            capabilities: vec![Capability::Evaluate],
            handles_events: vec!["plan.created".into()],
            dependencies: vec![],
            config_schema: None,
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_empty_name_fails() {
        let manifest = PluginManifest {
            name: "".into(),
            version: "0.1.0".into(),
            description: "".into(),
            capabilities: vec![Capability::Evaluate],
            handles_events: vec![],
            dependencies: vec![],
            config_schema: None,
        };
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("name")));
    }

    #[test]
    fn test_no_capabilities_or_events_fails() {
        let manifest = PluginManifest {
            name: "empty".into(),
            version: "0.1.0".into(),
            description: "".into(),
            capabilities: vec![],
            handles_events: vec![],
            dependencies: vec![],
            config_schema: None,
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_manifest_serde_roundtrip() {
        let manifest = PluginManifest {
            name: "discovery".into(),
            version: "0.1.0".into(),
            description: "File discovery plugin".into(),
            capabilities: vec![Capability::Discover {
                schemes: vec!["file".into()],
            }],
            handles_events: vec!["file.discovered".into()],
            dependencies: vec![PluginDependency {
                name: "storage".into(),
                version_req: ">=0.1.0".into(),
            }],
            config_schema: None,
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "discovery");
        assert_eq!(deserialized.capabilities.len(), 1);
    }
}
