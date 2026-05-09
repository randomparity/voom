//! `SQLite` storage plugin: persistent storage for files, tracks, jobs, plans, and plugin data.

pub mod schema;
pub mod store;

mod event_handlers;

use std::sync::Arc;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_domain::storage::{EventLogRecord, EventLogStorage};
use voom_kernel::{Plugin, PluginContext};

use crate::event_handlers::handle_domain_event;
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
    fn name(&self) -> &'static str {
        "sqlite-store"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, _event_type: &str) -> bool {
        // Handles ALL events: specific types get domain-specific storage,
        // and every event is logged to the event_log table.
        true
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        let Some(store) = &self.store else {
            return Err(voom_domain::errors::VoomError::Plugin {
                plugin: "sqlite-store".into(),
                message: "store not initialized — call init() first".into(),
            });
        };

        handle_domain_event(store, event)?;
        log_event(store, event)?;

        Ok(None)
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<voom_domain::events::Event>> {
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
        Ok(vec![])
    }

    fn shutdown(&self) -> Result<()> {
        tracing::info!("sqlite store shutting down");
        Ok(())
    }
}

fn log_event(store: &SqliteStore, event: &Event) -> Result<()> {
    let payload = serde_json::to_string(event).map_err(|err| voom_domain::VoomError::Storage {
        kind: voom_domain::errors::StorageErrorKind::Other,
        message: format!("failed to serialize event log payload: {err}"),
    })?;
    let log_record = EventLogRecord::new(
        uuid::Uuid::new_v4(),
        event.event_type().to_string(),
        payload,
        event.summary(),
    );
    store.insert_event_log(&log_record)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::storage::PluginDataStorage;

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
        assert!(plugin.handles(Event::FILE_DISCOVERED));
        assert!(plugin.handles(Event::FILE_INTROSPECTED));
        assert!(plugin.handles(Event::FILE_INTROSPECTION_FAILED));
        assert!(plugin.handles(Event::PLAN_CREATED));
        assert!(plugin.handles(Event::PLAN_COMPLETED));
        assert!(plugin.handles(Event::PLAN_FAILED));
        assert!(plugin.handles(Event::METADATA_ENRICHED));
        assert!(plugin.handles(Event::TOOL_DETECTED));
        assert!(plugin.handles(Event::HEALTH_STATUS));
    }

    #[test]
    fn test_handles_executor_capabilities() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.handles(Event::EXECUTOR_CAPABILITIES));
    }

    #[test]
    fn test_handles_plan_skipped() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.handles(Event::PLAN_SKIPPED));
    }

    #[test]
    fn test_handles_all_event_types() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.handles(Event::JOB_STARTED));
        assert!(plugin.handles(Event::JOB_PROGRESS));
        assert!(plugin.handles(Event::JOB_COMPLETED));
        assert!(plugin.handles(Event::JOB_ENQUEUE_REQUESTED));
        assert!(plugin.handles(Event::PLAN_EXECUTING));
        assert!(plugin.handles(Event::PLUGIN_ERROR));
        assert!(plugin.handles("unknown.event"));
    }

    #[test]
    fn test_on_event_returns_error_when_store_not_initialized() {
        let plugin = SqliteStorePlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffprobe",
            "6.0",
            "/usr/bin/ffprobe".into(),
        ));
        let result = plugin.on_event(&event);
        assert!(result.is_err());
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
    fn test_on_event_handles_file_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            "/media/test.mkv".into(),
            1_500_000_000,
            Some("abc123def456".into()),
        ));
        plugin.on_event(&event).unwrap();

        // Verify discovered file was stored
        let store = plugin.store().unwrap();
        let files = store.list_discovered_files(None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/media/test.mkv");
        assert_eq!(files[0].size, 1_500_000_000);
    }

    #[test]
    fn test_on_event_handles_executor_capabilities() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        let event =
            Event::ExecutorCapabilities(voom_domain::events::ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                voom_domain::events::CodecCapabilities::new(
                    vec!["h264".into(), "hevc".into()],
                    vec!["libx264".into()],
                ),
                vec!["matroska".into(), "mp4".into()],
                vec!["videotoolbox".into()],
            ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());

        // Verify data was stored
        let store = plugin.store().unwrap();
        let data = store
            .plugin_data("ffmpeg-executor", "executor_capabilities:ffmpeg-executor")
            .unwrap();
        assert!(data.is_some());
        let value: serde_json::Value = serde_json::from_slice(&data.unwrap()).unwrap();
        assert_eq!(value["plugin_name"], "ffmpeg-executor");
        assert_eq!(value["codecs"]["decoders"][0], "h264");
        assert_eq!(value["formats"][0], "matroska");
        assert_eq!(value["hw_accels"][0], "videotoolbox");
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

    #[test]
    fn test_handles_subtitle_generated() {
        let plugin = SqliteStorePlugin::new();
        assert!(plugin.handles(Event::SUBTITLE_GENERATED));
    }

    #[test]
    fn test_on_event_handles_subtitle_generated() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plugin = SqliteStorePlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, tmp.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        let event = Event::SubtitleGenerated(voom_domain::events::SubtitleGeneratedEvent::new(
            "/media/movie.mkv".into(),
            "/media/movie.forced-eng.srt".into(),
            "eng",
            true,
        ));
        plugin.on_event(&event).unwrap();

        let store = plugin.store().unwrap();
        let records = store.list_subtitles("/media/movie.mkv").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].language, "eng");
        assert!(records[0].forced);
    }
}
