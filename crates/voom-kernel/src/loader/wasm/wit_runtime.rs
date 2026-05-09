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

fn val_kind(val: &wasmtime::component::Val) -> &'static str {
    use wasmtime::component::Val;

    match val {
        Val::Bool(_) => "bool",
        Val::S8(_) => "s8",
        Val::U8(_) => "u8",
        Val::S16(_) => "s16",
        Val::U16(_) => "u16",
        Val::S32(_) => "s32",
        Val::U32(_) => "u32",
        Val::S64(_) => "s64",
        Val::U64(_) => "u64",
        Val::Float32(_) => "float32",
        Val::Float64(_) => "float64",
        Val::Char(_) => "char",
        Val::String(_) => "string",
        Val::List(_) => "list",
        Val::Record(_) => "record",
        Val::Tuple(_) => "tuple",
        Val::Variant(_, _) => "variant",
        Val::Enum(_) => "enum",
        Val::Option(_) => "option",
        Val::Result(_) => "result",
        Val::Flags(_) => "flags",
        Val::Resource(_) => "resource",
    }
}

fn unexpected_field_value(
    field_name: &str,
    expected: &str,
    val: &wasmtime::component::Val,
) -> WasmLoadError {
    WasmLoadError::UnexpectedValue(format!(
        "field '{field_name}' expected {expected}, got {}",
        val_kind(val)
    ))
}

fn expect_string(
    field_name: &str,
    val: &wasmtime::component::Val,
) -> Result<String, WasmLoadError> {
    match val {
        wasmtime::component::Val::String(value) => Ok(value.to_string()),
        other => Err(unexpected_field_value(field_name, "string", other)),
    }
}

fn expect_bytes(
    field_name: &str,
    val: &wasmtime::component::Val,
) -> Result<Vec<u8>, WasmLoadError> {
    let wasmtime::component::Val::List(items) = val else {
        return Err(unexpected_field_value(field_name, "list<u8>", val));
    };

    let mut bytes = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        match item {
            wasmtime::component::Val::U8(value) => bytes.push(*value),
            other => {
                return Err(WasmLoadError::UnexpectedValue(format!(
                    "field '{field_name}' expected list<u8>, got {} at index {index}",
                    val_kind(other)
                )));
            }
        }
    }
    Ok(bytes)
}

fn expect_optional_string(
    field_name: &str,
    val: &wasmtime::component::Val,
) -> Result<Option<String>, WasmLoadError> {
    match val {
        wasmtime::component::Val::Option(Some(boxed)) => {
            expect_string(field_name, boxed.as_ref()).map(Some)
        }
        wasmtime::component::Val::Option(None) => Ok(None),
        other => Err(unexpected_field_value(field_name, "option<string>", other)),
    }
}

fn expect_optional_bytes(
    field_name: &str,
    val: &wasmtime::component::Val,
) -> Result<Option<Vec<u8>>, WasmLoadError> {
    match val {
        wasmtime::component::Val::Option(Some(boxed)) => {
            expect_bytes(field_name, boxed.as_ref()).map(Some)
        }
        wasmtime::component::Val::Option(None) => Ok(None),
        other => Err(unexpected_field_value(
            field_name,
            "option<list<u8>>",
            other,
        )),
    }
}

fn parse_event_data(
    field_name: &str,
    val: &wasmtime::component::Val,
) -> Result<(String, Vec<u8>), WasmLoadError> {
    if let wasmtime::component::Val::Record(fields) = val {
        let mut evt_type = String::new();
        let mut payload = Vec::new();
        for (name, field_val) in fields {
            match name.as_str() {
                "event-type" => {
                    evt_type = expect_string(&format!("{field_name}.event-type"), field_val)?;
                }
                "payload" => {
                    payload = expect_bytes(&format!("{field_name}.payload"), field_val)?;
                }
                _ => {}
            }
        }
        Ok((evt_type, payload))
    } else {
        Err(unexpected_field_value(field_name, "record", val))
    }
}

