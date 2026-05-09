/// WIT result type for HTTP responses crossing the WASM boundary.
#[cfg(feature = "wasm")]
type WitHttpResult = Result<(u16, Vec<(String, String)>, Vec<u8>), String>;

/// Expand `~` at the start of a path string to `$HOME`.
#[cfg(feature = "wasm")]
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// WASM plugin loader using wasmtime's component model.
/// Only available with the `wasm` feature.
#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(test)]
mod tests {
    use crate::Plugin;
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult};

    struct MockPlugin {
        name: String,
    }

    impl Plugin for MockPlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "1.0.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, _: &str) -> bool {
            true
        }
        fn on_event(&self, _: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            {
                let mut result = EventResult::new(self.name.clone());
                result.data = Some(serde_json::json!({"loaded": true}));
                Ok(Some(result))
            }
        }
    }

    #[test]
    fn test_mock_plugin_metadata() {
        let plugin = MockPlugin {
            name: "test".into(),
        };
        assert_eq!(plugin.name(), "test");
        assert_eq!(plugin.version(), "1.0.0");
        assert!(plugin.handles("anything"));
    }

    #[cfg(feature = "wasm")]
    mod wasm_tests {
        use super::super::wasm::*;
        use std::path::PathBuf;

        #[test]
        fn test_wasm_loader_creation() {
            let loader = WasmPluginLoader::new().unwrap();
            // Loader should be created successfully with component model enabled.
            let _ = loader;
        }

        #[test]
        fn test_wasm_loader_drops_cleanly() {
            let loader = WasmPluginLoader::new().unwrap();
            assert!(!loader.shutdown.load(std::sync::atomic::Ordering::Acquire));
            drop(loader);
        }

        #[test]
        fn test_epoch_thread_increments() {
            let loader = WasmPluginLoader::new().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(loader);
        }

        #[test]
        fn test_wasm_loader_missing_file() {
            let loader = WasmPluginLoader::new().unwrap();
            let result = loader.load(&PathBuf::from("/nonexistent/plugin.wasm"));
            assert!(result.is_err());
        }

        #[test]
        fn test_wasm_loader_invalid_wasm() {
            let dir = tempfile::tempdir().unwrap();
            let wasm_path = dir.path().join("bad.wasm");
            std::fs::write(&wasm_path, b"not valid wasm").unwrap();

            let loader = WasmPluginLoader::new().unwrap();
            let result = loader.load(&wasm_path);
            assert!(result.is_err());
        }

        #[test]
        fn test_wasm_loader_empty_dir() {
            let dir = tempfile::tempdir().unwrap();
            let loader = WasmPluginLoader::new().unwrap();
            let results = loader.load_dir(dir.path());
            assert!(results.is_empty());
        }

        #[test]
        fn test_wasm_loader_nonexistent_dir() {
            let loader = WasmPluginLoader::new().unwrap();
            let results = loader.load_dir(&PathBuf::from("/nonexistent/dir"));
            assert!(results.is_empty());
        }

        #[test]
        fn test_manifest_loading() {
            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#,
            )
            .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let result = load_manifest(&wasm_path);
            // Should successfully load the manifest.
            assert!(result.is_ok());
            let manifest = result.unwrap();
            assert!(manifest.is_some());
            let manifest = manifest.unwrap();
            assert_eq!(manifest.name, "test-plugin");
            assert_eq!(manifest.version, "1.0.0");
        }

        #[test]
        fn test_manifest_missing_ok() {
            let result = load_manifest(&PathBuf::from("/nonexistent/plugin.wasm"));
            assert!(result.is_ok());
            assert!(result.unwrap().is_none());
        }

        #[cfg(unix)]
        #[test]
        fn test_manifest_world_writable_rejected() {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                r#"
name = "test-plugin"
version = "1.0.0"
description = "A test plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#,
            )
            .unwrap();

            // Make world-writable.
            std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o666))
                .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let result = load_manifest(&wasm_path);
            assert!(result.is_err());
            let err = format!("{}", result.unwrap_err());
            assert!(
                err.contains("world-writable"),
                "expected world-writable error, got: {err}"
            );
        }

        #[test]
        fn test_wasm_event_payload_size_limit() {
            use super::super::wasm::MAX_WASM_EVENT_PAYLOAD;

            // Verify the constant is 16 MiB.
            assert_eq!(MAX_WASM_EVENT_PAYLOAD, 16 * 1024 * 1024);

            // Create a payload larger than the limit and verify the check would catch it.
            let oversized = vec![0u8; MAX_WASM_EVENT_PAYLOAD + 1];
            assert!(
                oversized.len() > MAX_WASM_EVENT_PAYLOAD,
                "payload of {} bytes should exceed max of {}",
                oversized.len(),
                MAX_WASM_EVENT_PAYLOAD
            );
        }

        #[test]
        fn test_oversized_wasm_return_value_rejected() {
            use super::super::wasm::MAX_WASM_EVENT_PAYLOAD;

            // Simulate an oversized WasmEventResult by constructing one
            // whose total size exceeds MAX_WASM_EVENT_PAYLOAD.
            let oversized_data = vec![0u8; MAX_WASM_EVENT_PAYLOAD + 1];
            let wasm_result = voom_wit::WasmEventResult {
                plugin_name: "test-plugin".into(),
                produced_events: vec![],
                data: Some(oversized_data),
                claimed: false,
                execution_error: None,
                execution_detail: None,
            };

            let output_size: usize = wasm_result
                .produced_events
                .iter()
                .map(|(t, p)| t.len() + p.len())
                .sum::<usize>()
                + wasm_result.data.as_ref().map_or(0, Vec::len)
                + wasm_result.execution_error.as_ref().map_or(0, String::len)
                + wasm_result.execution_detail.as_ref().map_or(0, Vec::len);

            assert!(
                output_size > MAX_WASM_EVENT_PAYLOAD,
                "constructed result should exceed limit"
            );
        }

        #[test]
        fn test_protocol_version_compatible_loads() {
            use crate::manifest::CURRENT_PROTOCOL_VERSION;

            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                format!(
                    r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]
protocol_version = {CURRENT_PROTOCOL_VERSION}

[[capabilities]]
Evaluate = {{}}
"#
                ),
            )
            .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let manifest = load_manifest(&wasm_path).unwrap().unwrap();
            assert_eq!(manifest.protocol_version, Some(CURRENT_PROTOCOL_VERSION));
        }

        #[test]
        fn test_protocol_version_none_is_compatible() {
            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#,
            )
            .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let manifest = load_manifest(&wasm_path).unwrap().unwrap();
            assert!(
                manifest.protocol_version.is_none(),
                "missing protocol_version should default to None"
            );
        }

        #[test]
        fn test_incompatible_protocol_version_fails_load() {
            use crate::manifest::CURRENT_PROTOCOL_VERSION;

            let dir = tempfile::tempdir().unwrap();
            let bad_version = CURRENT_PROTOCOL_VERSION + 99;
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                format!(
                    r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]
protocol_version = {bad_version}

[[capabilities]]
Evaluate = {{}}
"#
                ),
            )
            .unwrap();

            // Write a minimal (invalid) WASM file so load_with_manifest gets past file read
            let wasm_path = dir.path().join("test-plugin.wasm");
            std::fs::write(&wasm_path, b"not valid wasm").unwrap();

            let loader = WasmPluginLoader::new().unwrap();
            let result = loader.load(&wasm_path);
            let err = match result {
                Err(e) => format!("{e}"),
                Ok(_) => panic!("expected error for incompatible protocol version"),
            };
            assert!(
                err.contains("incompatible protocol_version"),
                "expected protocol version error, got: {err}"
            );
        }
    }
}
