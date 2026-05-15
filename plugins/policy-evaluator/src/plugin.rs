//! Policy evaluator plugin: claims `Capability::EvaluatePolicy` and routes
//! `Call::EvaluatePolicy` to the existing free-function evaluator surface.
//!
//! The plugin subscribes to `Event::EXECUTOR_CAPABILITIES` to keep a cached
//! `CapabilityMap`, which `on_call` consults when no `capabilities_override`
//! rides with the call. This mirrors today's capability-collector pattern
//! while exposing a Call-handling capability the kernel can route to.

use parking_lot::RwLock;

use voom_domain::capabilities::Capability;
use voom_domain::capability_map::CapabilityMap;
use voom_domain::events::{Event, EventResult};
use voom_kernel::Plugin;

/// Policy evaluator plugin — claims `Capability::EvaluatePolicy` (Exclusive)
/// and maintains a cached `CapabilityMap` from `ExecutorCapabilities` events.
pub struct PolicyEvaluatorPlugin {
    capabilities: Vec<Capability>,
    cache: RwLock<CapabilityMap>,
}

impl PolicyEvaluatorPlugin {
    /// Bootstrap-only constructor — the canonical public entry point.
    ///
    /// All runtime evaluator invocations flow through
    /// `Kernel::dispatch_to_capability(Exclusive(EvaluatePolicy), Call::EvaluatePolicy)`.
    /// Constructing a `PolicyEvaluatorPlugin` directly is legitimate only at
    /// kernel-bootstrap registration time and inside in-crate tests
    /// (via the `pub(crate) fn new()` companion).
    #[must_use]
    pub fn for_bootstrap() -> Self {
        Self::new()
    }

    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            capabilities: vec![Capability::EvaluatePolicy],
            cache: RwLock::new(CapabilityMap::new()),
        }
    }
}

