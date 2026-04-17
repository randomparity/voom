use serde::{Deserialize, Serialize};
use voom_domain::capabilities::Capability;

/// Current protocol version for MessagePack serialization at the WASM boundary.
/// Plugins declaring a `protocol_version` must match this value to load.
pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

/// Plugin manifest describing a plugin's identity and requirements.
/// For native plugins this is built in code; for WASM plugins it's loaded from a TOML file.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: String,
    pub capabilities: Vec<Capability>,
    pub handles_events: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    /// Allowed HTTP domains for this plugin (empty = deny all).
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Allowed filesystem paths for this plugin.
    /// - `None` (field omitted): inherit host/config-provided paths.
    /// - `Some([])` (explicit empty): deny all filesystem access.
    /// - `Some([...])`: allow only the listed paths.
    #[serde(default)]
    pub allowed_paths: Option<Vec<String>>,
    /// Event bus priority for this plugin (lower = runs first in dispatch).
    /// Defaults to 70 if not specified in the manifest.
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// MessagePack protocol version for WASM boundary serialization.
    /// `None` means the plugin predates versioning and is treated as compatible.
    /// If set, must match `CURRENT_PROTOCOL_VERSION` at load time.
    #[serde(default)]
    pub protocol_version: Option<u32>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDependency {
    pub name: String,
    pub version_req: String,
}

fn default_priority() -> i32 {
    70
}

impl PluginManifest {
    /// Validate that all required fields are present and well-formed.
    ///
    /// Returns `Err(Vec<String>)` containing **all** validation errors (not just
    /// the first); callers can surface every issue to the user at once instead of
    /// requiring fix-and-rerun cycles.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.name.is_empty() {
            errors.push("plugin name cannot be empty".to_string());
        }
        if self.version.is_empty() {
            errors.push("plugin version cannot be empty".to_string());
        } else if let Err(e) = semver::Version::parse(&self.version) {
            errors.push(format!(
                "plugin version '{}' is not valid semver: {e}",
                self.version
            ));
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

    fn valid_manifest() -> PluginManifest {
        PluginManifest {
            name: "test-plugin".into(),
            version: "0.1.0".into(),
            description: "A test plugin".into(),
            author: String::new(),
            license: String::new(),
            homepage: String::new(),
            capabilities: vec![Capability::Evaluate],
            handles_events: vec!["plan.created".into()],
            dependencies: vec![],
            config_schema: None,
            allowed_domains: vec![],
            allowed_paths: None,
            priority: 70,
            protocol_version: None,
        }
    }

    #[test]
    fn test_valid_manifest() {
        assert!(valid_manifest().validate().is_ok());
    }

    #[test]
    fn test_empty_name_fails() {
        let mut manifest = valid_manifest();
        manifest.name = String::new();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("name")));
    }

    #[test]
    fn test_no_capabilities_or_events_fails() {
        let mut manifest = valid_manifest();
        manifest.capabilities = vec![];
        manifest.handles_events = vec![];
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_invalid_semver_fails_validation() {
        let mut manifest = valid_manifest();
        manifest.version = "not-a-version".into();
        let errors = manifest.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("not valid semver")),
            "expected semver error, got: {errors:?}"
        );
    }

    #[test]
    fn test_valid_semver_passes_validation() {
        let mut manifest = valid_manifest();
        manifest.version = "1.2.3-beta.1+build.42".into();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_protocol_version_none_defaults_to_compatible() {
        let manifest = valid_manifest();
        assert!(manifest.protocol_version.is_none());
        // None is treated as compatible — validate() does not reject it.
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_protocol_version_defaults_on_deserialize() {
        let toml_str = r#"
name = "test-plugin"
version = "1.0.0"
description = "A test plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.protocol_version.is_none());
    }

    #[test]
    fn test_manifest_serde_roundtrip() {
        let manifest = PluginManifest {
            name: "discovery".into(),
            version: "0.1.0".into(),
            description: "File discovery plugin".into(),
            author: "VOOM Contributors".into(),
            license: "MIT".into(),
            homepage: "https://github.com/voom/voom".into(),
            capabilities: vec![Capability::Discover {
                schemes: vec!["file".into()],
            }],
            handles_events: vec!["file.discovered".into()],
            dependencies: vec![PluginDependency {
                name: "storage".into(),
                version_req: ">=0.1.0".into(),
            }],
            config_schema: None,
            allowed_domains: vec![],
            allowed_paths: None,
            priority: 50,
            protocol_version: Some(1),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "discovery");
        assert_eq!(deserialized.capabilities.len(), 1);
        assert_eq!(deserialized.protocol_version, Some(1));
    }
}
