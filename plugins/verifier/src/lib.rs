//! Media file integrity verifier plugin.
//!
//! Three modes: quick (ffprobe header check), thorough (ffmpeg full
//! decode pass), hash (sha256 bit-rot detection). Library-callable
//! from the CLI; bus subscriber that handles `PlanCreated` events
//! carrying `VerifyMedia` or `Quarantine` operations.

pub mod config;
pub mod hash;
pub mod hwaccel;
pub mod quarantine;
pub mod quick;
pub mod thorough;
mod util;

use std::sync::Arc;
use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult, FileQuarantinedEvent, VerifyCompletedEvent};
use voom_domain::plan::{ActionParams, OperationType, Plan};
use voom_domain::storage::StorageTrait;
use voom_domain::transition::FileStatus;
use voom_domain::verification::{VerificationMode, VerificationRecord};
use voom_kernel::{Plugin, PluginContext};

pub use config::VerifierConfig;

/// Verifier plugin — handles `verify` operations from DSL plans and
/// exposes library helpers for direct CLI invocation.
pub struct VerifierPlugin {
    capabilities: Vec<Capability>,
    config: VerifierConfig,
    store: Option<Arc<dyn StorageTrait>>,
}

fn plugin_err(message: impl Into<String>) -> VoomError {
    VoomError::plugin("verifier", message)
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
            store: None,
        }
    }

    /// Construct with an injected storage handle. Used by the CLI bootstrap
    /// so `on_event` can persist verification records and update
    /// `files.status` on quarantine.
    #[must_use]
    pub fn with_store(store: Arc<dyn StorageTrait>) -> Self {
        let mut p = Self::new();
        p.store = Some(store);
        p
    }

    /// Access the parsed plugin config.
    #[must_use]
    pub fn config(&self) -> &VerifierConfig {
        &self.config
    }

    /// Resolve `thorough_hw_accel` from config to a concrete decode backend.
    ///
    /// Returns `None` for CPU decode (default, unrecognised values, or
    /// when the configured backend isn't advertised by the local ffmpeg).
    fn resolve_thorough_hw_accel(&self) -> Option<hwaccel::HwAccelMode> {
        let mode =
            hwaccel::HwAccelMode::parse(&self.config.thorough_hw_accel).unwrap_or_else(|| {
                tracing::warn!(
                    value = self.config.thorough_hw_accel.as_str(),
                    "unrecognised thorough_hw_accel value (valid: none, auto, nvdec, \
                     vaapi, qsv, videotoolbox); falling back to CPU"
                );
                hwaccel::HwAccelMode::None
            });
        if matches!(mode, hwaccel::HwAccelMode::None) {
            return None;
        }
        let probed = hwaccel::probe_hwaccels(&self.config.ffmpeg_path);
        hwaccel::resolve(mode, &probed)
    }

    fn handle_verify_plan(&self, plan: &Plan) -> Result<Option<EventResult>> {
        let Some(action) = plan.actions.first() else {
            return Ok(None);
        };
        let ActionParams::VerifyMedia(params) = &action.parameters else {
            return Ok(None);
        };

        let store = self.store.as_ref().ok_or_else(|| {
            plugin_err("verifier has no storage handle; cannot persist verification record")
        })?;

        let file_id = plan.file.id.to_string();
        let path = plan.file.path.clone();
        let mode = params.mode;

        let record = match mode {
            VerificationMode::Quick => quick::run_quick(
                &file_id,
                &path,
                &self.config.ffprobe_path,
                Duration::from_secs(self.config.quick_timeout_secs),
            )?,
            VerificationMode::Thorough => {
                let timeout = thorough::timeout_from_duration(
                    Some(plan.file.duration),
                    self.config.thorough_timeout_multiplier,
                    self.config.thorough_timeout_floor_secs,
                );
                let hw_accel = self.resolve_thorough_hw_accel();
                thorough::run_thorough(
                    &file_id,
                    &path,
                    &self.config.ffmpeg_path,
                    timeout,
                    hw_accel,
                )?
            }
            VerificationMode::Hash => {
                let prior = store.latest_verification(&file_id, VerificationMode::Hash)?;
                hash::run_hash(&file_id, &path, prior.as_ref())?
            }
        };

        store.insert_verification(&record)?;
        Ok(Some(verify_result(&record, path)))
    }

    fn handle_quarantine_plan(&self, plan: &Plan) -> Result<Option<EventResult>> {
        let Some(action) = plan.actions.first() else {
            return Ok(None);
        };
        let ActionParams::Quarantine(params) = &action.parameters else {
            return Ok(None);
        };

        let store = self.store.as_ref().ok_or_else(|| {
            plugin_err("verifier has no storage handle; cannot mark file quarantined")
        })?;
        let dir = self.config.quarantine_dir.as_ref().ok_or_else(|| {
            plugin_err("quarantine_dir is not configured; set [plugin.verifier].quarantine_dir")
        })?;

        let from = plan.file.path.clone();
        let to = quarantine::quarantine_file(&from, dir)?;
        store.set_file_status(&plan.file.id, FileStatus::Quarantined)?;

        Ok(Some(quarantine_result(
            plan.file.id.to_string(),
            from,
            to,
            params.reason.clone(),
        )))
    }
}

