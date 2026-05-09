use crate::errors::WasmLoadError;

use super::WasmPluginInner;

/// Call the on-event export of a WASM component.
///
/// Lazily instantiates the component on first call, then invokes the
/// `on-event` function from the `voom:plugin/plugin` interface.
pub(super) fn call_on_event(
    inner: &mut WasmPluginInner,
    event_type: &str,
    payload: &[u8],
) -> Result<Option<voom_wit::WasmEventResult>, WasmLoadError> {
    use wasmtime::component::Val;

    if inner.instance.is_none() {
        let instance = inner
            .linker
            .instantiate(&mut inner.store, &inner.component)
            .map_err(|e| WasmLoadError::Instantiation(e.to_string()))?;
        inner.instance = Some(instance);
    }

    let instance = inner.instance.as_ref().unwrap();

    let on_event = instance
        .get_export(&mut inner.store, None, "voom:plugin/plugin@0.3.0")
        .and_then(|idx| instance.get_export(&mut inner.store, Some(&idx), "on-event"))
        .and_then(|idx| instance.get_func(&mut inner.store, idx))
        .or_else(|| {
            instance
                .get_export(&mut inner.store, None, "voom:plugin/plugin@0.2.0")
                .and_then(|idx| instance.get_export(&mut inner.store, Some(&idx), "on-event"))
                .and_then(|idx| instance.get_func(&mut inner.store, idx))
        })
        .or_else(|| {
            let idx = instance.get_export(&mut inner.store, None, "on-event")?;
            instance.get_func(&mut inner.store, idx)
        });

    let on_event = match on_event {
        Some(func) => func,
        None => {
            tracing::warn!("WASM component has no 'on-event' export");
            return Ok(None);
        }
    };

    let event_data = Val::Record(vec![
        ("event-type".into(), Val::String(event_type.into())),
        (
            "payload".into(),
            Val::List(payload.iter().map(|b| Val::U8(*b)).collect()),
        ),
    ]);

    let mut results = vec![Val::Option(None)];

    on_event
        .call(&mut inner.store, &[event_data], &mut results)
        .map_err(|e| WasmLoadError::ComponentCall(e.to_string()))?;
    on_event
        .post_return(&mut inner.store)
        .map_err(|e| WasmLoadError::ComponentCall(e.to_string()))?;

    match &results[0] {
        Val::Option(None) => Ok(None),
        Val::Option(Some(boxed_val)) => parse_event_result(boxed_val).map(Some),
        other => Err(WasmLoadError::UnexpectedValue(format!(
            "expected Option return from on-event, got {:?}",
            std::mem::discriminant(other)
        ))),
    }
}

fn val_to_string(val: &wasmtime::component::Val) -> String {
    if let wasmtime::component::Val::String(s) = val {
        s.to_string()
    } else {
        String::new()
    }
}

fn val_to_bytes(val: &wasmtime::component::Val) -> Vec<u8> {
    if let wasmtime::component::Val::List(items) = val {
        items
            .iter()
            .filter_map(|v| {
                if let wasmtime::component::Val::U8(b) = v {
                    Some(*b)
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn parse_event_data(val: &wasmtime::component::Val) -> Option<(String, Vec<u8>)> {
    if let wasmtime::component::Val::Record(fields) = val {
        let mut evt_type = String::new();
        let mut payload = Vec::new();
        for (name, field_val) in fields {
            match name.as_str() {
                "event-type" => evt_type = val_to_string(field_val),
                "payload" => payload = val_to_bytes(field_val),
                _ => {}
            }
        }
        Some((evt_type, payload))
    } else {
        None
    }
}

fn parse_event_result(
    val: &wasmtime::component::Val,
) -> Result<voom_wit::WasmEventResult, WasmLoadError> {
    use wasmtime::component::Val;

    let fields = match val {
        Val::Record(fields) => fields,
        other => {
            return Err(WasmLoadError::UnexpectedValue(format!(
                "expected Record for event-result, got {:?}",
                std::mem::discriminant(other)
            )))
        }
    };

    let mut plugin_name = String::new();
    let mut produced_events = Vec::new();
    let mut data: Option<Vec<u8>> = None;
    let mut claimed = false;
    let mut execution_error: Option<String> = None;
    let mut execution_detail: Option<Vec<u8>> = None;

    for (name, field_val) in fields {
        match name.as_str() {
            "plugin-name" => plugin_name = val_to_string(field_val),
            "produced-events" => {
                if let Val::List(items) = field_val {
                    produced_events = items.iter().filter_map(parse_event_data).collect();
                }
            }
            "data" => match field_val {
                Val::Option(Some(boxed)) => data = Some(val_to_bytes(boxed.as_ref())),
                Val::Option(None) => data = None,
                _ => {}
            },
            "claimed" => {
                if let Val::Bool(value) = field_val {
                    claimed = *value;
                }
            }
            "execution-error" => match field_val {
                Val::Option(Some(boxed)) => {
                    execution_error = Some(val_to_string(boxed.as_ref()));
                }
                Val::Option(None) => execution_error = None,
                _ => {}
            },
            "execution-detail" => match field_val {
                Val::Option(Some(boxed)) => {
                    execution_detail = Some(val_to_bytes(boxed.as_ref()));
                }
                Val::Option(None) => execution_detail = None,
                _ => {}
            },
            _ => {}
        }
    }

    Ok(voom_wit::WasmEventResult {
        plugin_name,
        produced_events,
        data,
        claimed,
        execution_error,
        execution_detail,
    })
}
