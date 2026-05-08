//! Domain event persistence handlers for the `SQLite` storage plugin.

use voom_domain::errors::Result;
use voom_domain::events::Event;
use voom_domain::storage::{
    BadFileStorage, FileStorage, HealthCheckRecord, HealthCheckStorage, PendingOpsStorage,
    PlanStorage, PluginDataStorage,
};

use crate::store::SqliteStore;

pub(crate) fn handle_domain_event(store: &SqliteStore, event: &Event) -> Result<()> {
    match event {
        Event::FileDiscovered(e) => handle_file_discovered(store, e)?,
        Event::FileIntrospected(e) => handle_file_introspected(store, e)?,
        Event::FileIntrospectionFailed(e) => handle_file_introspection_failed(store, e)?,
        Event::PlanExecuting(e) => handle_plan_executing(store, e),
        Event::PlanCreated(e) => handle_plan_created(store, e)?,
        Event::PlanCompleted(e) => handle_plan_completed(store, e)?,
        Event::PlanSkipped(e) => handle_plan_skipped(store, e)?,
        Event::PlanFailed(e) => handle_plan_failed(store, e)?,
        Event::MetadataEnriched(e) => handle_metadata_enriched(store, e)?,
        Event::ToolDetected(e) => handle_tool_detected(store, e)?,
        Event::ExecutorCapabilities(e) => handle_executor_capabilities(store, e)?,
        Event::HealthStatus(e) => handle_health_status(store, e)?,
        Event::SubtitleGenerated(e) => handle_subtitle_generated(store, e)?,
        _ => {
            tracing::trace!(
                event_type = event.event_type(),
                "no domain-specific handler"
            );
        }
    }

    Ok(())
}

fn handle_file_discovered(
    store: &SqliteStore,
    e: &voom_domain::events::FileDiscoveredEvent,
) -> Result<()> {
    let path_str = e.path.to_string_lossy();
    store.upsert_discovered_file(&path_str, e.size, e.content_hash.as_deref())?;
    tracing::info!(path = %e.path.display(), "stored discovered file");
    Ok(())
}

fn handle_file_introspected(
    store: &SqliteStore,
    e: &voom_domain::events::FileIntrospectedEvent,
) -> Result<()> {
    store.upsert_file(&e.file)?;
    store.delete_bad_file_by_path(&e.file.path)?;
    tracing::info!(path = %e.file.path.display(), "stored introspected file");
    Ok(())
}

fn handle_file_introspection_failed(
    store: &SqliteStore,
    e: &voom_domain::events::FileIntrospectionFailedEvent,
) -> Result<()> {
    let bad_file = voom_domain::bad_file::BadFile::new(
        e.path.clone(),
        e.size,
        e.content_hash.clone(),
        e.error.clone(),
        e.error_source,
    );
    store.upsert_bad_file(&bad_file)?;
    tracing::info!(path = %e.path.display(), error = %e.error, "stored bad file");
    Ok(())
}

fn handle_plan_executing(store: &SqliteStore, e: &voom_domain::events::PlanExecutingEvent) {
    let op = voom_domain::storage::PendingOperation {
        id: e.plan_id,
        file_path: e.path.clone(),
        phase_name: e.phase_name.clone(),
        started_at: chrono::Utc::now(),
    };
    if let Err(err) = store.insert_pending_op(&op) {
        tracing::warn!(
            error = %err,
            plan_id = %e.plan_id,
            "failed to insert pending operation"
        );
    }
}

// Plan is recorded with status='pending' here; PlanCompleted/Skipped/Failed
// drive the subsequent status update via update_plan_status(). See
// PRIORITY_STORAGE in crates/voom-cli/src/app.rs for the ordering rationale.
fn handle_plan_created(
    store: &SqliteStore,
    e: &voom_domain::events::PlanCreatedEvent,
) -> Result<()> {
    let plan_id = store.save_plan(&e.plan)?;
    tracing::info!(%plan_id, "stored plan");
    Ok(())
}

fn handle_plan_completed(
    store: &SqliteStore,
    e: &voom_domain::events::PlanCompletedEvent,
) -> Result<()> {
    tracing::info!(path = %e.path.display(), phase = %e.phase_name, "plan completed");
    store.update_plan_status(&e.plan_id, voom_domain::storage::PlanStatus::Completed)?;
    if let Err(err) = store.delete_pending_op(&e.plan_id) {
        tracing::warn!(
            error = %err,
            plan_id = %e.plan_id,
            "failed to delete pending operation on completion"
        );
    }
    Ok(())
}

fn handle_plan_skipped(
    store: &SqliteStore,
    e: &voom_domain::events::PlanSkippedEvent,
) -> Result<()> {
    tracing::info!(
        path = %e.path.display(),
        phase = %e.phase_name,
        reason = %e.skip_reason,
        "plan skipped"
    );
    store.update_plan_status(&e.plan_id, voom_domain::storage::PlanStatus::Skipped)?;
    Ok(())
}

