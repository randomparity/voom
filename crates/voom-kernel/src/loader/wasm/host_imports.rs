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
                    Ok(id) => ctx
                        .data()
                        .require_capability_kind("store", "transition history access")
                        .and_then(|_| ctx.data().get_file_transitions(&id)),
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
                let result = ctx
                    .data()
                    .require_capability_kind("store", "transition history access")
                    .and_then(|_| ctx.data().get_path_transitions(&path));
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
                let result = match ctx
                    .data()
                    .require_capability_kind("store", "plugin data mutation")
                {
                    Ok(()) => ctx.data_mut().set_plugin_data(&key, &value),
                    Err(error) => Err(error),
                };
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
                let result = ctx
                    .data()
                    .require_capability_kind("serve_http", "HTTP GET")
                    .and_then(|_| ctx.data().http_get(&url, &headers));
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
                let result = ctx
                    .data()
                    .require_capability_kind("serve_http", "HTTP POST")
                    .and_then(|_| ctx.data().http_post(&url, &headers, &body));
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
                let result = ctx
                    .data()
                    .require_filesystem_capability("file writing")
                    .and_then(|_| ctx.data().write_file(&path, &content));
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
                if let Err(error) = ctx
                    .data()
                    .require_filesystem_capability("file metadata reads")
                {
                    return Ok((Err(error),));
                }
                if ctx.data().allowed_paths.is_empty() {
                    return Ok((Err(format!(
                        "path '{path}' is not within allowed directories"
                    )),));
                }

                let file_path = std::path::Path::new(&path);
                let canonical = match std::fs::canonicalize(file_path) {
                    Ok(p) => p,
                    Err(e) => {
                        return Ok((Err(format!("cannot resolve path '{path}': {e}")),));
                    }
                };
                let allowed = ctx
                    .data()
                    .allowed_paths
                    .iter()
                    .any(|p| canonical.starts_with(p));
                if !allowed {
                    return Ok((Err(format!(
                        "path '{path}' is not within allowed directories"
                    )),));
                }

                match std::fs::metadata(file_path) {
                    Ok(meta) => {
                        let info = serde_json::json!({
                            "size": meta.len(),
                            "is_file": meta.is_file(),
                            "is_dir": meta.is_dir(),
                            "readonly": meta.permissions().readonly(),
                            "modified": meta.modified().ok().map(|t| {
                                t.duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                            }),
                        });
                        let bytes = rmp_serde::to_vec(&info)
                            .map_err(|e| format!("failed to serialize metadata: {e}"));
                        Ok((bytes,))
                    }
                    Err(e) => Ok((Err(format!("failed to read metadata for '{path}': {e}")),)),
                }
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
                if let Err(error) = ctx
                    .data()
                    .require_filesystem_capability("directory listing")
                {
                    return Ok((Err(error),));
                }
                if ctx.data().allowed_paths.is_empty() {
                    return Ok((Err(format!(
                        "directory '{dir}' is not within allowed directories"
                    )),));
                }

                let dir_path = std::path::Path::new(&dir);
                let canonical = match std::fs::canonicalize(dir_path) {
                    Ok(p) => p,
                    Err(e) => {
                        return Ok((Err(format!("cannot resolve path '{dir}': {e}")),));
                    }
                };
                let allowed = ctx
                    .data()
                    .allowed_paths
                    .iter()
                    .any(|p| canonical.starts_with(p));
                if !allowed {
                    return Ok((Err(format!(
                        "directory '{dir}' is not within allowed directories"
                    )),));
                }

                match std::fs::read_dir(dir_path) {
                    Ok(entries) => {
                        let files: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                if pattern.is_empty() {
                                    true
                                } else {
                                    e.file_name().to_string_lossy().contains(&pattern)
                                }
                            })
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect();
                        Ok((Ok(files),))
                    }
                    Err(e) => Ok((Err(format!("failed to list directory '{dir}': {e}")),)),
                }
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