/// Build the EventResult for a successful verification.
fn verify_result(record: &VerificationRecord, path: std::path::PathBuf) -> EventResult {
    let event = Event::VerifyCompleted(VerifyCompletedEvent::new(
        record.file_id.clone(),
        path,
        record.mode,
        record.outcome,
        record.error_count,
        record.warning_count,
        record.id,
    ));
    let mut result = EventResult::new("verifier");
    result.claimed = true;
    result.produced_events = vec![event];
    result
}

/// Build the EventResult for a successful quarantine.
fn quarantine_result(
    file_id: String,
    from: std::path::PathBuf,
    to: std::path::PathBuf,
    reason: String,
) -> EventResult {
    let event = Event::FileQuarantined(FileQuarantinedEvent::new(file_id, from, to, reason));
    let mut result = EventResult::new("verifier");
    result.claimed = true;
    result.produced_events = vec![event];
    result
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

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::PLAN_CREATED
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
            has_store = self.store.is_some(),
            "verifier initialized"
        );

        Ok(vec![])
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        let Event::PlanCreated(ev) = event else {
            return Ok(None);
        };
        let plan = &ev.plan;
        if plan.is_empty() || plan.is_skipped() {
            return Ok(None);
        }
        let Some(first) = plan.actions.first() else {
            return Ok(None);
        };
        match first.operation {
            OperationType::VerifyMedia => self.handle_verify_plan(plan),
            OperationType::Quarantine => self.handle_quarantine_plan(plan),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::PlanCreatedEvent;
    use voom_domain::media::MediaFile;
    use voom_domain::plan::{PlannedAction, QuarantineParams};
    use voom_domain::test_support::InMemoryStore;

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
        assert_eq!(cfg.thorough_hw_accel, "none");
        assert!(!cfg.verify_on_scan);
        assert_eq!(cfg.verify_freshness_days, 7);
    }

    #[test]
    fn resolve_thorough_hw_accel_none_short_circuits() {
        // Default config carries thorough_hw_accel = "none"; resolving
        // must not invoke ffmpeg (which may be absent in the test env).
        let p = VerifierPlugin::new();
        assert!(p.resolve_thorough_hw_accel().is_none());
    }

    #[test]
    fn resolve_thorough_hw_accel_invalid_falls_back_to_cpu() {
        let mut p = VerifierPlugin::new();
        p.config.thorough_hw_accel = "garbage".into();
        assert!(p.resolve_thorough_hw_accel().is_none());
    }

    #[test]
    fn handles_only_plan_created() {
        let p = VerifierPlugin::new();
        assert!(p.handles(Event::PLAN_CREATED));
        assert!(!p.handles(Event::PLAN_EXECUTING));
        assert!(!p.handles(Event::FILE_DISCOVERED));
        assert!(!p.handles(Event::PLAN_COMPLETED));
    }

    #[test]
    fn ignores_unrelated_plans() {
        // Plan with a non-verify operation must not be claimed.
        let p = VerifierPlugin::with_store(Arc::new(InMemoryStore::new()));
        let file = MediaFile::new(PathBuf::from("/m/x.mkv"));
        let plan = Plan::new(file, "policy", "init").with_action(PlannedAction::track_op(
            OperationType::SetDefault,
            0,
            ActionParams::Empty,
            "set default",
        ));
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let result = p.on_event(&event).unwrap();
        assert!(result.is_none(), "verifier must not claim non-verify plans");
    }

    #[test]
    fn ignores_empty_and_skipped_plans() {
        let p = VerifierPlugin::with_store(Arc::new(InMemoryStore::new()));
        let file = MediaFile::new(PathBuf::from("/m/x.mkv"));

        let empty = Plan::new(file.clone(), "policy", "verify");
        let event = Event::PlanCreated(PlanCreatedEvent::new(empty));
        assert!(p.on_event(&event).unwrap().is_none());

        let skipped = Plan::new(file, "policy", "verify").with_skip_reason("no source");
        let event = Event::PlanCreated(PlanCreatedEvent::new(skipped));
        assert!(p.on_event(&event).unwrap().is_none());
    }

    #[test]
    fn ignores_other_event_types() {
        let p = VerifierPlugin::with_store(Arc::new(InMemoryStore::new()));
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/m/x.mkv"),
            10,
            None,
        ));
        assert!(p.on_event(&event).unwrap().is_none());
    }

    #[test]
    fn quarantine_without_dir_errors() {
        let p = VerifierPlugin::with_store(Arc::new(InMemoryStore::new()));
        let file = MediaFile::new(PathBuf::from("/m/x.mkv"));
        let plan = Plan::new(file, "policy", "quarantine").with_action(PlannedAction::file_op(
            OperationType::Quarantine,
            ActionParams::Quarantine(QuarantineParams {
                reason: "decode error".into(),
            }),
            "quarantine",
        ));
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let err = p.on_event(&event).unwrap_err();
        assert!(
            err.to_string().contains("quarantine_dir"),
            "expected quarantine_dir error, got: {err}"
        );
    }

    #[test]
    fn quarantine_moves_file_and_emits_event() {
        let qd = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let src_path = src_dir.path().join("bad.mkv");
        std::fs::write(&src_path, b"corrupt").unwrap();

        let store = Arc::new(InMemoryStore::new());
        let mut p = VerifierPlugin::with_store(store.clone() as Arc<dyn StorageTrait>);
        p.config.quarantine_dir = Some(qd.path().to_path_buf());

        let mut file = MediaFile::new(src_path.clone());
        let file_id = file.id;
        // Seed the store so set_file_status has something to update.
        use voom_domain::storage::FileStorage;
        file.path = src_path.clone();
        store.upsert_file(&file).unwrap();

        let plan = Plan::new(file, "policy", "quarantine").with_action(PlannedAction::file_op(
            OperationType::Quarantine,
            ActionParams::Quarantine(QuarantineParams {
                reason: "decode error".into(),
            }),
            "quarantine",
        ));
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let result = p.on_event(&event).unwrap().expect("expected EventResult");
        assert!(result.claimed);
        assert_eq!(result.plugin_name, "verifier");
        assert_eq!(result.produced_events.len(), 1);
        match &result.produced_events[0] {
            Event::FileQuarantined(e) => {
                assert_eq!(e.from, src_path);
                assert!(e.to.starts_with(qd.path()));
                assert_eq!(e.reason, "decode error");
                assert_eq!(e.file_id, file_id.to_string());
            }
            other => panic!("expected FileQuarantined, got {other:?}"),
        }
        assert!(!src_path.exists(), "source file must be moved");

        // Verify the store records the new status.
        let after = store.file(&file_id).unwrap().expect("file row exists");
        // Quarantined files round-trip with the new FileStatus::Quarantined variant.
        assert_eq!(after.status, FileStatus::Quarantined);
    }
}
