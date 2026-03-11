//! Event serialization/deserialization helpers for the WASM boundary.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use voom_domain::events::Event;

/// Deserialize a domain Event from MessagePack bytes (as received from the host).
pub fn deserialize_event(payload: &[u8]) -> Result<Event> {
    rmp_serde::from_slice(payload).context("failed to deserialize event from MessagePack")
}

/// Serialize a domain Event to MessagePack bytes (for sending to the host).
pub fn serialize_event(event: &Event) -> Result<Vec<u8>> {
    rmp_serde::to_vec(event).context("failed to serialize event to MessagePack")
}

/// Deserialize any JSON-compatible type from bytes.
pub fn deserialize_json<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    serde_json::from_slice(data).context("failed to deserialize JSON from bytes")
}

/// Serialize any type to JSON bytes.
pub fn serialize_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("failed to serialize to JSON bytes")
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