fn handle_plan_failed(store: &SqliteStore, e: &voom_domain::events::PlanFailedEvent) -> Result<()> {
    tracing::info!(path = %e.path.display(), phase = %e.phase_name, error = %e.error, "plan failed");
    store.update_plan_status(&e.plan_id, voom_domain::storage::PlanStatus::Failed)?;
    store.update_plan_error(&e.plan_id, &e.error, e.execution_detail.as_ref())?;
    if let Err(err) = store.delete_pending_op(&e.plan_id) {
        tracing::warn!(
            error = %err,
            plan_id = %e.plan_id,
            "failed to delete pending operation on failure"
        );
    }
    Ok(())
}

fn handle_metadata_enriched(
    store: &SqliteStore,
    e: &voom_domain::events::MetadataEnrichedEvent,
) -> Result<()> {
    let key = format!("metadata:{}", e.path.display());
    let value = serde_json::to_vec(&e.metadata).map_err(|err| voom_domain::VoomError::Storage {
        kind: voom_domain::errors::StorageErrorKind::Other,
        message: format!("failed to serialize enriched metadata: {err}"),
    })?;
    store.set_plugin_data(&e.source, &key, &value)?;
    tracing::info!(
        path = %e.path.display(),
        source = %e.source,
        "stored enriched metadata"
    );
    Ok(())
}

fn handle_tool_detected(
    store: &SqliteStore,
    e: &voom_domain::events::ToolDetectedEvent,
) -> Result<()> {
    let key = format!("tool:{}", e.tool_name);
    let value = serde_json::json!({
        "tool_name": e.tool_name,
        "version": e.version,
        "path": e.path,
    });
    let bytes = serde_json::to_vec(&value).map_err(|err| voom_domain::VoomError::Storage {
        kind: voom_domain::errors::StorageErrorKind::Other,
        message: format!("failed to serialize tool info: {err}"),
    })?;
    store.set_plugin_data("tool-detector", &key, &bytes)?;
    tracing::info!(
        tool = %e.tool_name,
        version = %e.version,
        "stored detected tool"
    );
    Ok(())
}

fn handle_executor_capabilities(
    store: &SqliteStore,
    e: &voom_domain::events::ExecutorCapabilitiesEvent,
) -> Result<()> {
    let key = format!("executor_capabilities:{}", e.plugin_name);
    let bytes = serde_json::to_vec(e).map_err(|err| voom_domain::VoomError::Storage {
        kind: voom_domain::errors::StorageErrorKind::Other,
        message: format!("failed to serialize executor capabilities: {err}"),
    })?;
    store.set_plugin_data(&e.plugin_name, &key, &bytes)?;
    tracing::info!(
        plugin = %e.plugin_name,
        codecs_decoders = e.codecs.decoders.len(),
        codecs_encoders = e.codecs.encoders.len(),
        formats = e.formats.len(),
        hw_accels = e.hw_accels.len(),
        "stored executor capabilities"
    );
    Ok(())
}

fn handle_health_status(
    store: &SqliteStore,
    e: &voom_domain::events::HealthStatusEvent,
) -> Result<()> {
    let record = HealthCheckRecord::new(&e.check_name, e.passed, e.details.clone());
    store.insert_health_check(&record)?;
    if e.passed {
        tracing::info!(
            check = %e.check_name,
            "stored health check (passed)"
        );
    } else {
        tracing::warn!(
            check = %e.check_name,
            details = ?e.details,
            "stored health check (FAILED)"
        );
    }
    Ok(())
}

fn handle_subtitle_generated(
    store: &SqliteStore,
    e: &voom_domain::events::SubtitleGeneratedEvent,
) -> Result<()> {
    store.upsert_subtitle(
        &e.path.to_string_lossy(),
        &e.subtitle_path.to_string_lossy(),
        &e.language,
        e.forced,
        e.title.as_deref(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteStore::open(&dir.path().join("voom.db")).expect("open sqlite store");
        (dir, store)
    }

    #[test]
    fn handle_domain_event_persists_discovered_file() {
        let (_dir, store) = temp_store();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            "/media/movie.mkv".into(),
            1024,
            Some("abc123".into()),
        ));

        handle_domain_event(&store, &event).expect("persist discovered file");

        let files = store
            .list_discovered_files(None)
            .expect("list discovered files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/media/movie.mkv");
        assert_eq!(files[0].content_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn handle_domain_event_persists_subtitle_generated() {
        let (_dir, store) = temp_store();
        let event = Event::SubtitleGenerated(voom_domain::events::SubtitleGeneratedEvent::new(
            "/media/movie.mkv".into(),
            "/media/movie.eng.srt".into(),
            "eng",
            false,
        ));

        handle_domain_event(&store, &event).expect("persist subtitle");

        let subtitles = store
            .list_subtitles("/media/movie.mkv")
            .expect("list subtitles");
        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].subtitle_path, "/media/movie.eng.srt");
        assert_eq!(subtitles[0].language, "eng");
        assert!(!subtitles[0].forced);
    }
}
