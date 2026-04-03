use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::storage::FileTransitionStorage;
use voom_domain::transition::{FileTransition, TransitionSource};

use super::SqliteStore;

impl FileTransitionStorage for SqliteStore {
    fn record_transition(&self, _transition: &FileTransition) -> Result<()> {
        // Stub: real implementation in Task 3 (schema migration adds file_transitions table)
        Ok(())
    }

    fn transitions_for_file(&self, _file_id: &Uuid) -> Result<Vec<FileTransition>> {
        // Stub: real implementation in Task 3
        Ok(Vec::new())
    }

    fn transitions_by_source(&self, _source: TransitionSource) -> Result<Vec<FileTransition>> {
        // Stub: real implementation in Task 3
        Ok(Vec::new())
    }
}
