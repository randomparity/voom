//! File transition lookup host functions.

use std::path::{Path, PathBuf};

use crate::host::HostState;

impl HostState {
    /// Query transitions for a file by its UUID.
    /// Returns MessagePack-serialized `Vec<FileTransition>`.
    pub fn get_file_transitions(&self, file_id: &uuid::Uuid) -> Result<Vec<u8>, String> {
        self.require_capability_kind("store", "transition history access")?;
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "file transitions are not within allowed directories for plugin '{}' \
                 (no paths configured)",
                self.plugin_name
            ));
        }
        let store = self.transition_store.as_ref().ok_or_else(|| {
            "file transition history not available \
                 (no transition store configured)"
                .to_string()
        })?;
        let transitions = store.transitions_for_file(file_id)?;
        self.check_transitions_allowed(&transitions)?;
        rmp_serde::to_vec(&transitions).map_err(|e| format!("failed to serialize transitions: {e}"))
    }

    fn check_transitions_allowed(
        &self,
        transitions: &[voom_domain::transition::FileTransition],
    ) -> Result<(), String> {
        for transition in transitions {
            self.check_transition_path_allowed(&transition.path)?;
            if let Some(from_path) = &transition.from_path {
                self.check_transition_path_allowed(from_path)?;
            }
        }
        Ok(())
    }

    fn check_transition_path_allowed(&self, path: &Path) -> Result<(), String> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let allowed = self
            .allowed_paths
            .iter()
            .any(|allowed_dir| canonical.starts_with(allowed_dir));
        if !allowed {
            return Err(format!(
                "transition path '{}' is not within allowed directories for plugin '{}'",
                path.display(),
                self.plugin_name
            ));
        }
        Ok(())
    }

    /// Query transitions for a file by its filesystem path.
    /// Returns MessagePack-serialized `Vec<FileTransition>`.
    ///
    /// Enforces the same `allowed_paths` sandbox as other filesystem-aware
    /// host functions: requires a non-empty path allowlist and verifies the
    /// query path falls within it.
    pub fn get_path_transitions(&self, path: &str) -> Result<Vec<u8>, String> {
        self.require_capability_kind("store", "transition history access")?;
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "path '{path}' is not within allowed directories for plugin '{}' \
                 (no paths configured)",
                self.plugin_name
            ));
        }
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
        let allowed = self.allowed_paths.iter().any(|p| canonical.starts_with(p));
        if !allowed {
            return Err(format!(
                "path '{path}' is not within allowed directories for plugin '{}'",
                self.plugin_name
            ));
        }
        let store = self.transition_store.as_ref().ok_or_else(|| {
            "file transition history not available \
                 (no transition store configured)"
                .to_string()
        })?;
        let path = Path::new(path);
        let transitions = store.transitions_for_path(path)?;
        rmp_serde::to_vec(&transitions).map_err(|e| format!("failed to serialize transitions: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::host::{HostState, InMemoryTransitionStore};
    use voom_domain::transition::{FileTransition, TransitionSource};

    fn state_with_capability(kind: &str) -> HostState {
        HostState::new("test".into()).with_capabilities(HashSet::from([kind.to_string()]))
    }

    #[test]
    fn test_get_file_transitions_no_store() {
        let state = state_with_capability("store").with_paths(vec![PathBuf::from("/")]);
        let result = state.get_file_transitions(&uuid::Uuid::new_v4());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[test]
    fn test_get_file_transitions_with_store() {
        let store = Arc::new(InMemoryTransitionStore::new());
        let file_id = uuid::Uuid::new_v4();
        let transition = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "hash123".into(),
            2000,
            TransitionSource::Discovery,
        );
        store.record_transition(&transition).unwrap();

        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![PathBuf::from("/movies")])
            .with_capabilities(HashSet::from(["store".to_string()]));

        let bytes = state.get_file_transitions(&file_id).unwrap();
        let transitions: Vec<FileTransition> = rmp_serde::from_slice(&bytes).expect("deserialize");
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to_hash, "hash123");
    }

    #[test]
    fn test_get_path_transitions_no_paths_configured() {
        let state = state_with_capability("store");
        let result = state.get_path_transitions("/movies/test.mkv");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no paths configured"));
    }

    #[test]
    fn test_get_path_transitions_no_store() {
        let state = state_with_capability("store").with_paths(vec![PathBuf::from("/movies")]);
        let result = state.get_path_transitions("/movies/test.mkv");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[test]
    fn test_get_path_transitions_with_store() {
        let store = Arc::new(InMemoryTransitionStore::new());
        let path = PathBuf::from("/movies/test.mkv");
        let transition = FileTransition::new(
            uuid::Uuid::new_v4(),
            path.clone(),
            "hash456".into(),
            3000,
            TransitionSource::Voom,
        );
        store.record_transition(&transition).unwrap();

        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![PathBuf::from("/movies")])
            .with_capabilities(HashSet::from(["store".to_string()]));

        let bytes = state.get_path_transitions(&path.to_string_lossy()).unwrap();
        let transitions: Vec<FileTransition> = rmp_serde::from_slice(&bytes).expect("deserialize");
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to_hash, "hash456");
    }

    #[test]
    fn test_get_file_transitions_preserves_metadata_snapshot() {
        use voom_domain::media::{Container, MediaFile, Track, TrackType};
        use voom_domain::snapshot::MetadataSnapshot;

        let store = Arc::new(InMemoryTransitionStore::new());
        let file_id = uuid::Uuid::new_v4();

        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(7200.0)
            .with_tracks(vec![
                Track::new(0, TrackType::Video, "hevc".into()),
                Track::new(1, TrackType::AudioMain, "aac".into()),
            ]);
        let snap = MetadataSnapshot::from_media_file(&file);

        let transition = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "hash789".into(),
            2_000_000,
            TransitionSource::Voom,
        )
        .with_metadata_snapshot(snap.clone());
        store.record_transition(&transition).unwrap();

        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![PathBuf::from("/movies")])
            .with_capabilities(HashSet::from(["store".to_string()]));
        let bytes = state.get_file_transitions(&file_id).unwrap();
        let transitions: Vec<FileTransition> = rmp_serde::from_slice(&bytes).expect("deserialize");
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].metadata_snapshot, Some(snap));
    }

    #[test]
    fn test_get_file_transitions_denied_by_empty_paths() {
        let store = Arc::new(InMemoryTransitionStore::new());
        let state = state_with_capability("store").with_transition_store(store);

        let result = state.get_file_transitions(&uuid::Uuid::new_v4());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no paths configured"));
    }

    #[test]
    fn test_get_file_transitions_blocked_by_allowed_paths() {
        let store = Arc::new(InMemoryTransitionStore::new());
        let file_id = uuid::Uuid::new_v4();
        let transition = FileTransition::new(
            file_id,
            PathBuf::from("/private/test.mkv"),
            "hash123".into(),
            2000,
            TransitionSource::Discovery,
        );
        store.record_transition(&transition).unwrap();

        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![PathBuf::from("/movies")])
            .with_capabilities(HashSet::from(["store".to_string()]));

        let result = state.get_file_transitions(&file_id);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not within allowed directories"));
    }

    #[test]
    fn test_get_path_transitions_blocked_by_allowed_paths() {
        let store = Arc::new(InMemoryTransitionStore::new());
        let state = HostState::new("test".into())
            .with_transition_store(store)
            .with_paths(vec![PathBuf::from("/movies")])
            .with_capabilities(HashSet::from(["store".to_string()]));

        let result = state.get_path_transitions("/etc/passwd");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not within allowed directories"));
    }

    #[test]
    fn test_get_path_transitions_denied_by_empty_paths() {
        let store = Arc::new(InMemoryTransitionStore::new());
        let state = state_with_capability("store").with_transition_store(store);

        let result = state.get_path_transitions("/movies/test.mkv");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no paths configured"));
    }
}
