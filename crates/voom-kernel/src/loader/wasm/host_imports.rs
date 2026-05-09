use crate::errors::WasmLoadError;
use crate::host::HostState;

pub(super) fn register_host_functions(
    linker: &mut wasmtime::component::Linker<HostState>,
) -> Result<(), WasmLoadError> {
    register_host_instance(linker, "voom:plugin/host@0.3.0")?;
    register_host_instance(linker, "voom:plugin/host@0.2.0")?;
    Ok(())
}

type HostLinkerInstance<'a> = wasmtime::component::LinkerInstance<'a, HostState>;

fn register_log_func(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "log",
            |ctx: wasmtime::StoreContextMut<'_, HostState>, (level, message): (u32, String)| {
                let level_str = match level {
                    0 => "trace",
                    1 => "debug",
                    2 => "info",
                    3 => "warn",
                    4 => "error",
                    _ => "info",
                };
                ctx.data().log(level_str, &message);
                Ok(())
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))
}

fn register_transition_funcs(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "get-file-transitions",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (file_id,): (String,)|
             -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                let uuid = uuid::Uuid::parse_str(&file_id)
                    .map_err(|e| format!("invalid file ID '{file_id}': {e}"));
                let result = match uuid {
                    Ok(id) => ctx.data().get_file_transitions(&id),
                    Err(e) => Err(e),
                };
                Ok((result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    instance
        .func_wrap(
            "get-path-transitions",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (path,): (String,)|
             -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                let result = ctx.data().get_path_transitions(&path);
                Ok((result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    Ok(())
}

fn register_plugin_data_funcs(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "get-plugin-data",
            |ctx: wasmtime::StoreContextMut<'_, HostState>, (key,): (String,)| {
                let result = ctx.data().get_plugin_data(&key);
                Ok((result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    instance
        .func_wrap(
            "set-plugin-data",
            |mut ctx: wasmtime::StoreContextMut<'_, HostState>, (key, value): (String, Vec<u8>)| {
                let result = ctx.data_mut().set_plugin_data(&key, &value);
                Ok((result.map_err(|e| e.to_string()),))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    Ok(())
}

fn register_run_tool_func(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "run-tool",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (tool, args, timeout_ms): (String, Vec<String>, u64)| {
                tracing::debug!(
                    plugin = %ctx.data().plugin_name,
                    tool = %tool,
                    args = ?args,
                    "WASM plugin requesting tool execution"
                );
                let result = ctx.data().run_tool(&tool, &args, timeout_ms);
                let wit_result: Result<(i32, Vec<u8>, Vec<u8>), String> = match result {
                    Ok(output) => Ok((output.exit_code, output.stdout, output.stderr)),
                    Err(e) => Err(e),
                };
                Ok((wit_result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))
}

fn register_http_funcs(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "http-get",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (url, headers): (String, Vec<(String, String)>)| {
                let result = ctx.data().http_get(&url, &headers);
                let wit_result: super::super::WitHttpResult = match result {
                    Ok(resp) => Ok((resp.status, resp.headers, resp.body)),
                    Err(e) => Err(e),
                };
                Ok((wit_result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    instance
        .func_wrap(
            "http-post",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (url, headers, body): (String, Vec<(String, String)>, Vec<u8>)| {
                let result = ctx.data().http_post(&url, &headers, &body);
                let wit_result: super::super::WitHttpResult = match result {
                    Ok(resp) => Ok((resp.status, resp.headers, resp.body)),
                    Err(e) => Err(e),
                };
                Ok((wit_result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    Ok(())
}

fn register_filesystem_funcs(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    register_write_file(instance)?;
    register_read_file_metadata(instance)?;
    register_list_files(instance)
}

fn register_write_file(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "write-file",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (path, content): (String, Vec<u8>)|
             -> Result<(Result<(), String>,), wasmtime::Error> {
                let result = ctx.data().write_file(&path, &content);
                Ok((result,))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))
}

fn register_read_file_metadata(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "read-file-metadata",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (path,): (String,)|
             -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                Ok((ctx.data().read_file_metadata(&path),))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))
}

fn register_list_files(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
    instance
        .func_wrap(
            "list-files",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (dir, pattern): (String, String)|
             -> Result<(Result<Vec<String>, String>,), wasmtime::Error> {
                Ok((ctx.data().list_files(&dir, &pattern),))
            },
        )
        .map_err(|e| WasmLoadError::Linker(e.to_string()))
}

fn register_host_instance(
    linker: &mut wasmtime::component::Linker<HostState>,
    instance_name: &str,
) -> Result<(), WasmLoadError> {
    let mut root = linker.root();
    let mut instance = root
        .instance(instance_name)
        .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

    register_log_func(&mut instance)?;
    register_plugin_data_funcs(&mut instance)?;
    register_run_tool_func(&mut instance)?;
    register_http_funcs(&mut instance)?;
    register_filesystem_funcs(&mut instance)?;
    register_transition_funcs(&mut instance)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use voom_domain::capabilities::Capability;

    use super::super::host_state_config::configure_manifest_permissions;
    use super::*;
    use crate::manifest::PluginManifest;

    fn state_with_paths(paths: Vec<std::path::PathBuf>) -> HostState {
        HostState::new("test-plugin".into())
            .with_paths(paths)
            .with_capabilities(HashSet::from(["discover".to_string()]))
    }

    fn canonical(path: &Path) -> std::path::PathBuf {
        std::fs::canonicalize(path).expect("test path should canonicalize")
    }

    fn manifest_with_allowed_path(path: &Path) -> PluginManifest {
        PluginManifest {
            name: "test-plugin".into(),
            version: "1.0.0".into(),
            description: "test plugin".into(),
            author: String::new(),
            license: String::new(),
            homepage: String::new(),
            capabilities: vec![Capability::Discover {
                schemes: vec!["file".into()],
            }],
            handles_events: Vec::new(),
            dependencies: Vec::new(),
            config_schema: None,
            allowed_domains: Vec::new(),
            allowed_paths: Some(vec![canonical(path).display().to_string()]),
            priority: 70,
            protocol_version: None,
        }
    }

    #[test]
    fn read_metadata_and_list_files_deny_empty_allowed_paths() {
        let state = state_with_paths(Vec::new());

        let metadata_error = state.read_file_metadata("/tmp/missing").unwrap_err();
        assert!(
            metadata_error.contains("not within allowed directories"),
            "unexpected metadata error: {metadata_error}"
        );

        let list_error = state.list_files("/tmp", "").unwrap_err();
        assert!(
            list_error.contains("not within allowed directories"),
            "unexpected list error: {list_error}"
        );
    }

    #[test]
    fn read_metadata_and_list_files_allow_canonical_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("clip.mov");
        std::fs::write(&file_path, b"video").expect("write test file");
        let state = state_with_paths(vec![canonical(dir.path())]);

        let bytes = state
            .read_file_metadata(file_path.to_str().expect("utf-8 path"))
            .expect("metadata");
        let metadata: serde_json::Value = rmp_serde::from_slice(&bytes).expect("metadata json");
        assert_eq!(metadata["size"], 5);
        assert_eq!(metadata["is_file"], true);

        let files = state
            .list_files(dir.path().to_str().expect("utf-8 path"), "clip")
            .expect("list files");
        assert_eq!(files, vec!["clip.mov"]);
    }

    #[test]
    fn read_metadata_and_list_files_reject_outside_or_missing_paths() {
        let allowed_dir = tempfile::tempdir().expect("allowed tempdir");
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let outside_file = outside_dir.path().join("outside.mov");
        std::fs::write(&outside_file, b"outside").expect("write outside file");
        let state = state_with_paths(vec![canonical(allowed_dir.path())]);

        let outside_error = state
            .read_file_metadata(outside_file.to_str().expect("utf-8 path"))
            .unwrap_err();
        assert!(
            outside_error.contains("not within allowed directories"),
            "unexpected outside path error: {outside_error}"
        );

        let missing_path = allowed_dir.path().join("missing.mov");
        let missing_error = state
            .read_file_metadata(missing_path.to_str().expect("utf-8 path"))
            .unwrap_err();
        assert!(
            missing_error.contains("cannot resolve path"),
            "unexpected missing path error: {missing_error}"
        );

        let outside_list_error = state
            .list_files(outside_dir.path().to_str().expect("utf-8 path"), "")
            .unwrap_err();
        assert!(
            outside_list_error.contains("not within allowed directories"),
            "unexpected outside list error: {outside_list_error}"
        );
    }

    #[test]
    fn manifest_allowed_paths_override_configured_paths() {
        let config_dir = tempfile::tempdir().expect("config tempdir");
        let manifest_dir = tempfile::tempdir().expect("manifest tempdir");
        let config_file = config_dir.path().join("config.mov");
        let manifest_file = manifest_dir.path().join("manifest.mov");
        std::fs::write(&config_file, b"config").expect("write config file");
        std::fs::write(&manifest_file, b"manifest").expect("write manifest file");

        let state = state_with_paths(vec![canonical(config_dir.path())]);
        let manifest = manifest_with_allowed_path(manifest_dir.path());
        let state = configure_manifest_permissions(state, Some(&manifest));

        assert!(
            state
                .list_files(manifest_dir.path().to_str().expect("utf-8 path"), "")
                .is_ok(),
            "manifest path should be allowed"
        );
        let error = state
            .read_file_metadata(config_file.to_str().expect("utf-8 path"))
            .unwrap_err();
        assert!(
            error.contains("not within allowed directories"),
            "config path should be overridden, got: {error}"
        );
    }
}
