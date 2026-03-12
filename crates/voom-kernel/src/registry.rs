use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::Plugin;

/// Stores all registered plugins and enables capability-based lookup.
pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, Arc<dyn Plugin>>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
        }
    }

    /// Register a plugin by name.
    pub fn register(&self, plugin: Arc<dyn Plugin>) {
        let name = plugin.name().to_string();
        self.plugins.write().insert(name, plugin);
    }

    /// Look up a plugin by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.read().get(name).cloned()
    }

    /// Find all plugins that have a capability of the given kind.
    pub fn find_by_capability_kind(&self, kind: &str) -> Vec<Arc<dyn Plugin>> {
        let plugins = self.plugins.read();
        plugins
            .values()
            .filter(|p| p.capabilities().iter().any(|c| c.kind() == kind))
            .cloned()
            .collect()
    }

    /// Find the best plugin for an operation on a given format.
    /// Returns the first matching plugin (arbitrary if multiple match).
    pub fn find_for_operation(&self, operation: &str, format: &str) -> Option<Arc<dyn Plugin>> {
        let plugins = self.plugins.read();
        plugins
            .values()
            .find(|p| {
                p.capabilities()
                    .iter()
                    .any(|c| c.supports_operation(operation) && c.supports_format(format))
            })
            .cloned()
    }

    /// Returns the names of all registered plugins.
    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.read().keys().cloned().collect()
    }

    /// Returns the total number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult};

    struct FakePlugin {
        name: String,
        caps: Vec<Capability>,
    }

    impl Plugin for FakePlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &self.caps
        }
        fn handles(&self, _event_type: &str) -> bool {
            false
        }
        fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            Ok(None)
        }
    }

    #[test]
    fn test_register_and_get() {
        let registry = PluginRegistry::new();
        let plugin = Arc::new(FakePlugin {
            name: "test-plugin".into(),
            caps: vec![],
        });
        registry.register(plugin);

        assert!(registry.get("test-plugin").is_some());
        assert!(registry.get("nonexistent").is_none());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_find_by_capability() {
        let registry = PluginRegistry::new();

        let executor = Arc::new(FakePlugin {
            name: "mkvtoolnix".into(),
            caps: vec![Capability::Execute {
                operations: vec!["metadata".into(), "reorder".into(), "remux".into()],
                formats: vec!["mkv".into()],
            }],
        });
        let discovery = Arc::new(FakePlugin {
            name: "discovery".into(),
            caps: vec![Capability::Discover {
                schemes: vec!["file".into()],
            }],
        });

        registry.register(executor);
        registry.register(discovery);

        let executors = registry.find_by_capability_kind("execute");
        assert_eq!(executors.len(), 1);
        assert_eq!(executors[0].name(), "mkvtoolnix");

        let discoverers = registry.find_by_capability_kind("discover");
        assert_eq!(discoverers.len(), 1);
    }

    #[test]
    fn test_find_for_operation() {
        let registry = PluginRegistry::new();

        let mkv = Arc::new(FakePlugin {
            name: "mkvtoolnix".into(),
            caps: vec![Capability::Execute {
                operations: vec!["metadata".into(), "remux".into()],
                formats: vec!["mkv".into()],
            }],
        });
        let ffmpeg = Arc::new(FakePlugin {
            name: "ffmpeg".into(),
            caps: vec![Capability::Execute {
                operations: vec!["transcode".into()],
                formats: vec![],
            }],
        });

        registry.register(mkv);
        registry.register(ffmpeg);

        // mkvtoolnix handles metadata on mkv
        let p = registry.find_for_operation("metadata", "mkv");
        assert!(p.is_some());
        assert_eq!(p.unwrap().name(), "mkvtoolnix");

        // ffmpeg handles transcode on any format
        let p = registry.find_for_operation("transcode", "mp4");
        assert!(p.is_some());
        assert_eq!(p.unwrap().name(), "ffmpeg");

        // nobody handles "magic"
        assert!(registry.find_for_operation("magic", "mkv").is_none());
    }
}