impl Plugin for PolicyEvaluatorPlugin {
    fn name(&self) -> &'static str {
        "policy-evaluator"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::EXECUTOR_CAPABILITIES
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::ExecutorCapabilities(caps) = event {
            self.cache.write().register(caps.clone());
        }
        Ok(None)
    }

    fn on_call(
        &self,
        call: &voom_domain::call::Call,
    ) -> voom_domain::errors::Result<voom_domain::call::CallResponse> {
        use voom_domain::call::{Call, CallResponse};

        let Call::EvaluatePolicy {
            policy,
            file,
            phase,
            phase_outputs,
            phase_outcomes,
            capabilities_override,
        } = call
        else {
            return Err(voom_domain::errors::VoomError::plugin(
                self.name(),
                format!(
                    "PolicyEvaluatorPlugin only handles Call::EvaluatePolicy, got {:?}",
                    std::mem::discriminant(call)
                ),
            ));
        };

        // Resolve capability snapshot: override takes precedence; otherwise the
        // cached snapshot populated by on_event. The lock is released before
        // any evaluator work runs.
        let caps = capabilities_override
            .clone()
            .unwrap_or_else(|| self.cache.read().clone());

        let result = match (phase.as_deref(), phase_outputs.as_ref()) {
            (None, None) => {
                // Full policy, no cross-phase lookup. The high-level wrapper
                // already includes apply_capability_hints.
                crate::evaluate_with_capabilities(policy, file, &caps)
            }
            (None, Some(outputs)) => {
                // Full policy WITH cross-phase lookup. Build the closure-based
                // lookup, evaluate, then apply hints.
                let lookup_fn: Box<crate::condition::PhaseOutputLookup<'_>> =
                    Box::new(|phase_name: &str| outputs.get(phase_name).cloned());
                let mut res = crate::evaluator::evaluate_with_phase_outputs(
                    policy,
                    file,
                    Some(&caps),
                    Some(&*lookup_fn),
                );
                crate::evaluator::apply_capability_hints(&mut res.plans, &caps);
                res
            }
            (Some(phase_name), None) => {
                // Single phase, no cross-phase lookup.
                // `evaluate_single_phase_with_hints` calls
                // `apply_capability_hints` internally.
                let outcomes = phase_outcomes.clone().unwrap_or_default();
                let plan = crate::evaluate_single_phase_with_hints(
                    phase_name, policy, file, &outcomes, &caps,
                );
                voom_domain::evaluation::EvaluationResult::new(
                    plan.map(|p| vec![p]).unwrap_or_default(),
                )
            }
            (Some(phase_name), Some(outputs)) => {
                // Single phase WITH cross-phase lookup. Build lookup, evaluate,
                // apply hints to the (single) plan.
                let lookup_fn: Box<crate::condition::PhaseOutputLookup<'_>> =
                    Box::new(|phase_name: &str| outputs.get(phase_name).cloned());
                let outcomes = phase_outcomes.clone().unwrap_or_default();
                let mut plan = crate::evaluator::evaluate_single_phase_with_phase_outputs(
                    phase_name,
                    policy,
                    file,
                    &outcomes,
                    Some(&caps),
                    Some(&*lookup_fn),
                );
                if let Some(p) = plan.as_mut() {
                    crate::evaluator::apply_capability_hints(std::slice::from_mut(p), &caps);
                }
                voom_domain::evaluation::EvaluationResult::new(
                    plan.map(|p| vec![p]).unwrap_or_default(),
                )
            }
        };

        Ok(CallResponse::EvaluatePolicy(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

    #[test]
    fn claims_exclusive_evaluate_policy_capability() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert_eq!(plugin.capabilities(), &[Capability::EvaluatePolicy]);
        assert_eq!(
            Capability::EvaluatePolicy.resolution(),
            voom_domain::capability_resolution::CapabilityResolution::Exclusive,
            "EvaluatePolicy must be Exclusive — second plugin claiming it fails at register"
        );
    }

    #[test]
    fn subscribes_only_to_executor_capabilities_events() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert!(plugin.handles(Event::EXECUTOR_CAPABILITIES));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn on_event_populates_capability_cache() {
        let plugin = PolicyEvaluatorPlugin::new();
        let event = Event::ExecutorCapabilities(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(vec!["h264".into()], vec!["libx264".into(), "aac".into()]),
            vec!["matroska".into()],
            vec![],
        ));

        plugin.on_event(&event).expect("on_event should not fail");

        let snapshot = plugin.cache.read().clone();
        assert!(!snapshot.is_empty(), "cache must be populated after event");
        assert!(snapshot.has_encoder("libx264"));
        assert!(snapshot.has_format("matroska"));
    }

    #[test]
    fn on_call_wrong_variant_returns_plugin_error() {
        let plugin = PolicyEvaluatorPlugin::new();
        let call = voom_domain::call::Call::Orchestrate {
            plans: vec![],
            policy_name: "demo".into(),
        };
        let err = plugin
            .on_call(&call)
            .expect_err("Orchestrate is not handled by policy-evaluator");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("PolicyEvaluatorPlugin") || msg.contains("policy-evaluator"),
            "error must name the plugin; got: {msg}"
        );
        assert!(
            msg.contains("EvaluatePolicy"),
            "error must say which variant it expected; got: {msg}"
        );
    }

    #[test]
    fn on_call_full_policy_no_phase_outputs_uses_evaluate_with_capabilities() {
        use std::path::PathBuf;
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::media::MediaFile;

        let plugin = PolicyEvaluatorPlugin::new();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let call = Call::EvaluatePolicy {
            policy: Box::new(policy),
            file: Box::new(file),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };

        let response = plugin.on_call(&call).expect("on_call should succeed");
        let CallResponse::EvaluatePolicy(result) = response else {
            panic!("expected EvaluatePolicy response; got {response:?}");
        };
        assert_eq!(result.plans.len(), 1, "init phase produces one plan");
        assert_eq!(result.plans[0].phase_name, "init");
    }

    #[test]
    fn on_call_single_phase_returns_zero_or_one_plan() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::media::MediaFile;

        let plugin = PolicyEvaluatorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "demo" {
                phase one { container mkv }
                phase two { keep audio where lang in [eng] }
            }"#,
        )
        .unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        // Phase "one" exists — one plan returned.
        let call_present = Call::EvaluatePolicy {
            policy: Box::new(policy.clone()),
            file: Box::new(file.clone()),
            phase: Some("one".into()),
            phase_outputs: None,
            phase_outcomes: Some(HashMap::new()),
            capabilities_override: None,
        };
        let CallResponse::EvaluatePolicy(present) = plugin.on_call(&call_present).unwrap() else {
            panic!("expected EvaluatePolicy");
        };
        assert_eq!(present.plans.len(), 1);
        assert_eq!(present.plans[0].phase_name, "one");

        // Phase "missing" does not exist — zero plans returned (Option::None
        // surfaces as an empty Vec so CLI callers can use plans.first()).
        let call_missing = Call::EvaluatePolicy {
            policy: Box::new(policy),
            file: Box::new(file),
            phase: Some("missing".into()),
            phase_outputs: None,
            phase_outcomes: Some(HashMap::new()),
            capabilities_override: None,
        };
        let CallResponse::EvaluatePolicy(missing) = plugin.on_call(&call_missing).unwrap() else {
            panic!("expected EvaluatePolicy");
        };
        assert!(
            missing.plans.is_empty(),
            "missing phase must surface as empty plans, not error"
        );
    }

    #[test]
    fn on_call_full_policy_with_phase_outputs_routes_through_lookup() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::media::MediaFile;

        let plugin = PolicyEvaluatorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "demo" {
                phase a { container mkv }
                phase b { keep audio where lang in [eng] }
            }"#,
        )
        .unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let call = Call::EvaluatePolicy {
            policy: Box::new(policy),
            file: Box::new(file),
            phase: None,
            // Empty map is fine — the goal is to exercise the routing arm
            // (boxed PhaseOutputLookup construction + apply_capability_hints).
            phase_outputs: Some(HashMap::new()),
            phase_outcomes: None,
            capabilities_override: None,
        };

        let CallResponse::EvaluatePolicy(result) = plugin.on_call(&call).expect("on_call") else {
            panic!("expected EvaluatePolicy");
        };
        assert_eq!(result.plans.len(), 2);
        // Phase order is determined by the DSL compiler's topological sort,
        // which is not the source order when phases have no `depends_on`. Just
        // assert that both phases were evaluated — the routing arm wraps the
        // result and applies hints regardless of order.
        let mut phase_names: Vec<&str> =
            result.plans.iter().map(|p| p.phase_name.as_str()).collect();
        phase_names.sort_unstable();
        assert_eq!(phase_names, vec!["a", "b"]);
    }

    #[test]
    fn on_call_single_phase_with_phase_outputs_routes_through_lookup() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::media::MediaFile;

        let plugin = PolicyEvaluatorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "demo" {
                phase target { container mkv }
            }"#,
        )
        .unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let call = Call::EvaluatePolicy {
            policy: Box::new(policy),
            file: Box::new(file),
            phase: Some("target".into()),
            phase_outputs: Some(HashMap::new()),
            phase_outcomes: Some(HashMap::new()),
            capabilities_override: None,
        };

        let CallResponse::EvaluatePolicy(result) = plugin.on_call(&call).expect("on_call") else {
            panic!("expected EvaluatePolicy");
        };
        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].phase_name, "target");
    }

    #[test]
    fn on_call_prefers_capabilities_override_over_cached_snapshot() {
        use std::path::PathBuf;
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, Event, ExecutorCapabilitiesEvent};
        use voom_domain::media::MediaFile;

        let plugin = PolicyEvaluatorPlugin::new();
        // Seed cache with a registered executor that lists NO encoders. The
        // map is non-empty (so apply_capability_hints will run) but no executor
        // can encode h264 — yielding warnings and no executor_hint.
        plugin
            .on_event(&Event::ExecutorCapabilities(
                ExecutorCapabilitiesEvent::new(
                    "ffmpeg-executor",
                    CodecCapabilities::new(vec![], vec![]),
                    vec![],
                    vec![],
                ),
            ))
            .unwrap();

        let mut override_map = CapabilityMap::new();
        // `encoders_for` matches encoder names against the policy's codec
        // identifier verbatim, so the encoders list must contain "h264"
        // (the wire name used in the policy), not the ffmpeg library alias.
        override_map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(vec!["h264".into()], vec!["h264".into()]),
            vec!["matroska".into()],
            vec![],
        ));

        let policy = voom_dsl::compile_policy(
            r#"policy "demo" {
                phase tc {
                    transcode video to h264 { crf: 20 }
                }
            }"#,
        )
        .unwrap();
        let mut file = MediaFile::new(PathBuf::from("/movies/test.mkv"));
        file.container = voom_domain::media::Container::Mkv;
        let mut video =
            voom_domain::media::Track::new(0, voom_domain::media::TrackType::Video, "hevc".into());
        video.width = Some(1920);
        video.height = Some(1080);
        file.tracks = vec![video];

        let with_override = Call::EvaluatePolicy {
            policy: Box::new(policy.clone()),
            file: Box::new(file.clone()),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: Some(override_map),
        };
        let without_override = Call::EvaluatePolicy {
            policy: Box::new(policy),
            file: Box::new(file),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };

        let CallResponse::EvaluatePolicy(with_caps) = plugin.on_call(&with_override).unwrap()
        else {
            panic!()
        };
        let CallResponse::EvaluatePolicy(without_caps) = plugin.on_call(&without_override).unwrap()
        else {
            panic!()
        };

        // `apply_capability_hints` mutates `plan.executor_hint` (the hint is
        // attached to the Plan, not to individual PlannedActions). With the
        // override map ffmpeg-executor is the sole h264 encoder, so the hint
        // is set; without the override no executor advertises h264, so the
        // hint is None.
        let with_hints: Vec<_> = with_caps
            .plans
            .iter()
            .map(|p| p.executor_hint.clone())
            .collect();
        let without_hints: Vec<_> = without_caps
            .plans
            .iter()
            .map(|p| p.executor_hint.clone())
            .collect();
        assert_ne!(
            with_hints, without_hints,
            "override map must yield different hints than empty cache; with={with_hints:?} without={without_hints:?}"
        );
        assert_eq!(
            with_hints,
            vec![Some("ffmpeg-executor".to_string())],
            "override map should pin the plan to ffmpeg-executor"
        );
        assert_eq!(
            without_hints,
            vec![None],
            "cached snapshot with no encoders should leave executor_hint unset"
        );
    }
}
