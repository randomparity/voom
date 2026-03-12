pub mod schema;
pub mod store;

use std::sync::Arc;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_domain::storage::StorageTrait;
use voom_kernel::{Plugin, PluginContext};

use crate::store::SqliteStore;

/// The SQLite storage plugin. Persists media files, jobs, plans, and stats.
pub struct SqliteStorePlugin {
    store: Option<Arc<SqliteStore>>,
    capabilities: Vec<Capability>,
}

impl SqliteStorePlugin {
    pub fn new() -> Self {
        Self {
            store: None,
            capabilities: vec![Capability::Store {
                backend: "sqlite".to_string(),
            }],
        }
    }

    /// Get a reference to the underlying store. Returns None if not initialized.
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
            "file.introspected" | "plan.created" | "plan.completed" | "plan.failed"
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
                tracing::info!(path = %e.file.path.display(), "stored introspected file");
            }
            Event::PlanCreated(e) => {
                let plan_id = store.save_plan(&e.plan)?;
                tracing::info!(%plan_id, "stored plan");
            }
            Event::PlanCompleted(e) => {
                tracing::info!(path = %e.path.display(), phase = %e.phase_name, "plan completed");
                store.update_plan_status(&e.plan_id, "completed")?;
            }
            Event::PlanFailed(e) => {
                tracing::info!(path = %e.path.display(), phase = %e.phase_name, error = %e.error, "plan failed");
                store.update_plan_status(&e.plan_id, "failed")?;
            }
            _ => {}
        }

        Ok(None)
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<()> {
        let db_path = ctx.data_dir.join("voom.db");

        // Ensure data directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                voom_domain::VoomError::Storage(format!(
                    "failed to create data dir {}: {e}",
                    parent.display()
                ))
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
