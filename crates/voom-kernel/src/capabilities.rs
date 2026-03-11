use voom_domain::capabilities::Capability;

use crate::Plugin;
use std::sync::Arc;

/// Query helper for finding plugins by capability constraints.
pub struct CapabilityQuery<'a> {
    plugins: &'a [Arc<dyn Plugin>],
}

impl<'a> CapabilityQuery<'a> {
    pub fn new(plugins: &'a [Arc<dyn Plugin>]) -> Self {
        Self { plugins }
    }

    /// Find plugins with a specific capability kind.
    pub fn with_kind(&self, kind: &str) -> Vec<&Arc<dyn Plugin>> {
        self.plugins
            .iter()
            .filter(|p| p.capabilities().iter().any(|c| c.kind() == kind))
            .collect()
    }

    /// Find plugins that can execute a specific operation on a format.
    pub fn can_execute(&self, operation: &str, format: &str) -> Vec<&Arc<dyn Plugin>> {
        self.plugins
            .iter()
            .filter(|p| {
                p.capabilities()
                    .iter()
                    .any(|c| c.supports_operation(operation) && c.supports_format(format))
            })
            .collect()
    }

    /// Find plugins that can introspect a given format.
    pub fn can_introspect(&self, format: &str) -> Vec<&Arc<dyn Plugin>> {
        self.plugins
            .iter()
            .filter(|p| {
                p.capabilities().iter().any(|c| {
                    matches!(c, Capability::Introspect { .. }) && c.supports_format(format)
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::{Event, EventResult};

    struct StubPlugin {
        name: String,
        caps: Vec<Capability>,
    }

    impl Plugin for StubPlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &self.caps
        }
        fn handles(&self, _: &str) -> bool {
            false
        }
        fn on_event(&self, _: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            Ok(None)
        }
    }

    #[test]
    fn test_capability_query() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(StubPlugin {
                name: "ffprobe".into(),
                caps: vec![Capability::Introspect {
                    formats: vec!["mkv".into(), "mp4".into()],
                }],
            }),
            Arc::new(StubPlugin {
                name: "mkvtoolnix".into(),
                caps: vec![Capability::Execute {
                    operations: vec!["metadata".into()],
                    formats: vec!["mkv".into()],
                }],
            }),
        ];

        let query = CapabilityQuery::new(&plugins);

        assert_eq!(query.with_kind("introspect").len(), 1);
        assert_eq!(query.can_introspect("mkv").len(), 1);
        assert_eq!(query.can_introspect("avi").len(), 0);
        assert_eq!(query.can_execute("metadata", "mkv").len(), 1);
        assert_eq!(query.can_execute("metadata", "mp4").len(), 0);
    }
}
