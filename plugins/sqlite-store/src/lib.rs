//! `SQLite` storage plugin: persistent storage for files, tracks, jobs, plans, and plugin data.

#![allow(clippy::missing_errors_doc)]

pub mod schema;
pub mod store;

use std::sync::Arc;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_domain::storage::{BadFileStorage, FileStorage, PlanStorage, PluginDataStorage};
use voom_kernel::{Plugin, PluginContext};

use crate::store::SqliteStore;

/// The `SQLite` storage plugin. Persists media files, jobs, plans, and stats.
pub struct SqliteStorePlugin {
    store: Option<Arc<SqliteStore>>,
    capabilities: Vec<Capability>,
}

impl SqliteStorePlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: None,
            capabilities: vec![Capability::Store {
                backend: "sqlite".to_string(),
            }],
        }
    }

    /// Get a reference to the underlying store. Returns None if not initialized.
    #[must_use]
    pub fn store(&self) -> Option<&Arc<SqliteStore>> {
        self.store.as_ref()
    }
}

impl Default for SqliteStorePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for SqliteStorePlugin {
    fn name(&self) -> &str {
        "sqlite-store"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        matches!(
            event_type,
            Event::FILE_INTROSPECTED
                | Event::FILE_INTROSPECTION_FAILED
                | Event::PLAN_CREATED
                | Event::PLAN_COMPLETED
                | Event::PLAN_FAILED
                | Event::METADATA_ENRICHED
                | Event::TOOL_DETECTED
        )
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        let store = match &self.store {
            Some(s) => s,
            None => return Ok(None),
        };

        match event {
            Event::FileIntrospected(e) => {
                store.upsert_file(&e.file)?;
                // Auto-clear any bad file entry when introspection succeeds
                store.delete_bad_file_by_path(&e.file.path)?;
                tracing::info!(path = %e.file.path.display(), "stored introspected file");
            }
            Event::FileIntrospectionFailed(e) => {
                let bad_file = voom_domain::bad_file::BadFile::new(
                    e.path.clone(),
                    e.size,
                    e.content_hash.clone(),
                    e.error.clone(),
                    e.error_source,
                );
                store.upsert_bad_file(&bad_file)?;
                tracing::info!(path = %e.path.display(), error = %e.error, "stored bad file");
            }
            Event::PlanCreated(e) => {
                let plan_id = store.save_plan(&e.plan)?;
                tracing::info!(%plan_id, "stored plan");
            }
            Event::PlanCompleted(e) => {
                tracing::info!(path = %e.path.display(), phase = %e.phase_name, "plan completed");
                store
                    .update_plan_status(&e.plan_id, voom_domain::storage::PlanStatus::Completed)?;
            }
            Event::PlanFailed(e) => {
                tracing::info!(path = %e.path.display(), phase = %e.phase_name, error = %e.error, "plan failed");
                store.update_plan_status(&e.plan_id, voom_domain::storage::PlanStatus::Failed)?;
            }
            Event::MetadataEnriched(e) => {
                let key = format!("metadata:{}", e.path.display());
                let value = serde_json::to_vec(&e.metadata).map_err(|err| {
                    voom_domain::VoomError::Storage {
                        kind: voom_domain::errors::StorageErrorKind::Other,
                        message: format!("failed to serialize enriched metadata: {err}"),
                    }
                })?;
                store.set_plugin_data(&e.source, &key, &value)?;
                tracing::info!(
                    path = %e.path.display(),
                    source = %e.source,
                    "stored enriched metadata"
                );
            }
            Event::ToolDetected(e) => {
                let key = format!("tool:{}", e.tool_name);
                let value = serde_json::json!({
                    "tool_name": e.tool_name,
                    "version": e.version,
                    "path": e.path,
                });
                let bytes =
                    serde_json::to_vec(&value).map_err(|err| voom_domain::VoomError::Storage {
                        kind: voom_domain::errors::StorageErrorKind::Other,
                        message: format!("failed to serialize tool info: {err}"),
                    })?;
                store.set_plugin_data("tool-detector", &key, &bytes)?;
                tracing::info!(
                    tool = %e.tool_name,
                    version = %e.version,
                    "stored detected tool"
                );
            }
            _ => {}
        }

        Ok(None)
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<()> {
        let db_path = ctx.data_dir.join("voom.db");

        // Ensure data directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| voom_domain::VoomError::Storage {
                kind: voom_domain::errors::StorageErrorKind::Other,
                message: format!("failed to create data dir {}: {e}", parent.display()),
            })?;
        }

        let sqlite_store = SqliteStore::open(&db_path)?;
        self.store = Some(Arc::new(sqlite_store));
        tracing::info!(path = %db_path.display(), "sqlite store initialized");
        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        tracing::info!("sqlite store shutting down");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_plugin_with_store_capability() {
        let plugin = SqliteStorePlugin::new();
        assert_eq!(
            plugin.capabilities(),
            &[Capability::Store {
                backend: "sqlite".to_string()
            }]
        );
    }

    #[test]
    fn test_default_creates_same_as_new() {
        let plugin = SqliteStorePlugin::default();
        assert_eq!(plugin.name(), "sqlite-store");
        assert_eq!(
            plugin.capabilities(),
            &[Capability::Store {
                backend: "sqlite".to_string()
            }]
        );
    }

    #[test]
    fn test_plugin_name_and_version() {
        let plugin = SqliteStorePlugin::new();
        assert_eq!(plugin.name(), "sqlite-store");
        assert!(!plugin.version().is_empty());
    }

    #[test]
    fn test_store_is_none_before_init() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.store().is_none());
    }

    #[test]
    fn test_handles_expected_event_types() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.handles(Event::FILE_INTROSPECTED));
        assert!(plugin.handles(Event::FILE_INTROSPECTION_FAILED));
        assert!(plugin.handles(Event::PLAN_CREATED));
        assert!(plugin.handles(Event::PLAN_COMPLETED));
        assert!(plugin.handles(Event::PLAN_FAILED));
        assert!(plugin.handles(Event::METADATA_ENRICHED));
        assert!(plugin.handles(Event::TOOL_DETECTED));
    }

    #[test]
    fn test_does_not_handle_unrelated_event_types() {
        let plugin = SqliteStorePlugin::new();
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::JOB_STARTED));
        assert!(!plugin.handles(""));
    }

    #[test]
    fn test_on_event_returns_none_when_store_not_initialized() {
        let plugin = SqliteStorePlugin::new();
        // Even for a handled event type, returns None if store is not init'd
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffprobe",
            "6.0",
            "/usr/bin/ffprobe".into(),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_init_creates_store() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();
        assert!(plugin.store().is_some());
    }

    #[test]
    fn test_init_creates_data_dir_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("nested").join("dir");
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, nested);
        plugin.init(&ctx).unwrap();
        assert!(plugin.store().is_some());
    }

    #[test]
    fn test_shutdown_succeeds() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.shutdown().is_ok());
    }

    #[test]
    fn test_on_event_handles_introspection_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        let event =
            Event::FileIntrospectionFailed(voom_domain::events::FileIntrospectionFailedEvent::new(
                "/media/corrupt.mkv".into(),
                2048,
                Some("abc123".into()),
                "ffprobe failed".into(),
                voom_domain::bad_file::BadFileSource::Introspection,
            ));
        plugin.on_event(&event).unwrap();

        // Verify bad file was stored
        let store = plugin.store().unwrap();
        use voom_domain::storage::BadFileStorage;
        let bf = store
            .bad_file_by_path(std::path::Path::new("/media/corrupt.mkv"))
            .unwrap();
        assert!(bf.is_some());
        assert_eq!(bf.unwrap().error, "ffprobe failed");
    }

    #[test]
    fn test_on_event_introspected_clears_bad_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        // First mark file as bad
        let fail_event =
            Event::FileIntrospectionFailed(voom_domain::events::FileIntrospectionFailedEvent::new(
                "/media/recovered.mkv".into(),
                2048,
                Some("abc123".into()),
                "ffprobe failed".into(),
                voom_domain::bad_file::BadFileSource::Introspection,
            ));
        plugin.on_event(&fail_event).unwrap();

        // Then successfully introspect it
        let file =
            voom_domain::media::MediaFile::new(std::path::PathBuf::from("/media/recovered.mkv"));
        let success_event =
            Event::FileIntrospected(voom_domain::events::FileIntrospectedEvent::new(file));
        plugin.on_event(&success_event).unwrap();

        // Bad file entry should be cleared
        let store = plugin.store().unwrap();
        use voom_domain::storage::BadFileStorage;
        let bf = store
            .bad_file_by_path(std::path::Path::new("/media/recovered.mkv"))
            .unwrap();
        assert!(bf.is_none());
    }

    #[test]
    fn test_on_event_with_initialized_store_handles_tool_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffprobe",
            "6.0",
            "/usr/bin/ffprobe".into(),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none()); // on_event always returns None for store

        // Verify data was stored
        let store = plugin.store().unwrap();
        let data = store.plugin_data("tool-detector", "tool:ffprobe").unwrap();
        assert!(data.is_some());
        let value: serde_json::Value = serde_json::from_slice(&data.unwrap()).unwrap();
        assert_eq!(value["tool_name"], "ffprobe");
        assert_eq!(value["version"], "6.0");
    }
}
