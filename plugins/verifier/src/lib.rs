//! Media file integrity verifier plugin.
//!
//! Three modes: quick (ffprobe header check), thorough (ffmpeg full
//! decode pass), hash (sha256 bit-rot detection). Library-callable
//! from the CLI; bus subscriber for DSL-driven `verify` phase plans
//! (subscription wiring lands in Task 13).

pub mod config;
pub mod hash;
pub mod quarantine;
pub mod quick;
pub mod thorough;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::Event;
use voom_domain::verification::VerificationMode;
use voom_kernel::{Plugin, PluginContext};

pub use config::VerifierConfig;

/// Verifier plugin — handles `verify` operations from DSL plans and
/// exposes library helpers for direct CLI invocation.
pub struct VerifierPlugin {
    capabilities: Vec<Capability>,
    config: VerifierConfig,
}

impl VerifierPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Verify {
                modes: vec![
                    VerificationMode::Quick,
                    VerificationMode::Thorough,
                    VerificationMode::Hash,
                ],
            }],
            config: VerifierConfig::default(),
        }
    }

    /// Access the parsed plugin config.
    #[must_use]
    pub fn config(&self) -> &VerifierConfig {
        &self.config
    }
}

impl Default for VerifierPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for VerifierPlugin {
    fn name(&self) -> &str {
        "verifier"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, _event_type: &str) -> bool {
        // Bus subscription added in Task 13.
        false
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        self.config = match ctx.parse_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("verifier config parse failed, using defaults: {e}");
                VerifierConfig::default()
            }
        };

        tracing::info!(
            quick_timeout_secs = self.config.quick_timeout_secs,
            thorough_timeout_multiplier = self.config.thorough_timeout_multiplier,
            "verifier initialized"
        );

        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_advertises_verify_capability() {
        let p = VerifierPlugin::new();
        assert_eq!(p.name(), "verifier");
        assert!(p.capabilities().iter().any(|c| matches!(
            c,
            Capability::Verify { modes } if modes.len() == 3
        )));
    }

    #[test]
    fn config_defaults() {
        let cfg = VerifierConfig::default();
        assert_eq!(cfg.quick_timeout_secs, 30);
        assert!((cfg.thorough_timeout_multiplier - 4.0).abs() < f32::EPSILON);
        assert_eq!(cfg.thorough_timeout_floor_secs, 60);
        assert_eq!(cfg.ffprobe_path, "ffprobe");
        assert_eq!(cfg.ffmpeg_path, "ffmpeg");
        assert!(cfg.quarantine_dir.is_none());
    }
}
