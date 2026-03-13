//! Event serialization/deserialization helpers for the WASM boundary.

use serde::de::DeserializeOwned;
use serde::Serialize;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::Event;

/// Deserialize a domain Event from `MessagePack` bytes (as received from the host).
pub fn deserialize_event(payload: &[u8]) -> Result<Event> {
    rmp_serde::from_slice(payload)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize event: {e}")))
}

/// Serialize a domain Event to `MessagePack` bytes (for sending to the host).
pub fn serialize_event(event: &Event) -> Result<Vec<u8>> {
    rmp_serde::to_vec(event)
        .map_err(|e| VoomError::Wasm(format!("failed to serialize event: {e}")))
}

/// Deserialize any JSON-compatible type from bytes.
pub fn deserialize_json<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    serde_json::from_slice(data)
        .map_err(|e| VoomError::Wasm(format!("failed to deserialize JSON: {e}")))
}

/// Serialize any type to JSON bytes.
pub fn serialize_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value)
        .map_err(|e| VoomError::Wasm(format!("failed to serialize JSON: {e}")))
}

/// Load a plugin config from a `get_plugin_data("config")` provider.
///
/// This is a convenience for the common pattern of loading JSON config
/// from the host's plugin data store:
///
/// ```rust,ignore
/// let config: Option<MyConfig> = load_plugin_config(|key| host.get_plugin_data(key));
/// ```
pub fn load_plugin_config<T: DeserializeOwned>(
    get_data: impl FnOnce(&str) -> Option<Vec<u8>>,
) -> Option<T> {
    let data = get_data("config")?;
    serde_json::from_slice(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::*;

    #[test]
    fn test_event_serialize_deserialize() {
        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: PathBuf::from("/test.mkv"),
            size: 42,
            content_hash: "abc".into(),
        });

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
