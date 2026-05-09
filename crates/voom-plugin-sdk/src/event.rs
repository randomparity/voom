//! Event serialization/deserialization helpers for the WASM boundary.

use serde::de::DeserializeOwned;
use serde::Serialize;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::Event;

use crate::host::HostFunctions;

/// Deserialize a domain Event from `MessagePack` bytes (as received from the host).
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use voom_plugin_sdk::{serialize_event, deserialize_event};
/// use voom_domain::events::{Event, FileDiscoveredEvent};
///
/// let event = Event::FileDiscovered(FileDiscoveredEvent::new(
///     PathBuf::from("/test.mkv"), 42, None,
/// ));
/// let bytes = serialize_event(&event).unwrap();
/// let restored = deserialize_event(&bytes).unwrap();
/// assert_eq!(restored.event_type(), "file.discovered");
/// ```
pub fn deserialize_event(payload: &[u8]) -> Result<Event> {
    rmp_serde::from_slice(payload)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize event: {e}")))
}

pub fn serialize_event(event: &Event) -> Result<Vec<u8>> {
    rmp_serde::to_vec(event).map_err(|e| VoomError::Wasm(format!("failed to serialize event: {e}")))
}

pub fn deserialize_event_or_log(payload: &[u8], host: &dyn HostFunctions) -> Option<Event> {
    deserialize_event(payload)
        .map_err(|e| {
            host.log("error", &format!("failed to deserialize event: {e}"));
        })
        .ok()
}

pub fn serialize_event_or_log(event: &Event, host: &dyn HostFunctions) -> Option<Vec<u8>> {
    serialize_event(event)
        .map_err(|e| {
            host.log("error", &format!("failed to serialize event: {e}"));
        })
        .ok()
}

pub fn deserialize_json<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    serde_json::from_slice(data)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize JSON: {e}")))
}

pub fn serialize_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| VoomError::Wasm(format!("failed to serialize JSON: {e}")))
}

/// Load a plugin config from a `get_plugin_data("config")` provider.
///
/// This is a convenience for the common pattern of loading JSON config
/// from the host's plugin data store.
///
/// # Examples
///
/// ```
/// use serde::Deserialize;
/// use voom_plugin_sdk::load_plugin_config;
///
/// #[derive(Deserialize)]
/// struct MyConfig {
///     enabled: bool,
/// }
///
/// let config: Option<MyConfig> = load_plugin_config(|key| {
///     assert_eq!(key, "config");
///     Some(br#"{"enabled": true}"#.to_vec())
/// });
/// assert!(config.unwrap().enabled);
/// ```
pub fn load_plugin_config<T: DeserializeOwned>(
    get_data: impl FnOnce(&str) -> Option<Vec<u8>>,
) -> Option<T> {
    load_plugin_config_named(None, get_data)
}

/// Like [`load_plugin_config`], but includes the plugin name in the warning
/// log when deserialization fails.
///
/// # Examples
///
/// ```
/// use serde::Deserialize;
/// use voom_plugin_sdk::load_plugin_config_named;
///
/// #[derive(Deserialize)]
/// struct MyConfig {
///     threshold: u32,
/// }
///
/// let config: Option<MyConfig> = load_plugin_config_named(
///     Some("my-plugin"),
///     |key| {
///         assert_eq!(key, "config");
///         Some(br#"{"threshold": 10}"#.to_vec())
///     },
/// );
/// assert_eq!(config.unwrap().threshold, 10);
/// ```
pub fn load_plugin_config_named<T: DeserializeOwned>(
    plugin_name: Option<&str>,
    get_data: impl FnOnce(&str) -> Option<Vec<u8>>,
) -> Option<T> {
    let data = get_data("config")?;
    match serde_json::from_slice(&data) {
        Ok(config) => Some(config),
        Err(e) => {
            if let Some(name) = plugin_name {
                tracing::warn!("Failed to deserialize plugin config for '{name}': {e}");
            } else {
                tracing::warn!("Failed to deserialize plugin config: {e}");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::*;

    #[test]
    fn test_event_serialize_deserialize() {
        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/test.mkv"),
            42,
            Some("abc".into()),
        ));

        let bytes = serialize_event(&event).unwrap();
        let restored = deserialize_event(&bytes).unwrap();
        assert_eq!(restored.event_type(), "file.discovered");
    }

    #[test]
    fn test_json_serialize_deserialize() {
        let value = serde_json::json!({"key": "value", "num": 42});
        let bytes = serialize_json(&value).unwrap();
        let restored: serde_json::Value = deserialize_json(&bytes).unwrap();
        assert_eq!(restored["key"], "value");
        assert_eq!(restored["num"], 42);
    }

    #[test]
    fn test_deserialize_invalid_bytes() {
        assert!(deserialize_event(&[0xFF, 0xFE]).is_err());
        assert!(deserialize_json::<serde_json::Value>(&[0xFF]).is_err());
    }
}
