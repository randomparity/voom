use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::Plugin;

/// Stores all registered plugins and enables capability-based lookup.
pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, Arc<dyn Plugin>>>,
}

impl PluginRegistry {
    #[must_use]
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

    /// Returns `true` if a plugin with the given name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.plugins.read().contains_key(name)
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
}
