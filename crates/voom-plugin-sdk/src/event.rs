//! Event serialization/deserialization helpers for the WASM boundary.

use serde::de::DeserializeOwned;
use serde::Serialize;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::Event;

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

/// Serialize a domain Event to `MessagePack` bytes (for sending to the host).
pub fn serialize_event(event: &Event) -> Result<Vec<u8>> {
    rmp_serde::to_vec(event).map_err(|e| VoomError::Wasm(format!("failed to serialize event: {e}")))
}

/// Deserialize any JSON-compatible type from bytes.
pub fn deserialize_json<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    serde_json::from_slice(data)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize JSON: {e}")))
}

/// Serialize any type to JSON bytes.
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
///     Ok(Some(br#"{"enabled": true}"#.to_vec()))
/// }).unwrap();
/// assert!(config.unwrap().enabled);
/// ```
pub fn load_plugin_config<T: DeserializeOwned>(
    get_data: impl FnOnce(&str) -> std::result::Result<Option<Vec<u8>>, String>,
) -> Result<Option<T>> {
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
///         Ok(Some(br#"{"threshold": 10}"#.to_vec()))
///     },
/// ).unwrap();
/// assert_eq!(config.unwrap().threshold, 10);
/// ```
pub fn load_plugin_config_named<T: DeserializeOwned>(
    plugin_name: Option<&str>,
    get_data: impl FnOnce(&str) -> std::result::Result<Option<Vec<u8>>, String>,
) -> Result<Option<T>> {
    let Some(data) = get_data("config").map_err(|e| plugin_config_read_error(plugin_name, e))?
    else {
        return Ok(None);
    };

    serde_json::from_slice(&data)
        .map(Some)
        .map_err(|e| plugin_config_error(plugin_name, e))
}

fn plugin_config_error(plugin_name: Option<&str>, source: serde_json::Error) -> VoomError {
    let context = plugin_name
        .map(|name| format!(" for '{name}'"))
        .unwrap_or_default();
    VoomError::Wasm(format!(
        "failed to deserialize plugin config{context}: {source}"
    ))
}

fn plugin_config_read_error(plugin_name: Option<&str>, source: String) -> VoomError {
    let context = plugin_name
        .map(|name| format!(" for '{name}'"))
        .unwrap_or_default();
    VoomError::Wasm(format!("failed to read plugin config{context}: {source}"))
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

    #[test]
    fn test_load_plugin_config_missing_returns_none() {
        let config = load_plugin_config::<serde_json::Value>(|_| Ok(None)).unwrap();

        assert!(config.is_none());
    }

    #[test]
    fn test_load_plugin_config_valid_returns_config() {
        let config: serde_json::Value =
            load_plugin_config(|_| Ok(Some(br#"{"enabled": true}"#.to_vec())))
                .unwrap()
                .unwrap();

        assert_eq!(config["enabled"], true);
    }

    #[test]
    fn test_load_plugin_config_malformed_returns_error() {
        let error = load_plugin_config_named::<serde_json::Value>(Some("test-plugin"), |_| {
            Ok(Some(br#"{"enabled":"#.to_vec()))
        })
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("test-plugin"));
        assert!(message.contains("failed to deserialize plugin config"));
    }

    #[test]
    fn test_load_plugin_config_read_error_is_not_missing() {
        let error = load_plugin_config_named::<serde_json::Value>(Some("test-plugin"), |_| {
            Err("storage unavailable".to_string())
        })
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("test-plugin"));
        assert!(message.contains("storage unavailable"));
        assert!(message.contains("failed to read plugin config"));
    }
}
