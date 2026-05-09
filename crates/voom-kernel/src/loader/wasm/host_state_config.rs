use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::host::{HostState, StorageBackedPluginStore, StorageBackedTransitionStore};
use crate::manifest::PluginManifest;

pub(super) struct ManifestPluginMetadata {
    pub version: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub homepage: String,
    pub capabilities: Vec<voom_domain::capabilities::Capability>,
    pub handled_events: Vec<String>,
}

pub(super) fn plugin_name_from_manifest(
    manifest: Option<&PluginManifest>,
    wasm_path: &Path,
) -> String {
    manifest.map(|m| m.name.clone()).unwrap_or_else(|| {
        wasm_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    })
}

pub(super) fn configure_manifest_permissions(
    mut state: HostState,
    manifest: Option<&PluginManifest>,
) -> HostState {
    if let Some(manifest) = manifest {
        state.allowed_capabilities = manifest
            .capabilities
            .iter()
            .map(|c| c.kind().to_string())
            .collect();
        state.allowed_http_domains = manifest.allowed_domains.clone();
        if let Some(paths) = &manifest.allowed_paths {
            state.allowed_paths = paths
                .iter()
                .map(|p| super::super::expand_tilde(p))
                .collect();
        }
    }
    state
}

pub(super) fn host_state_from_config(plugin_name: &str, table: &toml::Table) -> HostState {
    let config_value = match serde_json::to_value(table) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                plugin = %plugin_name,
                error = %error,
                "failed to convert plugin config to JSON; using empty config"
            );
            serde_json::json!({})
        }
    };
    let mut state = HostState::new(plugin_name.to_string()).with_initial_config(config_value);

    if let Some(paths) = table.get("allowed_paths").and_then(toml::Value::as_array) {
        let paths: Vec<PathBuf> = paths
            .iter()
            .filter_map(toml::Value::as_str)
            .map(super::super::expand_tilde)
            .collect();
        state = state.with_paths(paths);
    }

    state
}

pub(super) fn attach_storage(
    host_state: &mut Option<HostState>,
    plugin_name: &str,
    storage: Option<&Arc<dyn voom_domain::storage::StorageTrait>>,
) {
    let Some(storage) = storage else {
        return;
    };

    let state = host_state.get_or_insert_with(|| HostState::new(plugin_name.to_string()));
    let plugin_store: Arc<dyn crate::host::WasmPluginStore> =
        Arc::new(StorageBackedPluginStore::new(Arc::clone(storage)));
    let transition_store: Arc<dyn crate::host::WasmTransitionStore> =
        Arc::new(StorageBackedTransitionStore::new(Arc::clone(storage)));
    state.storage = Some(plugin_store);
    state.transition_store = Some(transition_store);
}

pub(super) fn manifest_metadata(manifest: Option<&PluginManifest>) -> ManifestPluginMetadata {
    match manifest {
        Some(manifest) => ManifestPluginMetadata {
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            author: manifest.author.clone(),
            license: manifest.license.clone(),
            homepage: manifest.homepage.clone(),
            capabilities: manifest.capabilities.clone(),
            handled_events: manifest.handles_events.clone(),
        },
        None => ManifestPluginMetadata {
            version: "0.0.0".to_string(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            homepage: String::new(),
            capabilities: Vec::new(),
            handled_events: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn manifest_from_toml(toml: &str) -> PluginManifest {
        toml::from_str(toml).expect("valid manifest TOML")
    }

    #[test]
    fn explicit_empty_allowed_paths_clear_config_paths() {
        let state = HostState::new("test-plugin".into())
            .with_paths(vec![PathBuf::from("/some/config/path")]);
        assert!(!state.allowed_paths.is_empty());

        let manifest = manifest_from_toml(
            r#"
name = "test-plugin"
version = "1.0.0"
description = "test"
capabilities = []
handles_events = []
allowed_paths = []
"#,
        );
        assert!(
            manifest.allowed_paths.is_some(),
            "explicit empty array should deserialize to Some([])"
        );

        let state = configure_manifest_permissions(state, Some(&manifest));

        assert!(
            state.allowed_paths.is_empty(),
            "manifest with allowed_paths = [] must clear config-provided paths"
        );
    }

    #[test]
    fn omitted_allowed_paths_preserve_config_paths() {
        let config_path = PathBuf::from("/some/config/path");
        let state = HostState::new("test-plugin".into()).with_paths(vec![config_path.clone()]);

        let manifest = manifest_from_toml(
            r#"
name = "test-plugin"
version = "1.0.0"
description = "test"
capabilities = []
handles_events = []
"#,
        );
        assert!(
            manifest.allowed_paths.is_none(),
            "omitted field should deserialize to None"
        );

        let state = configure_manifest_permissions(state, Some(&manifest));

        assert_eq!(
            state.allowed_paths,
            vec![config_path],
            "omitted allowed_paths must preserve config-provided paths"
        );
    }
}