fn expect_event_data_list(
    field_name: &str,
    val: &wasmtime::component::Val,
) -> Result<Vec<(String, Vec<u8>)>, WasmLoadError> {
    let wasmtime::component::Val::List(items) = val else {
        return Err(unexpected_field_value(field_name, "list<event-data>", val));
    };

    items
        .iter()
        .enumerate()
        .map(|(index, item)| parse_event_data(&format!("{field_name}[{index}]"), item))
        .collect()
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
            "plugin-name" => plugin_name = expect_string(name, field_val)?,
            "produced-events" => {
                produced_events = expect_event_data_list(name, field_val)?;
            }
            "data" => data = expect_optional_bytes(name, field_val)?,
            "claimed" => {
                claimed = match field_val {
                    Val::Bool(value) => *value,
                    other => return Err(unexpected_field_value(name, "bool", other)),
                };
            }
            "execution-error" => execution_error = expect_optional_string(name, field_val)?,
            "execution-detail" => execution_detail = expect_optional_bytes(name, field_val)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::component::Val;

    fn valid_event_result() -> Val {
        Val::Record(vec![
            ("plugin-name".into(), Val::String("test-plugin".into())),
            (
                "produced-events".into(),
                Val::List(vec![Val::Record(vec![
                    ("event-type".into(), Val::String("metadata.enriched".into())),
                    ("payload".into(), Val::List(vec![Val::U8(1), Val::U8(2)])),
                ])]),
            ),
            ("data".into(), Val::Option(None)),
            ("claimed".into(), Val::Bool(true)),
            (
                "execution-error".into(),
                Val::Option(Some(Box::new(Val::String("failed".into())))),
            ),
            (
                "execution-detail".into(),
                Val::Option(Some(Box::new(Val::List(vec![Val::U8(3)])))),
            ),
        ])
    }

    fn unexpected_value_message(
        result: Result<voom_wit::WasmEventResult, WasmLoadError>,
    ) -> String {
        match result {
            Ok(_) => panic!("expected UnexpectedValue error"),
            Err(WasmLoadError::UnexpectedValue(message)) => message,
            Err(other) => panic!("expected UnexpectedValue, got: {other}"),
        }
    }

    #[test]
    fn parse_event_result_accepts_valid_known_fields() {
        let result = parse_event_result(&valid_event_result()).unwrap();

        assert_eq!(result.plugin_name, "test-plugin");
        assert_eq!(
            result.produced_events,
            vec![("metadata.enriched".to_string(), vec![1, 2])]
        );
        assert!(result.claimed);
        assert_eq!(result.execution_error.as_deref(), Some("failed"));
        assert_eq!(result.execution_detail.as_deref(), Some(&[3][..]));
    }

    #[test]
    fn parse_event_result_ignores_unknown_extra_fields() {
        let Val::Record(mut fields) = valid_event_result() else {
            panic!("expected record");
        };
        fields.push(("future-field".into(), Val::Bool(false)));

        let result = parse_event_result(&Val::Record(fields)).unwrap();

        assert_eq!(result.plugin_name, "test-plugin");
    }

    #[test]
    fn parse_event_result_rejects_wrong_plugin_name_shape() {
        let Val::Record(mut fields) = valid_event_result() else {
            panic!("expected record");
        };
        fields[0] = ("plugin-name".into(), Val::Bool(false));

        let message = unexpected_value_message(parse_event_result(&Val::Record(fields)));

        assert!(message.contains("plugin-name"));
        assert!(message.contains("string"));
        assert!(message.contains("bool"));
    }

    #[test]
    fn parse_event_result_rejects_wrong_payload_item_shape() {
        let Val::Record(mut fields) = valid_event_result() else {
            panic!("expected record");
        };
        fields[1] = (
            "produced-events".into(),
            Val::List(vec![Val::Record(vec![
                ("event-type".into(), Val::String("metadata.enriched".into())),
                ("payload".into(), Val::List(vec![Val::Bool(false)])),
            ])]),
        );

        let message = unexpected_value_message(parse_event_result(&Val::Record(fields)));

        assert!(message.contains("produced-events[0].payload"));
        assert!(message.contains("list<u8>"));
        assert!(message.contains("index 0"));
    }

    #[test]
    fn parse_event_result_rejects_wrong_optional_data_shape() {
        let Val::Record(mut fields) = valid_event_result() else {
            panic!("expected record");
        };
        fields[2] = (
            "data".into(),
            Val::Option(Some(Box::new(Val::String("not bytes".into())))),
        );

        let message = unexpected_value_message(parse_event_result(&Val::Record(fields)));

        assert!(message.contains("data"));
        assert!(message.contains("list<u8>"));
        assert!(message.contains("string"));
    }
}
