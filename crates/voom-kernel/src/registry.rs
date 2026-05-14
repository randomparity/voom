use std::any::{Any, TypeId};
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
    ///
    /// Returns an error if a plugin with the same name is already registered.
    pub fn register(&self, plugin: Arc<dyn Plugin>) -> voom_domain::errors::Result<()> {
        let name = plugin.name().to_string();
        let mut plugins = self.plugins.write();
        if plugins.contains_key(&name) {
            return Err(voom_domain::errors::VoomError::Plugin {
                plugin: name,
                message: "a plugin with this name is already registered".into(),
            });
        }
        plugins.insert(name, plugin);
        Ok(())
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

    /// Snapshot of all currently-registered (name, plugin) pairs.
    ///
    /// Returns clones (the registry holds plugins behind `Arc`s, so cloning is
    /// cheap). The result is a `Vec` rather than an iterator to release the
    /// internal `RwLock` immediately.
    #[must_use]
    pub fn iter_all(&self) -> Vec<(String, Arc<dyn Plugin>)> {
        self.plugins
            .read()
            .iter()
            .map(|(n, p)| (n.clone(), p.clone()))
            .collect()
    }

    /// Retrieve a plugin by name, downcasting to the concrete type.
    ///
    /// Returns `None` if not registered, or if registered under a different
    /// concrete type. Use sparingly — the canonical addressing scheme for
    /// cross-plugin invocation is `Kernel::dispatch_to_capability`.
    #[must_use]
    pub fn get_typed<P: Plugin>(&self, name: &str) -> Option<Arc<P>> {
        let arc_dyn = self.get(name)?;
        // Verify the dynamic type matches before reconstructing the typed Arc.
        // `Plugin: Any` (supertrait) makes the per-impl `type_id` method
        // available via the trait object's vtable.
        if Any::type_id(&*arc_dyn) != TypeId::of::<P>() {
            return None;
        }
        // SAFETY: TypeId equality proves the underlying value is `P`. The Arc
        // was originally constructed as `Arc::new(p)` where `p: P`, so the
        // ArcInner allocation has the layout of `ArcInner<P>`. Casting the
        // wide pointer's data part to `*const P` and reconstructing via
        // `Arc::from_raw` is sound because (a) the data pointer is valid
        // `*const P`, and (b) Arc reference counts are tracked in the
        // ArcInner header, independent of the type parameter, so the strong
        // count remains correct across the type change.
        let raw: *const P = Arc::into_raw(arc_dyn).cast::<P>();
        Some(unsafe { Arc::from_raw(raw) })
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
        registry.register(plugin).unwrap();

        assert!(registry.get("test-plugin").is_some());
        assert!(registry.get("nonexistent").is_none());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_get_typed_returns_matching_plugin() {
        struct AlphaPlugin {
            value: u64,
        }
        impl Plugin for AlphaPlugin {
            fn name(&self) -> &str {
                "alpha"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn capabilities(&self) -> &[Capability] {
                &[]
            }
        }

        let registry = PluginRegistry::new();
        registry.register(Arc::new(AlphaPlugin { value: 42 })).unwrap();
        let retrieved = registry
            .get_typed::<AlphaPlugin>("alpha")
            .expect("typed retrieval should succeed");
        assert_eq!(retrieved.value, 42);
    }

    #[test]
    fn test_get_typed_returns_none_for_wrong_type() {
        struct AlphaPlugin;
        struct BetaPlugin;
        impl Plugin for AlphaPlugin {
            fn name(&self) -> &str {
                "alpha"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn capabilities(&self) -> &[Capability] {
                &[]
            }
        }
        impl Plugin for BetaPlugin {
            fn name(&self) -> &str {
                "beta"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn capabilities(&self) -> &[Capability] {
                &[]
            }
        }

        let registry = PluginRegistry::new();
        registry.register(Arc::new(AlphaPlugin)).unwrap();
        assert!(registry.get_typed::<BetaPlugin>("alpha").is_none());
    }

    #[test]
    fn test_get_typed_returns_none_for_missing_name() {
        struct DummyPlugin;
        impl Plugin for DummyPlugin {
            fn name(&self) -> &str {
                "dummy"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn capabilities(&self) -> &[Capability] {
                &[]
            }
        }
        let registry = PluginRegistry::new();
        assert!(registry.get_typed::<DummyPlugin>("nonexistent").is_none());
    }

    #[test]
    fn test_duplicate_register_rejected() {
        let registry = PluginRegistry::new();
        let p1 = Arc::new(FakePlugin {
            name: "dup".into(),
            caps: vec![],
        });
        let p2 = Arc::new(FakePlugin {
            name: "dup".into(),
            caps: vec![],
        });

        registry.register(p1).unwrap();
        let err = registry.register(p2).unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "expected 'already registered' error, got: {err}"
        );
        assert_eq!(registry.len(), 1);
    }
}
