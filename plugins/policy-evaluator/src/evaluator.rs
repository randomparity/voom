//! Core policy evaluation engine.
//!
//! Takes a [`CompiledPolicy`] and a [`MediaFile`] and produces a [`Plan`]
//! for each phase. The evaluator processes operations in order, generating
//! [`PlannedAction`]s that describe what changes need to be made.

use std::collections::{HashMap, HashSet};

use voom_domain::capability_map::CapabilityMap;
use voom_domain::errors::VoomError;
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{
    ActionParams, OperationType, Plan, PlannedAction, TranscodeSettings, VerifyMediaParams,
};
use voom_domain::safeguard::{SafeguardKind, SafeguardViolation};
use voom_dsl::compiled::{
    ClearActionsSettings, CompiledAction, CompiledConditional, CompiledDefault, CompiledFilter,
    CompiledOperation, CompiledPhase, CompiledPolicy, CompiledRule, CompiledSynthesize,
    CompiledTranscodeSettings, CompiledValueOrField, DefaultStrategy, ErrorStrategy, RulesMode,
    RunIfTrigger, SynthLanguage, SynthPosition, TrackTarget, TranscodeChannels,
};

use crate::condition::evaluate_condition;
use crate::container_compat::codec_supported;
use crate::field::{resolve_value_or_field, EvalContext, PhaseOutputLookup};
use crate::filter::{track_matches_with_context, tracks_for_target};

fn transcode_settings_from(s: &CompiledTranscodeSettings) -> TranscodeSettings {
    TranscodeSettings::default()
        .with_crf(s.crf)
        .with_preset(s.preset.clone())
        .with_bitrate(s.bitrate.clone())
        .with_channels(s.channels.clone())
        .with_hw(s.hw.clone())
        .with_hw_fallback(s.hw_fallback)
        .with_max_resolution(s.max_resolution.clone())
        .with_scale_algorithm(s.scale_algorithm.clone())
        .with_hdr_mode(s.hdr_mode.clone())
        .with_tune(s.tune.clone())
}

/// Result of evaluating a full policy against a file.
#[non_exhaustive]
#[derive(Debug)]
pub struct EvaluationResult {
    pub plans: Vec<Plan>,
}

/// Optional inputs used while evaluating every phase in a policy.
#[derive(Clone, Copy, Default)]
pub struct EvaluationContext<'a> {
    pub capabilities: Option<&'a CapabilityMap>,
    pub phase_output_lookup: Option<&'a PhaseOutputLookup<'a>>,
}

/// Outcome of a single phase evaluation.
///
/// This is internal to the evaluator and distinct from `voom_domain::plan::PhaseOutcome`,
/// which represents execution outcomes. This type tracks evaluation-time outcomes
/// (e.g., whether a phase produced modifications) for dependency resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationOutcome {
    Executed { modified: bool },
    Skipped,
    SafeguardFailed,
    ExecutionFailed,
}

/// Inputs used while evaluating one phase in a policy.
#[derive(Clone, Copy)]
pub struct SinglePhaseEvaluationContext<'a> {
    pub phase_outcomes: &'a HashMap<String, EvaluationOutcome>,
    pub capabilities: Option<&'a CapabilityMap>,
    pub phase_output_lookup: Option<&'a PhaseOutputLookup<'a>>,
}

/// Evaluate a compiled policy against a media file, producing plans for all phases.
#[must_use]
pub fn evaluate(policy: &CompiledPolicy, file: &MediaFile) -> EvaluationResult {
    evaluate_with_evaluation_context(policy, file, EvaluationContext::default())
}

/// Evaluate a compiled policy with an explicit evaluation context.
#[must_use]
pub fn evaluate_with_evaluation_context<'a>(
    policy: &CompiledPolicy,
    file: &MediaFile,
    context: EvaluationContext<'a>,
) -> EvaluationResult {
    let eval_ctx = EvalContext {
        capabilities: context.capabilities,
        phase_output_lookup: context.phase_output_lookup,
    };
    let mut plans = Vec::new();
    let mut phase_outcomes: HashMap<String, EvaluationOutcome> = HashMap::new();

    for phase_name in &policy.phase_order {
        let Some(phase) = policy.phases.iter().find(|p| &p.name == phase_name) else {
            continue;
        };

        let plan = evaluate_phase(phase, policy, file, &phase_outcomes, &eval_ctx);

        let outcome = if plan.is_skipped() {
            EvaluationOutcome::Skipped
        } else {
            EvaluationOutcome::Executed {
                modified: !plan.is_empty(),
            }
        };

        phase_outcomes.insert(phase_name.clone(), outcome);
        plans.push(plan);
    }

    if let Some(capabilities) = context.capabilities {
        apply_capability_hints(&mut plans, capabilities);
    }

    EvaluationResult { plans }
}

/// Evaluate a single phase of a compiled policy against a media file.
///
/// Used by the per-phase evaluate-execute-reintrospect loop: after each
/// phase executes and the file is re-introspected, the next phase is
/// evaluated against the updated file state.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use std::path::PathBuf;
/// use voom_domain::media::MediaFile;
/// use voom_dsl::compile_policy;
/// use voom_policy_evaluator::evaluator::evaluate_single_phase;
///
/// let policy = compile_policy(r#"policy "demo" {
///     phase init {
///         container mkv
///     }
/// }"#).unwrap();
///
/// let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));
/// let plan = evaluate_single_phase("init", &policy, &file, &HashMap::new(), None);
/// assert!(plan.is_some());
/// assert_eq!(plan.unwrap().phase_name, "init");
/// ```
#[must_use]
// Callers always use the default hasher; generalizing here would leak into many signatures.
#[allow(clippy::implicit_hasher)]
pub fn evaluate_single_phase(
    phase_name: &str,
    policy: &CompiledPolicy,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
    capabilities: Option<&CapabilityMap>,
) -> Option<Plan> {
    evaluate_single_phase_with_evaluation_context(
        phase_name,
        policy,
        file,
        SinglePhaseEvaluationContext {
            phase_outcomes,
            capabilities,
            phase_output_lookup: None,
        },
    )
}

/// Evaluate a single phase with both system capabilities and a phase-output
/// lookup for cross-phase field access.
///
/// Used by the per-phase evaluate-execute loop when the orchestrator already
/// has persisted phase outputs available.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn evaluate_single_phase_with_phase_outputs<'a>(
    phase_name: &str,
    policy: &CompiledPolicy,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
    capabilities: Option<&'a CapabilityMap>,
    phase_output_lookup: Option<&'a PhaseOutputLookup<'a>>,
) -> Option<Plan> {
    evaluate_single_phase_with_evaluation_context(
        phase_name,
        policy,
        file,
        SinglePhaseEvaluationContext {
            phase_outcomes,
            capabilities,
            phase_output_lookup,
        },
    )
}

/// Evaluate a single phase with an explicit evaluation context.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn evaluate_single_phase_with_evaluation_context<'a>(
    phase_name: &str,
    policy: &CompiledPolicy,
    file: &MediaFile,
    context: SinglePhaseEvaluationContext<'a>,
) -> Option<Plan> {
    let phase = policy.phases.iter().find(|p| p.name == phase_name)?;
    let eval_ctx = EvalContext {
        capabilities: context.capabilities,
        phase_output_lookup: context.phase_output_lookup,
    };
    let mut plan = evaluate_phase(phase, policy, file, context.phase_outcomes, &eval_ctx);
    if let Some(capabilities) = context.capabilities {
        apply_capability_hints(std::slice::from_mut(&mut plan), capabilities);
    }
    Some(plan)
}

/// Check whether a phase should be skipped based on `skip_when`, `run_if`,
/// and `depends_on` conditions. Returns `Some(reason)` if skipped.
fn check_skip_conditions(
    phase: &CompiledPhase,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
    eval_ctx: &EvalContext<'_>,
) -> Option<String> {
    if let Some(ref cond) = phase.skip_when {
        if evaluate_condition(cond, file, eval_ctx) {
            return Some("skip_when condition met".into());
        }
    }

    if let Some(ref run_if) = phase.run_if {
        let should_run = match phase_outcomes.get(&run_if.phase) {
            Some(outcome) => match run_if.trigger {
                RunIfTrigger::Modified => match outcome {
                    EvaluationOutcome::Executed { modified } => *modified,
                    EvaluationOutcome::Skipped
                    | EvaluationOutcome::SafeguardFailed
                    | EvaluationOutcome::ExecutionFailed => false,
                },
                RunIfTrigger::Completed => match outcome {
                    EvaluationOutcome::Executed { .. } => true,
                    EvaluationOutcome::Skipped
                    | EvaluationOutcome::SafeguardFailed
                    | EvaluationOutcome::ExecutionFailed => false,
                },
            },
            None => false,
        };
        if !should_run {
            return Some(format!(
                "run_if {}.{} not satisfied",
                run_if.phase,
                match run_if.trigger {
                    RunIfTrigger::Modified => "modified",
                    RunIfTrigger::Completed => "completed",
                }
            ));
        }
    }

    for dep in &phase.depends_on {
        if phase_outcomes.get(dep).is_none() {
            return Some(format!("dependency '{dep}' not yet executed"));
        }
    }

    None
}

/// Evaluate a single phase against a file.
fn evaluate_phase(
    phase: &CompiledPhase,
    policy: &CompiledPolicy,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
    eval_ctx: &EvalContext<'_>,
) -> Plan {
    let mut plan = Plan::new(file.clone(), policy.name.clone(), phase.name.clone());
    plan.policy_hash = if policy.source_hash.is_empty() {
        None
    } else {
        Some(policy.source_hash.clone())
    };

    if let Some(reason) = check_skip_conditions(phase, file, phase_outcomes, eval_ctx) {
        plan.skip_reason = Some(reason);
        return plan;
    }

    let mut ctx = PhaseContext {
        plan: &mut plan,
        file,
        eval_ctx,
    };

    for op in &phase.operations {
        if let Err(msg) = emit_operation(op, &mut ctx) {
            match phase.on_error {
                ErrorStrategy::Abort => {
                    ctx.plan.warnings.push(format!("Error (aborting): {msg}"));
                    break;
                }
                ErrorStrategy::Continue => {
                    ctx.plan.warnings.push(format!("Error (continuing): {msg}"));
                }
                ErrorStrategy::Skip => {
                    ctx.plan.skip_reason = Some(format!("Error (skipping phase): {msg}"));
                    break;
                }
                // Quarantine halts the phase like Abort; the actual quarantine
                // plan emission for the failed file is handled by the
                // phase-orchestrator / executor coordination, not the evaluator.
                ErrorStrategy::Quarantine => {
                    ctx.plan.warnings.push(format!("Error (aborting): {msg}"));
                    break;
                }
            }
        }
    }

    apply_safeguards(&mut plan, file);

    plan
}

/// Post-evaluation safeguards pass. Runs after all operations in a phase
/// have been emitted, and may retract actions from the plan when they would
/// produce an unsafe or impossible result.
///
/// Currently applies:
/// - `NoVideoTrack` / `NoAudioTrack`: catches multi-operation scenarios
///   where individual keep/remove operations each leave some tracks but
///   the combination would remove every video or audio track.
/// - `ContainerIncompatible`: if a planned container conversion targets a
///   format that cannot hold the (post-transcode) codecs of the surviving
///   tracks, the `ConvertContainer` action is retracted.
fn apply_safeguards(plan: &mut Plan, file: &MediaFile) {
    if plan.is_skipped() {
        return;
    }

    let removed_indices: HashSet<u32> = plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::RemoveTrack)
        .filter_map(|a| a.track_index)
        .collect();

    let filename = file_name(file);

    if !removed_indices.is_empty() {
        apply_safeguard_for_track_type(
            plan,
            file,
            &removed_indices,
            &filename,
            TrackType::is_video,
            SafeguardKind::NoVideoTrack,
            "video",
        );
        apply_safeguard_for_track_type(
            plan,
            file,
            &removed_indices,
            &filename,
            TrackType::is_audio,
            SafeguardKind::NoAudioTrack,
            "audio",
        );
    }

    apply_container_safeguard(plan, file, &filename);
}

/// Post-evaluation safeguard: verify that a planned container conversion
/// can actually hold the codecs of the surviving tracks (taking any planned
/// transcodes and synthesized tracks into account). If not, retract the
/// `ConvertContainer` action and record a violation so the file stays in its
/// original container.
fn apply_container_safeguard(plan: &mut Plan, file: &MediaFile, filename: &str) {
    // Find the target container from a ConvertContainer action, if any.
    let Some(target) = plan.actions.iter().find_map(|a| {
        if a.operation != OperationType::ConvertContainer {
            return None;
        }
        if let ActionParams::Container { container } = &a.parameters {
            Some(*container)
        } else {
            None
        }
    }) else {
        return;
    };

    // Surviving tracks: those not subject to a RemoveTrack action.
    let removed: HashSet<u32> = plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::RemoveTrack)
        .filter_map(|a| a.track_index)
        .collect();

    // Per-track transcode target codec (post-transcode effective codec).
    let mut transcode_codec: HashMap<u32, String> = HashMap::new();
    for action in &plan.actions {
        if matches!(
            action.operation,
            OperationType::TranscodeVideo | OperationType::TranscodeAudio
        ) {
            if let (Some(idx), ActionParams::Transcode { codec, .. }) =
                (action.track_index, &action.parameters)
            {
                transcode_codec.insert(idx, codec.clone());
            }
        }
    }

    let mut offenders: Vec<String> = Vec::new();
    for track in &file.tracks {
        if removed.contains(&track.index) {
            continue;
        }
        let effective_codec = transcode_codec
            .get(&track.index)
            .cloned()
            .unwrap_or_else(|| track.codec.clone());
        if let Some(false) = codec_supported(target, &effective_codec) {
            offenders.push(format!("track {} ({effective_codec})", track.index));
        }
    }

    // `codec: None` means the synthesize operation inherits codec from a
    // downstream action — skip because there is no codec to check.
    for action in &plan.actions {
        if action.operation != OperationType::SynthesizeAudio {
            continue;
        }
        let ActionParams::Synthesize {
            codec: Some(codec),
            name,
            ..
        } = &action.parameters
        else {
            continue;
        };
        if let Some(false) = codec_supported(target, codec) {
            offenders.push(format!("synthesized {name} ({codec})"));
        }
    }

    if offenders.is_empty() {
        return;
    }

    // Retract the planned container conversion.
    plan.actions
        .retain(|a| a.operation != OperationType::ConvertContainer);

    let details = offenders
        .iter()
        .take(4)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let suffix = if offenders.len() > 4 {
        format!(" and {} more", offenders.len() - 4)
    } else {
        String::new()
    };
    let msg = format!(
        "Safeguard: container conversion from {} to {} would leave \
         incompatible codecs in {filename}: {details}{suffix}; keeping \
         original container",
        file.container.as_str(),
        target.as_str(),
    );
    record_safeguard(plan, SafeguardKind::ContainerIncompatible, msg);
}

fn apply_safeguard_for_track_type(
    plan: &mut Plan,
    file: &MediaFile,
    removed_indices: &HashSet<u32>,
    filename: &str,
    type_check: fn(&TrackType) -> bool,
    kind: SafeguardKind,
    label: &str,
) {
    let total = file
        .tracks
        .iter()
        .filter(|t| type_check(&t.track_type))
        .count();
    if total == 0 {
        return;
    }
    let surviving = file
        .tracks
        .iter()
        .filter(|t| type_check(&t.track_type) && !removed_indices.contains(&t.index))
        .count();
    if surviving > 0 {
        return;
    }
    // Retract RemoveTrack actions for this track type
    plan.actions.retain(|a| {
        !(a.operation == OperationType::RemoveTrack
            && a.track_index.is_some_and(|idx| {
                file.tracks
                    .iter()
                    .any(|t| t.index == idx && type_check(&t.track_type))
            }))
    });
    let msg = format!(
        "Safeguard: would have removed all {label} tracks in \
         {filename}, keeping them instead"
    );
    record_safeguard(plan, kind, msg);
}

/// Record a safeguard warning and violation on a plan.
fn record_safeguard(plan: &mut Plan, kind: SafeguardKind, msg: String) {
    plan.warnings.push(msg.clone());
    plan.safeguard_violations
        .push(SafeguardViolation::new(kind, msg, &plan.phase_name));
}

struct PhaseContext<'a> {
    plan: &'a mut Plan,
    file: &'a MediaFile,
    eval_ctx: &'a EvalContext<'a>,
}

/// Emit planned actions for a single operation into the plan.
fn emit_operation(op: &CompiledOperation, ctx: &mut PhaseContext) -> Result<(), VoomError> {
    match op {
        CompiledOperation::SetContainer(container) => {
            emit_set_container(*container, ctx);
        }
        CompiledOperation::Keep { target, filter } => {
            emit_keep(*target, filter.as_ref(), ctx);
        }
        CompiledOperation::Remove { target, filter } => {
            emit_remove(*target, filter.as_ref(), ctx);
        }
        CompiledOperation::ReorderTracks(order) => {
            emit_reorder(order, ctx);
        }
        CompiledOperation::SetDefaults(defaults) => {
            emit_set_defaults(defaults, ctx);
        }
        CompiledOperation::ClearActions { target, settings } => {
            emit_clear_actions(*target, settings, ctx);
        }
        CompiledOperation::Transcode {
            target,
            codec,
            settings,
        } => {
            emit_transcode(*target, codec, settings, ctx);
        }
        CompiledOperation::Synthesize(synth) => {
            emit_synthesize(synth, ctx);
        }
        CompiledOperation::ClearTags => {
            emit_clear_tags(ctx);
        }
        CompiledOperation::SetTag { tag, value } => {
            emit_set_tag(tag, value, ctx)?;
        }
        CompiledOperation::DeleteTag(tag) => {
            emit_delete_tag(tag, ctx);
        }
        CompiledOperation::Conditional(cond) => {
            emit_conditional(cond, ctx)?;
        }
        CompiledOperation::Rules { mode, rules } => {
            emit_rules(*mode, rules, ctx)?;
        }
        CompiledOperation::Verify { mode } => {
            emit_verify(*mode, ctx);
        }
    }
    Ok(())
}

fn emit_set_container(target: Container, ctx: &mut PhaseContext) {
    if ctx.file.container != target {
        ctx.plan.actions.push(PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container { container: target },
            format!(
                "Convert container from {} to {}",
                ctx.file.container.as_str(),
                target.as_str()
            ),
        ));
    }
}

fn emit_verify(mode: voom_domain::verification::VerificationMode, ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::file_op(
        OperationType::VerifyMedia,
        ActionParams::VerifyMedia(VerifyMediaParams { mode }),
        format!("Verify media integrity ({})", mode.as_str()),
    ));
}

fn emit_remove_track(track: &Track, target: TrackTarget, reason: &str, ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::track_op(
        OperationType::RemoveTrack,
        track.index,
        ActionParams::RemoveTrack {
            reason: reason.into(),
            track_type: track.track_type,
        },
        format!(
            "Remove {} track {} ({}, {})",
            target_str(target),
            track.index,
            track.codec,
            track.language
        ),
    ));
}

fn emit_keep(target: TrackTarget, filter: Option<&CompiledFilter>, ctx: &mut PhaseContext) {
    let tracks = tracks_for_target(ctx.file, target);
    if tracks.is_empty() {
        return;
    }

    let actions_before = ctx.plan.actions.len();
    let mut kept = 0u32;
    for track in &tracks {
        let should_remove = match filter {
            Some(f) => !track_matches_with_context(track, f, ctx.file, ctx.eval_ctx),
            None => false, // "keep audio" with no filter keeps all
        };
        if should_remove {
            emit_remove_track(track, target, "does not match keep filter", ctx);
        } else {
            kept += 1;
        }
    }

    if kept == 0 {
        ctx.plan.actions.truncate(actions_before);
        let label = target_str(target);
        let filename = file_name(ctx.file);
        let msg = format!(
            "Safeguard: kept all {label} tracks in {filename} \
             — no tracks matched the keep filter, would have removed all"
        );
        record_safeguard(ctx.plan, SafeguardKind::AllTracksRemoved, msg);
    }
}

fn emit_remove(target: TrackTarget, filter: Option<&CompiledFilter>, ctx: &mut PhaseContext) {
    let tracks = tracks_for_target(ctx.file, target);
    if tracks.is_empty() {
        return;
    }

    let is_critical = matches!(target, TrackTarget::Video | TrackTarget::Audio);
    let actions_before = ctx.plan.actions.len();
    let mut kept = 0u32;
    for track in &tracks {
        let should_remove = match filter {
            Some(f) => track_matches_with_context(track, f, ctx.file, ctx.eval_ctx),
            None => true, // "remove audio" with no filter removes all
        };
        if should_remove {
            emit_remove_track(track, target, "matches remove filter", ctx);
        } else {
            kept += 1;
        }
    }

    if kept == 0 && is_critical {
        ctx.plan.actions.truncate(actions_before);
        let label = target_str(target);
        let filename = file_name(ctx.file);
        let msg = format!(
            "Safeguard: kept all {label} tracks in {filename} \
             — remove operation would have removed all"
        );
        record_safeguard(ctx.plan, SafeguardKind::AllTracksRemoved, msg);
    }
}

fn emit_reorder(order: &[String], ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::file_op(
        OperationType::ReorderTracks,
        ActionParams::ReorderTracks {
            order: order.to_vec(),
        },
        format!("Reorder tracks: {}", order.join(", ")),
    ));
}

fn emit_set_default(target: TrackTarget, track: &Track, detail: &str, ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::track_op(
        OperationType::SetDefault,
        track.index,
        ActionParams::Empty,
        format!(
            "Set default on {} track {}{detail}",
            target_str(target),
            track.index
        ),
    ));
}

fn emit_clear_default(target: TrackTarget, track: &Track, detail: &str, ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::track_op(
        OperationType::ClearDefault,
        track.index,
        ActionParams::Empty,
        format!(
            "Clear default flag on {} track {}{detail}",
            target_str(target),
            track.index
        ),
    ));
}

fn emit_clear_forced(target: TrackTarget, track: &Track, ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::track_op(
        OperationType::ClearForced,
        track.index,
        ActionParams::Empty,
        format!(
            "Clear forced flag on {} track {}",
            target_str(target),
            track.index
        ),
    ));
}

/// Emit a "clear title" action. Uses `SetTitle` with an empty string as the
/// canonical representation — executors treat an empty title as "remove title".
fn emit_clear_title(target: TrackTarget, track: &Track, ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction::track_op(
        OperationType::SetTitle,
        track.index,
        ActionParams::Title {
            title: String::new(),
        },
        format!(
            "Clear title on {} track {}",
            target_str(target),
            track.index
        ),
    ));
}

fn emit_defaults_none(target: TrackTarget, tracks: &[&Track], ctx: &mut PhaseContext) {
    for track in tracks {
        if track.is_default {
            emit_clear_default(target, track, "", ctx);
        }
    }
}

fn emit_defaults_first(target: TrackTarget, tracks: &[&Track], ctx: &mut PhaseContext) {
    if let Some((first_track, rest)) = tracks.split_first() {
        if !first_track.is_default {
            emit_set_default(target, first_track, "", ctx);
        }
        for track in rest {
            if track.is_default {
                emit_clear_default(target, track, "", ctx);
            }
        }
    }
}

fn emit_defaults_first_per_language(
    target: TrackTarget,
    tracks: &[&Track],
    ctx: &mut PhaseContext,
) {
    let mut seen_langs: HashSet<String> = HashSet::new();
    for track in tracks {
        let is_first = seen_langs.insert(track.language.clone());
        if is_first && !track.is_default {
            emit_set_default(
                target,
                track,
                &format!(" (first for lang '{}')", track.language),
                ctx,
            );
        } else if !is_first && track.is_default {
            emit_clear_default(
                target,
                track,
                &format!(" (not first for lang '{}')", track.language),
                ctx,
            );
        }
    }
}

fn emit_defaults_all(target: TrackTarget, tracks: &[&Track], ctx: &mut PhaseContext) {
    for track in tracks {
        if !track.is_default {
            emit_set_default(target, track, "", ctx);
        }
    }
}

fn emit_set_defaults(defaults: &[CompiledDefault], ctx: &mut PhaseContext) {
    for default in defaults {
        let tracks = tracks_for_target(ctx.file, default.target);
        match default.strategy {
            DefaultStrategy::None => emit_defaults_none(default.target, &tracks, ctx),
            DefaultStrategy::First => emit_defaults_first(default.target, &tracks, ctx),
            DefaultStrategy::FirstPerLanguage => {
                emit_defaults_first_per_language(default.target, &tracks, ctx);
            }
            DefaultStrategy::All => emit_defaults_all(default.target, &tracks, ctx),
        }
    }
}

fn emit_clear_actions(
    target: TrackTarget,
    settings: &ClearActionsSettings,
    ctx: &mut PhaseContext,
) {
    let tracks = tracks_for_target(ctx.file, target);
    let clear_default = settings.clear_all_default;
    let clear_forced = settings.clear_all_forced;
    let clear_titles = settings.clear_all_titles;

    for track in &tracks {
        if clear_default && track.is_default {
            emit_clear_default(target, track, "", ctx);
        }
        if clear_forced && track.is_forced {
            emit_clear_forced(target, track, ctx);
        }
        if clear_titles && !track.title.is_empty() {
            emit_clear_title(target, track, ctx);
        }
    }
}

fn emit_transcode(
    target: TrackTarget,
    codec: &str,
    settings: &CompiledTranscodeSettings,
    ctx: &mut PhaseContext,
) {
    let tracks = tracks_for_target(ctx.file, target);

    let preserve = &settings.preserve;

    let operation = match target {
        TrackTarget::Video => OperationType::TranscodeVideo,
        TrackTarget::Audio => OperationType::TranscodeAudio,
        TrackTarget::Subtitle | TrackTarget::Attachment | TrackTarget::Any => {
            tracing::warn!(
                target = ?target,
                "transcode not supported for this track target, skipping"
            );
            return;
        }
    };

    // Check hw_fallback: if a specific HW backend is requested but unavailable,
    // and hw_fallback is false, skip the entire phase gracefully.
    if let Some(ref hw) = settings.hw {
        if hw != "auto" && hw != "none" {
            let hw_available = ctx
                .eval_ctx
                .capabilities
                .is_some_and(|caps| caps.has_hwaccel(hw));
            if !hw_available && settings.hw_fallback == Some(false) {
                ctx.plan.skip_reason = Some(format!(
                    "hw backend '{hw}' unavailable and hw_fallback is false"
                ));
                ctx.plan.actions.clear();
                return;
            }
        }
    }

    for track in &tracks {
        if track.codec == codec {
            continue;
        }
        if preserve.iter().any(|p| p == &track.codec) {
            continue;
        }

        ctx.plan.actions.push(PlannedAction::track_op(
            operation,
            track.index,
            ActionParams::Transcode {
                codec: codec.into(),
                settings: transcode_settings_from(settings),
            },
            format!(
                "Transcode {} track {} from {} to {codec}",
                target_str(target),
                track.index,
                track.codec
            ),
        ));
    }
}

fn emit_synthesize(synth: &CompiledSynthesize, ctx: &mut PhaseContext) {
    // Check create_if condition
    if let Some(ref cond) = synth.create_if {
        if !evaluate_condition(cond, ctx.file, ctx.eval_ctx) {
            return;
        }
    }

    let audio_tracks = ctx.file.audio_tracks();

    // Check skip_if_exists
    if let Some(ref skip_filter) = synth.skip_if_exists {
        if audio_tracks
            .iter()
            .any(|t| track_matches_with_context(t, skip_filter, ctx.file, ctx.eval_ctx))
        {
            return;
        }
    }

    // Find source track
    let source_index = if let Some(ref source_filter) = synth.source {
        audio_tracks
            .iter()
            .find(|t| track_matches_with_context(t, source_filter, ctx.file, ctx.eval_ctx))
            .map(|t| t.index)
    } else {
        audio_tracks.first().map(|t| t.index)
    };

    let language = match &synth.language {
        Some(SynthLanguage::Inherit) => source_index.and_then(|idx| {
            ctx.file
                .tracks
                .iter()
                .find(|t| t.index == idx)
                .map(|src| src.language.clone())
        }),
        Some(SynthLanguage::Fixed(l)) => Some(l.clone()),
        None => None,
    };

    let channels = synth.channels.as_ref().map(|c| {
        c.to_count().unwrap_or_else(|| {
            if let TranscodeChannels::Named(name) = c {
                tracing::warn!(preset = name, "unknown channel preset, defaulting to 2");
            }
            2
        })
    });

    let position = synth.position.as_ref().map(|p| match p {
        SynthPosition::Index(n) => n.to_string(),
        SynthPosition::Named(s) => s.clone(),
    });

    let params = ActionParams::Synthesize {
        name: synth.name.clone(),
        language,
        codec: synth.codec.clone(),
        text: None,
        bitrate: synth.bitrate.clone(),
        channels,
        title: synth.title.clone(),
        position,
        source_track: source_index,
    };
    let desc = format!("Synthesize audio: {}", synth.name);
    ctx.plan.actions.push(match source_index {
        Some(idx) => PlannedAction::track_op(OperationType::SynthesizeAudio, idx, params, desc),
        None => PlannedAction::file_op(OperationType::SynthesizeAudio, params, desc),
    });
}

fn emit_clear_tags(ctx: &mut PhaseContext) {
    if ctx.file.tags.is_empty() {
        return;
    }
    let mut tag_keys: Vec<String> = ctx.file.tags.keys().cloned().collect();
    tag_keys.sort();
    ctx.plan.actions.push(PlannedAction::file_op(
        OperationType::ClearContainerTags,
        ActionParams::ClearTags {
            tags: tag_keys.clone(),
        },
        format!("Clear all container tags ({})", tag_keys.join(", ")),
    ));
}

fn emit_set_tag(
    tag: &str,
    value: &CompiledValueOrField,
    ctx: &mut PhaseContext,
) -> Result<(), VoomError> {
    let val = resolve_value_or_field(value, ctx.file, ctx.eval_ctx)
        .ok_or_else(|| VoomError::Validation(format!("Cannot resolve tag value for '{tag}'")))?;
    ctx.plan.actions.push(PlannedAction::file_op(
        OperationType::SetContainerTag,
        ActionParams::SetTag {
            tag: tag.into(),
            value: val.clone(),
        },
        format!("Set container tag '{tag}' = '{val}'"),
    ));
    Ok(())
}

fn emit_delete_tag(tag: &str, ctx: &mut PhaseContext) {
    if ctx.file.tags.contains_key(tag) {
        ctx.plan.actions.push(PlannedAction::file_op(
            OperationType::DeleteContainerTag,
            ActionParams::DeleteTag { tag: tag.into() },
            format!("Delete container tag '{tag}'"),
        ));
    } else {
        tracing::debug!(tag, "delete_tag: tag not present in file, skipping");
    }
}

fn emit_conditional(cond: &CompiledConditional, ctx: &mut PhaseContext) -> Result<(), VoomError> {
    let matched = evaluate_condition(&cond.condition, ctx.file, ctx.eval_ctx);
    let actions = if matched {
        &cond.then_actions
    } else {
        &cond.else_actions
    };
    for action in actions {
        emit_action(action, ctx)?;
    }
    Ok(())
}

fn emit_rules(
    mode: RulesMode,
    rules: &[CompiledRule],
    ctx: &mut PhaseContext,
) -> Result<(), VoomError> {
    for rule in rules {
        let matched = evaluate_condition(&rule.conditional.condition, ctx.file, ctx.eval_ctx);
        if matched {
            for action in &rule.conditional.then_actions {
                emit_action(action, ctx)?;
            }
            if mode == RulesMode::First {
                break;
            }
        } else {
            for action in &rule.conditional.else_actions {
                emit_action(action, ctx)?;
            }
        }
    }
    Ok(())
}

fn emit_action(action: &CompiledAction, ctx: &mut PhaseContext) -> Result<(), VoomError> {
    match action {
        CompiledAction::Keep { target, filter } => {
            emit_keep(*target, filter.as_ref(), ctx);
        }
        CompiledAction::Remove { target, filter } => {
            emit_remove(*target, filter.as_ref(), ctx);
        }
        CompiledAction::Skip(phase) => {
            ctx.plan.skip_reason = Some(match phase {
                Some(p) => format!("Skipped by action (phase: {p})"),
                None => "Skipped by action".into(),
            });
        }
        CompiledAction::Warn(msg) => {
            let expanded = expand_template(msg, ctx.file);
            ctx.plan.warnings.push(expanded);
        }
        CompiledAction::Fail(msg) => {
            let expanded = expand_template(msg, ctx.file);
            return Err(VoomError::Validation(expanded));
        }
        CompiledAction::SetDefault { target, filter } => {
            emit_flag_action(ctx, *target, filter.as_ref(), FlagKind::Default);
        }
        CompiledAction::SetForced { target, filter } => {
            emit_flag_action(ctx, *target, filter.as_ref(), FlagKind::Forced);
        }
        CompiledAction::SetLanguage {
            target,
            filter,
            value,
        } => {
            let lang = resolve_value_or_field(value, ctx.file, ctx.eval_ctx).ok_or_else(|| {
                VoomError::Validation("Cannot resolve language value".to_string())
            })?;
            let tracks = tracks_for_target(ctx.file, *target);
            for track in &tracks {
                if filter_matches_ctx(track, filter.as_ref(), ctx.file, ctx.eval_ctx)
                    && track.language != lang
                {
                    ctx.plan.actions.push(PlannedAction::track_op(
                        OperationType::SetLanguage,
                        track.index,
                        ActionParams::Language {
                            language: lang.clone(),
                        },
                        format!(
                            "Set language on {} track {} to '{lang}'",
                            target_str(*target),
                            track.index
                        ),
                    ));
                }
            }
        }
        CompiledAction::SetTag { tag, value } => {
            emit_set_tag(tag, value, ctx)?;
        }
    }
    Ok(())
}

/// Expand `{filename}` and `{path}` templates in a string.
fn expand_template(template: &str, file: &MediaFile) -> String {
    template
        .replace("{filename}", &file_name(file))
        .replace("{path}", &file.path.to_string_lossy())
}

fn filter_matches_ctx(
    track: &Track,
    filter: Option<&CompiledFilter>,
    file: &MediaFile,
    eval_ctx: &EvalContext<'_>,
) -> bool {
    match filter {
        Some(f) => track_matches_with_context(track, f, file, eval_ctx),
        None => true,
    }
}

#[derive(Clone, Copy)]
enum FlagKind {
    Default,
    Forced,
}

fn emit_flag_action(
    ctx: &mut PhaseContext,
    target: TrackTarget,
    filter: Option<&CompiledFilter>,
    kind: FlagKind,
) {
    let (op, label, is_set_fn): (OperationType, &str, fn(&Track) -> bool) = match kind {
        FlagKind::Default => (OperationType::SetDefault, "default", |t| t.is_default),
        FlagKind::Forced => (OperationType::SetForced, "forced", |t| t.is_forced),
    };
    let tracks = tracks_for_target(ctx.file, target);
    for track in &tracks {
        if filter_matches_ctx(track, filter, ctx.file, ctx.eval_ctx) && !is_set_fn(track) {
            ctx.plan.actions.push(PlannedAction::track_op(
                op,
                track.index,
                ActionParams::Empty,
                format!(
                    "Set {label} on {} track {}",
                    target_str(target),
                    track.index
                ),
            ));
        }
    }
}

fn file_name(file: &MediaFile) -> String {
    file.path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn target_str(target: TrackTarget) -> &'static str {
    match target {
        TrackTarget::Video => "video",
        TrackTarget::Audio => "audio",
        TrackTarget::Subtitle => "subtitle",
        TrackTarget::Attachment => "attachment",
        TrackTarget::Any => "track",
    }
}

/// Validate plans against known executor capabilities, adding warnings
/// for unsupported codecs/formats and setting `executor_hint` when a
/// single executor clearly matches all operations.
///
/// Skips validation entirely when the capability map is empty (no
/// executors reported capabilities).
/// Check a single action's parameters against known capabilities,
/// pushing warnings and updating the running encoder intersection.
fn check_action_capabilities<'a>(
    action: &PlannedAction,
    capabilities: &'a CapabilityMap,
    all_encoders: &mut Option<HashSet<&'a str>>,
    warnings: &mut Vec<String>,
) {
    match &action.parameters {
        ActionParams::Transcode { codec, .. } => {
            let executors = capabilities.encoders_for(codec);
            if executors.is_empty() {
                warnings.push(format!("No executor supports encoder '{codec}'"));
            }
            intersect_executors(all_encoders, &executors);
        }
        ActionParams::Synthesize { codec: Some(c), .. } => {
            let executors = capabilities.encoders_for(c);
            if executors.is_empty() {
                warnings.push(format!("No executor supports encoder '{c}'"));
            }
            intersect_executors(all_encoders, &executors);
        }
        ActionParams::Container { container } => {
            let fmt = container.ffmpeg_format_name().unwrap_or(container.as_str());
            if !capabilities.has_format(fmt) {
                warnings.push(format!("No executor supports format '{fmt}'"));
            }
        }
        _ => {}
    }
}

fn apply_capability_hints(plans: &mut [Plan], capabilities: &CapabilityMap) {
    if capabilities.is_empty() {
        return;
    }

    for plan in plans.iter_mut() {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }

        let mut all_encoders: Option<HashSet<&str>> = None;
        let mut warnings: Vec<String> = Vec::new();

        for action in &plan.actions {
            check_action_capabilities(action, capabilities, &mut all_encoders, &mut warnings);
        }

        plan.warnings.extend(warnings);

        if let Some(ref candidates) = all_encoders {
            if candidates.len() == 1 {
                plan.executor_hint = candidates.iter().next().map(|s| (*s).to_string());
            }
        }
    }
}

/// Intersect the running set of candidate executors with a new set.
fn intersect_executors<'a>(running: &mut Option<HashSet<&'a str>>, new: &[&'a str]) {
    let new_set: HashSet<&str> = new.iter().copied().collect();
    match running {
        Some(existing) => {
            existing.retain(|e| new_set.contains(e));
        }
        None => {
            *running = Some(new_set);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};

    fn test_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/Movie.mkv"));
        file.container = Container::Mkv;
        file.tracks = vec![
            {
                let mut t = Track::new(0, TrackType::Video, "hevc".into());
                t.width = Some(1920);
                t.height = Some(1080);
                t
            },
            {
                let mut t = Track::new(1, TrackType::AudioMain, "dts_hd".into());
                t.language = "eng".into();
                t.channels = Some(8);
                t.is_default = true;
                t.title = "DTS-HD MA 7.1".into();
                t
            },
            {
                let mut t = Track::new(2, TrackType::AudioAlternate, "aac".into());
                t.language = "jpn".into();
                t.channels = Some(2);
                t
            },
            {
                let mut t = Track::new(3, TrackType::AudioCommentary, "aac".into());
                t.language = "eng".into();
                t.channels = Some(2);
                t.title = "Director's Commentary".into();
                t
            },
            {
                let mut t = Track::new(4, TrackType::SubtitleMain, "srt".into());
                t.language = "eng".into();
                t.is_default = true;
                t
            },
            {
                let mut t = Track::new(5, TrackType::SubtitleCommentary, "srt".into());
                t.language = "eng".into();
                t.title = "Commentary".into();
                t
            },
            Track::new(6, TrackType::Attachment, "font/ttf".into()),
            {
                let mut t = Track::new(7, TrackType::Attachment, "image/jpeg".into());
                t.title = "cover.jpg".into();
                t
            },
        ];
        file
    }

    fn evaluate_with_caps(
        policy: &CompiledPolicy,
        file: &MediaFile,
        capabilities: &CapabilityMap,
    ) -> EvaluationResult {
        evaluate_with_evaluation_context(
            policy,
            file,
            EvaluationContext {
                capabilities: Some(capabilities),
                phase_output_lookup: None,
            },
        )
    }

    fn test_policy(source: &str) -> CompiledPolicy {
        voom_dsl::compile_policy(source).expect("Failed to compile test policy")
    }

    #[test]
    fn test_container_conversion() {
        let mut file = test_file();
        file.container = Container::Mp4;
        let policy = test_policy(r#"policy "test" { phase init { container mkv } }"#);
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].actions.len(), 1);
        assert_eq!(
            result.plans[0].actions[0].operation,
            OperationType::ConvertContainer
        );
    }

    #[test]
    fn test_container_no_change() {
        let file = test_file(); // already MKV
        let policy = test_policy(r#"policy "test" { phase init { container mkv } }"#);
        let result = evaluate(&policy, &file);
        assert!(result.plans[0].actions.is_empty());
    }

    #[test]
    fn test_keep_audio_by_language() {
        let file = test_file();
        let policy =
            test_policy(r#"policy "test" { phase norm { keep audio where lang in [eng] } }"#);
        let result = evaluate(&policy, &file);
        // Should remove jpn audio (track 2), keep eng tracks 1 and 3
        let remove_actions: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        assert_eq!(remove_actions.len(), 1);
        assert_eq!(remove_actions[0].track_index, Some(2));
    }

    #[test]
    fn test_keep_subtitles_not_commentary() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" { phase norm { keep subtitles where lang in [eng] and not commentary } }"#,
        );
        let result = evaluate(&policy, &file);
        let removes: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        // Track 5 is commentary, should be removed
        assert_eq!(removes.len(), 1);
        assert_eq!(removes[0].track_index, Some(5));
    }

    #[test]
    fn test_remove_attachments_not_font() {
        let file = test_file();
        let policy =
            test_policy(r#"policy "test" { phase norm { remove attachments where not font } }"#);
        let result = evaluate(&policy, &file);
        let removes: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        // Track 7 (image/jpeg) should be removed, track 6 (font/ttf) kept
        assert_eq!(removes.len(), 1);
        assert_eq!(removes[0].track_index, Some(7));
    }

    #[test]
    fn test_clear_actions() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase norm {
                    audio actions {
                        clear_all_default: true
                        clear_all_titles: true
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // Track 1 has default=true and title set
        // Track 3 has title set
        let clear_defaults: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::ClearDefault)
            .collect();
        assert_eq!(clear_defaults.len(), 1); // only track 1 is default

        let set_titles: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::SetTitle)
            .collect();
        assert_eq!(set_titles.len(), 2); // tracks 1 and 3 have titles
    }

    #[test]
    fn test_defaults_first_per_language() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase norm {
                    defaults {
                        audio: first_per_language
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // Track 1: eng, default=true → no change (first eng)
        // Track 2: jpn, default=false → set default (first jpn)
        // Track 3: eng, default=false → no change (not first eng, not default)
        let set_defaults: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::SetDefault)
            .collect();
        assert_eq!(set_defaults.len(), 1);
        assert_eq!(set_defaults[0].track_index, Some(2));
    }

    #[test]
    fn test_defaults_none() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase norm {
                    defaults { subtitle: none }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // Track 4: default=true → clear
        let clears: Vec<_> = result.plans[0]
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::ClearDefault)
            .collect();
        assert_eq!(clears.len(), 1);
        assert_eq!(clears[0].track_index, Some(4));
    }

    #[test]
    fn test_skip_when_condition() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase tc {
                    skip when video.codec == "hevc"
                    transcode video to hevc { crf: 20 }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert!(result.plans[0].is_skipped());
        assert!(result.plans[0].actions.is_empty());
    }

    #[test]
    fn test_transcode_video() {
        let mut file = test_file();
        file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
        let policy = test_policy(
            r#"policy "test" {
                phase tc {
                    transcode video to hevc { crf: 20 }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans[0].actions.len(), 1);
        assert_eq!(
            result.plans[0].actions[0].operation,
            OperationType::TranscodeVideo
        );
    }

    #[test]
    fn test_transcode_audio_with_preserve() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase tc {
                    transcode audio to aac {
                        preserve: [dts_hd]
                        bitrate: "192k"
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // Track 1: dts_hd → preserved
        // Track 2: aac → already target codec, skip
        // Track 3: aac → already target codec, skip
        assert!(result.plans[0].actions.is_empty());
    }

    #[test]
    fn test_conditional_warn() {
        let file = test_file(); // has jpn audio + eng subs
        let policy = test_policy(
            r#"policy "test" {
                phase validate {
                    when exists(audio where lang == jpn) {
                        warn "Japanese audio found in {filename}"
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans[0].warnings.len(), 1);
        assert!(result.plans[0].warnings[0].contains("Movie.mkv"));
    }

    #[test]
    fn test_conditional_else() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase validate {
                    when exists(audio where lang == fre) {
                        warn "has french"
                    } else {
                        warn "no french audio"
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans[0].warnings.len(), 1);
        assert!(result.plans[0].warnings[0].contains("no french"));
    }

    #[test]
    fn test_rules_first_match() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase validate {
                    rules first {
                        rule "multi-lang" {
                            when audio_is_multi_language {
                                warn "multiple languages"
                            }
                        }
                        rule "single-lang" {
                            when not audio_is_multi_language {
                                warn "single language"
                            }
                        }
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // first-match-wins: multi-lang matches first
        assert_eq!(result.plans[0].warnings.len(), 1);
        assert!(result.plans[0].warnings[0].contains("multiple"));
    }

    #[test]
    fn test_phase_dependencies() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase a { container mkv }
                phase b {
                    depends_on: [a]
                    container mkv
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 2);
        // Both should run (a first, then b)
        assert!(!result.plans[0].is_skipped());
        assert!(!result.plans[1].is_skipped());
    }

    #[test]
    fn test_run_if_modified_skips_when_not_modified() {
        let file = test_file(); // already MKV
        let policy = test_policy(
            r#"policy "test" {
                phase containerize { container mkv }
                phase validate {
                    depends_on: [containerize]
                    run_if containerize.modified
                    when exists(audio where lang == eng) { warn "has eng" }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // containerize produces no actions (already MKV), so not modified
        assert!(result.plans[1].is_skipped());
        assert!(result.plans[1]
            .skip_reason
            .as_ref()
            .unwrap()
            .contains("run_if"));
    }

    #[test]
    fn test_run_if_modified_runs_when_modified() {
        let mut file = test_file();
        file.container = Container::Mp4; // needs conversion
        let policy = test_policy(
            r#"policy "test" {
                phase containerize { container mkv }
                phase validate {
                    depends_on: [containerize]
                    run_if containerize.modified
                    when exists(audio where lang == eng) { warn "has eng" }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // containerize produces convert action, so modified
        assert!(!result.plans[1].is_skipped());
        assert_eq!(result.plans[1].warnings.len(), 1);
    }

    #[test]
    fn test_skipped_phase_does_not_block_depends_on() {
        let file = test_file(); // has hevc video
        let policy = test_policy(
            r#"policy "test" {
                phase tc {
                    skip when video.codec == "hevc"
                    transcode video to hevc { crf: 20 }
                }
                phase cleanup {
                    depends_on: [tc]
                    when exists(audio where lang == eng) { warn "has eng" }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert!(result.plans[0].is_skipped(), "tc should be skipped");
        // cleanup depends_on tc, but tc was skipped (not missing) — should still run
        assert!(!result.plans[1].is_skipped(), "cleanup should run");
        assert_eq!(result.plans[1].warnings.len(), 1);
    }

    #[test]
    fn test_skipped_phase_blocks_run_if_completed() {
        let file = test_file(); // has hevc video
        let policy = test_policy(
            r#"policy "test" {
                phase tc {
                    skip when video.codec == "hevc"
                    transcode video to hevc { crf: 20 }
                }
                phase post_tc {
                    depends_on: [tc]
                    run_if tc.completed
                    when exists(audio where lang == eng) { warn "has eng" }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert!(result.plans[0].is_skipped(), "tc should be skipped");
        // post_tc has run_if tc.completed — tc was skipped not completed, so post_tc skips
        assert!(result.plans[1].is_skipped(), "post_tc should be skipped");
        assert!(result.plans[1]
            .skip_reason
            .as_ref()
            .expect("skip reason")
            .contains("run_if"));
    }

    #[test]
    fn test_synthesize_basic() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase synth {
                    synthesize "Stereo AAC" {
                        codec: aac
                        channels: stereo
                        bitrate: "192k"
                        title: "Stereo (AAC)"
                        language: inherit
                        position: after_source
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans[0].actions.len(), 1);
        assert_eq!(
            result.plans[0].actions[0].operation,
            OperationType::SynthesizeAudio
        );
    }

    #[test]
    fn test_synthesize_skip_if_exists() {
        let file = test_file(); // has aac 2ch tracks
        let policy = test_policy(
            r#"policy "test" {
                phase synth {
                    synthesize "Stereo AAC" {
                        codec: aac
                        channels: stereo
                        skip_if_exists { codec in [aac] and channels == 2 and not commentary }
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // Track 2 is aac, 2ch, not commentary → skip
        assert!(result.plans[0].actions.is_empty());
    }

    #[test]
    fn test_reorder_tracks() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                phase norm {
                    order tracks [video, audio_main, subtitle_main]
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans[0].actions.len(), 1);
        assert_eq!(
            result.plans[0].actions[0].operation,
            OperationType::ReorderTracks
        );
    }

    #[test]
    fn test_fail_action_with_error_strategy() {
        let file = test_file();
        let policy = test_policy(
            r#"policy "test" {
                config { on_error: continue }
                phase validate {
                    when audio_is_multi_language {
                        fail "Multi-language not allowed"
                    }
                    when exists(audio where lang == eng) {
                        warn "has english"
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        // on_error: continue → error recorded but processing continues
        assert!(!result.plans[0].warnings.is_empty());
    }

    #[test]
    fn test_full_production_policy() {
        let source =
            include_str!("../../../crates/voom-dsl/tests/fixtures/production-normalize.voom");
        let policy = voom_dsl::compile_policy(source).unwrap();
        let file = test_file();
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 6);

        // Check that plans have reasonable structure
        for plan in &result.plans {
            assert_eq!(plan.policy_name, "production-normalize");
            assert!(!plan.phase_name.is_empty());
        }
    }

    #[test]
    fn test_clear_tags_with_tags() {
        let mut file = test_file();
        file.tags.insert("title".into(), "Old Title".into());
        file.tags.insert("encoder".into(), "HandBrake".into());

        let policy = test_policy(
            r#"policy "test" {
            phase clean {
                clear_tags
            }
        }"#,
        );

        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].operation, OperationType::ClearContainerTags);
        match &plan.actions[0].parameters {
            ActionParams::ClearTags { tags } => assert_eq!(tags.len(), 2),
            other => panic!("Expected ClearTags, got {other:?}"),
        }
    }

    #[test]
    fn test_clear_tags_without_tags() {
        let file = test_file();

        let policy = test_policy(
            r#"policy "test" {
            phase clean {
                clear_tags
            }
        }"#,
        );

        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert!(plan.actions.is_empty(), "no actions when no tags exist");
    }

    #[test]
    fn test_set_tag_operation() {
        let file = test_file();

        let policy = test_policy(
            r#"policy "test" {
            phase clean {
                set_tag "title" "My Movie"
            }
        }"#,
        );

        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].operation, OperationType::SetContainerTag);
        match &plan.actions[0].parameters {
            ActionParams::SetTag { tag, value } => {
                assert_eq!(tag, "title");
                assert_eq!(value, "My Movie");
            }
            other => panic!("Expected SetTag, got {other:?}"),
        }
    }

    #[test]
    fn test_delete_tag_existing() {
        let mut file = test_file();
        file.tags.insert("encoder".into(), "HandBrake".into());

        let policy = test_policy(
            r#"policy "test" {
            phase clean {
                delete_tag "encoder"
            }
        }"#,
        );

        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].operation, OperationType::DeleteContainerTag);
        match &plan.actions[0].parameters {
            ActionParams::DeleteTag { tag } => assert_eq!(tag, "encoder"),
            other => panic!("Expected DeleteTag, got {other:?}"),
        }
    }

    #[test]
    fn test_delete_tag_nonexistent() {
        let file = test_file();

        let policy = test_policy(
            r#"policy "test" {
            phase clean {
                delete_tag "nonexistent"
            }
        }"#,
        );

        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert!(plan.actions.is_empty(), "no action when tag doesn't exist");
    }

    #[test]
    fn test_combined_container_metadata_ordering() {
        let mut file = test_file();
        file.tags.insert("title".into(), "Old".into());
        file.tags.insert("encoder".into(), "x".into());

        let policy = test_policy(
            r#"policy "test" {
            phase clean {
                clear_tags
                set_tag "title" "New Title"
                delete_tag "encoder"
            }
        }"#,
        );

        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        // clear_tags produces 1 action, set_tag produces 1, delete_tag produces 1 (tag exists on file)
        assert_eq!(plan.actions.len(), 3);
        assert_eq!(plan.actions[0].operation, OperationType::ClearContainerTags);
        assert_eq!(plan.actions[1].operation, OperationType::SetContainerTag);
        assert_eq!(plan.actions[2].operation, OperationType::DeleteContainerTag);
    }

    #[test]
    fn test_convert_container_parameter_key_is_container() {
        // Use MP4-compatible codecs so the ContainerIncompatible safeguard
        // doesn't retract the action — this test is about parameter shape.
        let mut file = test_file();
        file.container = Container::Mkv;
        file.tracks = vec![
            Track::new(0, TrackType::Video, "h264".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
        ];
        let policy = test_policy(r#"policy "test" { phase init { container mp4 } }"#);
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].actions.len(), 1);
        let action = &result.plans[0].actions[0];
        assert_eq!(action.operation, OperationType::ConvertContainer);
        // Verify the parameter uses Container variant (regression guard)
        match &action.parameters {
            ActionParams::Container { container } => {
                assert_eq!(
                    *container,
                    voom_domain::media::Container::Mp4,
                    "ConvertContainer action must specify container"
                );
            }
            other => panic!("Expected Container params, got {other:?}"),
        }
    }

    // --- Safeguard tests ---

    #[test]
    fn test_keep_safeguard_retracts_when_all_tracks_removed() {
        let file = test_file();
        let policy =
            test_policy(r#"policy "test" { phase norm { keep audio where lang in [fre] } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        // Safeguard should retract all RemoveTrack actions
        let removes: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        assert_eq!(removes.len(), 0, "safeguard should retract remove actions");
        // Should have a safeguard violation
        assert_eq!(plan.safeguard_violations.len(), 1);
        assert_eq!(
            plan.safeguard_violations[0].kind,
            voom_domain::SafeguardKind::AllTracksRemoved
        );
        // Warning should also be present
        assert!(
            plan.warnings.iter().any(|w| w.contains("Safeguard")),
            "Expected safeguard warning, got: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn test_keep_no_safeguard_when_some_tracks_kept() {
        let file = test_file();
        let policy =
            test_policy(r#"policy "test" { phase norm { keep audio where lang in [eng] } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert!(
            plan.safeguard_violations.is_empty(),
            "Should not trigger safeguard when some tracks are kept"
        );
        // jpn audio (track 2) should still be removed
        let removes: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        assert_eq!(removes.len(), 1);
    }

    #[test]
    fn test_remove_safeguard_retracts_for_critical_tracks() {
        let file = test_file();
        // Remove all audio — safeguard should prevent this
        let policy = test_policy(
            r#"policy "test" { phase norm { remove audio where lang in [eng, jpn] } }"#,
        );
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        let removes: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        assert_eq!(removes.len(), 0, "safeguard should retract audio removes");
        assert_eq!(plan.safeguard_violations.len(), 1);
    }

    #[test]
    fn test_remove_no_safeguard_for_non_critical_tracks() {
        let file = test_file();
        // Remove all subtitles — subtitles are not critical, should proceed
        let policy =
            test_policy(r#"policy "test" { phase norm { remove subtitles where lang in [eng] } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        let removes: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        assert_eq!(removes.len(), 2, "non-critical tracks can be fully removed");
        assert!(plan.safeguard_violations.is_empty());
    }

    #[test]
    fn test_remove_no_safeguard_when_some_tracks_kept() {
        let file = test_file();
        // Remove only commentary audio — non-commentary tracks should remain
        let policy =
            test_policy(r#"policy "test" { phase norm { remove audio where commentary } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert!(
            plan.safeguard_violations.is_empty(),
            "Should not trigger safeguard when some tracks are kept"
        );
    }

    #[test]
    fn test_apply_safeguards_no_safeguard_when_tracks_survive() {
        let file = test_file();
        // keep audio where lang in [eng] — track 1 (eng) and track 3 (eng commentary) kept
        // track 2 (jpn) removed. Multiple tracks survive, no safeguard.
        let policy =
            test_policy(r#"policy "test" { phase norm { keep audio where lang in [eng] } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert!(
            plan.safeguard_violations.is_empty(),
            "No safeguard when tracks survive"
        );
    }

    #[test]
    fn test_apply_safeguards_retracts_when_all_video_removed() {
        // File with one video track — removing it should trigger safeguard
        let mut file = test_file();
        file.tracks[0] = {
            let mut t = Track::new(0, TrackType::Video, "h264".into());
            t.language = "und".into();
            t
        };
        let policy = test_policy(
            r#"policy "test" {
                phase norm {
                    remove video where codec == h264
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        // apply_safeguards should catch this since emit_remove doesn't guard video
        // (emit_remove guards audio/video at the per-operation level)
        let removes: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| a.operation == OperationType::RemoveTrack)
            .collect();
        assert_eq!(removes.len(), 0, "safeguard should retract video removal");
        assert!(plan
            .safeguard_violations
            .iter()
            .any(|v| v.kind == voom_domain::SafeguardKind::NoVideoTrack
                || v.kind == voom_domain::SafeguardKind::AllTracksRemoved));
    }

    // --- ContainerIncompatible safeguard tests ---

    /// Build a file whose container/tracks we control for container-safeguard tests.
    fn container_test_file(container: Container, tracks: Vec<Track>) -> MediaFile {
        let ext = container.as_str();
        let mut file = MediaFile::new(PathBuf::from(format!("/media/Sample.{ext}")));
        file.container = container;
        file.tracks = tracks;
        file
    }

    #[test]
    fn test_container_safeguard_blocks_webm_with_ac3() {
        // MKV with vp9 video + AC3 audio → policy wants WebM. AC3 is not a
        // WebM codec, so the conversion must be retracted.
        let file = container_test_file(
            Container::Mkv,
            vec![
                Track::new(0, TrackType::Video, "vp9".into()),
                Track::new(1, TrackType::AudioMain, "ac3".into()),
            ],
        );
        let policy = test_policy(r#"policy "p" { phase init { container webm } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should be retracted"
        );
        assert!(plan
            .safeguard_violations
            .iter()
            .any(|v| v.kind == SafeguardKind::ContainerIncompatible));
    }

    #[test]
    fn test_container_safeguard_allows_mp4_with_compatible_codecs() {
        let file = container_test_file(
            Container::Mkv,
            vec![
                Track::new(0, TrackType::Video, "h264".into()),
                Track::new(1, TrackType::AudioMain, "aac".into()),
            ],
        );
        let policy = test_policy(r#"policy "p" { phase init { container mp4 } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            plan.actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should remain"
        );
        assert!(plan
            .safeguard_violations
            .iter()
            .all(|v| v.kind != SafeguardKind::ContainerIncompatible));
    }

    #[test]
    fn test_container_safeguard_noop_when_no_conversion() {
        // Target container equals source → no ConvertContainer action → no check.
        let file = container_test_file(
            Container::Mkv,
            vec![Track::new(0, TrackType::Video, "opus".into())],
        );
        let policy = test_policy(r#"policy "p" { phase init { container mkv } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];
        assert!(plan
            .safeguard_violations
            .iter()
            .all(|v| v.kind != SafeguardKind::ContainerIncompatible));
    }

    #[test]
    fn test_container_safeguard_ignores_removed_tracks() {
        // Offending track is removed by the policy, so it doesn't count as
        // surviving and the conversion is fine.
        let file = container_test_file(
            Container::Mkv,
            vec![
                Track::new(0, TrackType::Video, "h264".into()),
                Track::new(1, TrackType::AudioMain, "aac".into()),
                {
                    let mut t = Track::new(2, TrackType::AudioAlternate, "dts".into());
                    t.language = "jpn".into();
                    t
                },
            ],
        );
        let policy = test_policy(
            r#"policy "p" {
                phase init {
                    container mp4
                    remove audio where codec == dts
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            plan.actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should remain — dts track is removed"
        );
        assert!(plan
            .safeguard_violations
            .iter()
            .all(|v| v.kind != SafeguardKind::ContainerIncompatible));
    }

    #[test]
    fn test_container_safeguard_uses_post_transcode_codec() {
        // File has a DTS audio track (not MP4-compatible), but policy
        // transcodes it to AAC before container conversion. No violation.
        let file = container_test_file(
            Container::Mkv,
            vec![Track::new(0, TrackType::Video, "h264".into()), {
                let mut t = Track::new(1, TrackType::AudioMain, "dts".into());
                t.language = "eng".into();
                t
            }],
        );
        let policy = test_policy(
            r#"policy "p" {
                phase init {
                    container mp4
                    transcode audio to aac {}
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            plan.actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should remain — dts is transcoded to aac"
        );
        assert!(plan
            .safeguard_violations
            .iter()
            .all(|v| v.kind != SafeguardKind::ContainerIncompatible));
    }

    #[test]
    fn test_container_safeguard_blocks_webm_with_synthesized_aac() {
        // MKV with vp9 + opus → policy converts to WebM AND synthesizes an
        // AAC track. AAC is not WebM-compatible, so the conversion must be
        // retracted and a violation recorded.
        let file = container_test_file(
            Container::Mkv,
            vec![
                Track::new(0, TrackType::Video, "vp9".into()),
                Track::new(1, TrackType::AudioMain, "opus".into()),
            ],
        );
        let policy = test_policy(
            r#"policy "p" {
                phase init {
                    container webm
                    synthesize "Stereo AAC" {
                        codec: aac
                        channels: stereo
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should be retracted — synthesized AAC is not WebM-compatible"
        );
        let violation = plan
            .safeguard_violations
            .iter()
            .find(|v| v.kind == SafeguardKind::ContainerIncompatible)
            .expect("expected ContainerIncompatible violation");
        assert!(
            violation.message.contains("synthesized"),
            "violation message should call out the synthesized track: {}",
            violation.message
        );
    }

    #[test]
    fn test_container_safeguard_allows_webm_with_synthesized_opus() {
        // MKV with vp9 + opus → policy converts to WebM AND synthesizes an
        // Opus track. Opus is WebM-compatible, so no violation.
        let file = container_test_file(
            Container::Mkv,
            vec![
                Track::new(0, TrackType::Video, "vp9".into()),
                Track::new(1, TrackType::AudioMain, "opus".into()),
            ],
        );
        let policy = test_policy(
            r#"policy "p" {
                phase init {
                    container webm
                    synthesize "Stereo Opus" {
                        codec: opus
                        channels: stereo
                    }
                }
            }"#,
        );
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            plan.actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should remain — synthesized Opus is WebM-compatible"
        );
        assert!(plan
            .safeguard_violations
            .iter()
            .all(|v| v.kind != SafeguardKind::ContainerIncompatible));
    }

    #[test]
    fn test_container_safeguard_skips_unmodeled_target() {
        // `mov` is not modeled — safeguard must not produce false positives.
        let file = container_test_file(
            Container::Mkv,
            vec![Track::new(0, TrackType::Video, "hevc".into())],
        );
        let policy = test_policy(r#"policy "p" { phase init { container mov } }"#);
        let result = evaluate(&policy, &file);
        let plan = &result.plans[0];

        assert!(
            plan.actions
                .iter()
                .any(|a| a.operation == OperationType::ConvertContainer),
            "ConvertContainer should remain — mov is unmodeled"
        );
        assert!(plan
            .safeguard_violations
            .iter()
            .all(|v| v.kind != SafeguardKind::ContainerIncompatible));
    }

    // --- Verify operation tests ---

    #[test]
    fn verify_op_produces_verify_media_action() {
        let policy = test_policy(r#"policy "p" { phase v { verify quick } }"#);
        let file = MediaFile::new(std::path::PathBuf::from("/m/x.mkv"));
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 1);
        let plan = &result.plans[0];
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].operation, OperationType::VerifyMedia);
    }

    #[test]
    fn verify_op_carries_mode_in_params() {
        use voom_domain::verification::VerificationMode;
        let policy = test_policy(r#"policy "p" { phase v { verify thorough } }"#);
        let file = MediaFile::new(std::path::PathBuf::from("/m/x.mkv"));
        let result = evaluate(&policy, &file);
        let action = &result.plans[0].actions[0];
        assert_eq!(action.operation, OperationType::VerifyMedia);
        match &action.parameters {
            ActionParams::VerifyMedia(p) => assert_eq!(p.mode, VerificationMode::Thorough),
            other => panic!("expected VerifyMedia params, got {other:?}"),
        }
    }

    #[test]
    fn verify_op_supports_hash_mode() {
        use voom_domain::verification::VerificationMode;
        let policy = test_policy(r#"policy "p" { phase v { verify hash } }"#);
        let file = MediaFile::new(std::path::PathBuf::from("/m/x.mkv"));
        let result = evaluate(&policy, &file);
        let action = &result.plans[0].actions[0];
        match &action.parameters {
            ActionParams::VerifyMedia(p) => assert_eq!(p.mode, VerificationMode::Hash),
            other => panic!("expected VerifyMedia params, got {other:?}"),
        }
    }

    // --- Capability validation tests ---

    mod capability_validation {
        use super::*;
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

        fn ffmpeg_codecs() -> CodecCapabilities {
            CodecCapabilities::new(
                vec!["h264".into(), "hevc".into(), "aac".into()],
                vec!["h264".into(), "hevc".into(), "aac".into()],
            )
        }

        /// Realistic ffmpeg muxer set covering every reachable `Container`
        /// variant via `Container::ffmpeg_format_name()`. Update this list
        /// whenever a new `Container` variant is added so the fixture
        /// stays in sync with the domain enum.
        fn ffmpeg_capabilities() -> CapabilityMap {
            let mut map = CapabilityMap::new();
            map.register(ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                ffmpeg_codecs(),
                vec![
                    "matroska".into(),
                    "mp4".into(),
                    "webm".into(),
                    "avi".into(),
                    "mov".into(),
                    "mpegts".into(),
                    "asf".into(),
                    "flv".into(),
                ],
                vec![],
            ));
            map
        }

        /// Deliberately narrow fixture used only by tests that need to
        /// exercise the "format not supported" warning path. Keep this
        /// list limited; real ffmpeg supports many more muxers — see
        /// `ffmpeg_capabilities()` for the realistic set.
        fn limited_ffmpeg_capabilities() -> CapabilityMap {
            let mut map = CapabilityMap::new();
            map.register(ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                ffmpeg_codecs(),
                vec!["matroska".into(), "mp4".into()],
                vec![],
            ));
            map
        }

        #[test]
        fn test_warns_on_unsupported_transcode_codec() {
            let file = test_file();
            let policy = test_policy(
                r#"policy "test" {
                    phase tc { transcode audio to opus {} }
                }"#,
            );
            let caps = ffmpeg_capabilities();
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert!(
                result.plans[0].warnings.iter().any(|w| w.contains("opus")),
                "Expected warning about unsupported codec 'opus', got: {:?}",
                result.plans[0].warnings
            );
        }

        #[test]
        fn test_no_warning_when_codec_supported() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc { transcode video to hevc { crf: 20 } }
                }"#,
            );
            let caps = ffmpeg_capabilities();
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert!(
                !result.plans[0]
                    .warnings
                    .iter()
                    .any(|w| w.contains("No executor")),
                "Should not warn when codec is supported"
            );
        }

        #[test]
        fn test_warns_on_unsupported_container_format() {
            // Tracks are all WebM-compatible so the ContainerIncompatible
            // safeguard leaves the ConvertContainer in place; the
            // capability-hints layer is what should surface the warning
            // here. Use the deliberately narrow `limited_ffmpeg_capabilities`
            // fixture, which intentionally omits webm so we can exercise
            // the "format not supported" warning path. Real ffmpeg
            // absolutely supports webm — see `ffmpeg_capabilities()`.
            let mut file = test_file();
            file.tracks = vec![
                Track::new(0, TrackType::Video, "vp9".into()),
                Track::new(1, TrackType::AudioMain, "opus".into()),
            ];
            let policy = test_policy(r#"policy "test" { phase init { container webm } }"#);
            let caps = limited_ffmpeg_capabilities();
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert!(
                result.plans[0]
                    .warnings
                    .iter()
                    .any(|w| w.contains("format") && w.contains("webm")),
                "Expected warning about unsupported format 'webm', got: {:?}",
                result.plans[0].warnings
            );
        }

        #[test]
        fn test_warns_on_unsupported_synthesize_codec() {
            let file = test_file();
            let policy = test_policy(
                r#"policy "test" {
                    phase synth {
                        synthesize "Stereo" {
                            codec: opus
                            channels: stereo
                        }
                    }
                }"#,
            );
            let caps = ffmpeg_capabilities();
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert!(
                result.plans[0].warnings.iter().any(|w| w.contains("opus")),
                "Expected warning about unsupported synthesize codec, got: {:?}",
                result.plans[0].warnings
            );
        }

        #[test]
        fn test_skipped_and_empty_plans_not_validated() {
            let file = test_file();
            // container mkv on a file already in mkv = empty plan
            let policy = test_policy(
                r#"policy "test" {
                    phase a { container mkv }
                }"#,
            );
            let result = evaluate(&policy, &file);
            assert!(result.plans[0].is_empty());

            // Use an empty capability map — would cause warnings if validation ran
            let caps = CapabilityMap::new();
            let result = evaluate_with_caps(&policy, &file, &caps);
            assert!(result.plans[0].warnings.is_empty());
        }

        #[test]
        fn test_empty_capability_map_skips_validation() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy =
                test_policy(r#"policy "test" { phase tc { transcode video to hevc {} } }"#);
            let result = evaluate(&policy, &file);
            assert!(!result.plans[0].is_empty());

            let caps = CapabilityMap::new();
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert!(
                !result.plans[0]
                    .warnings
                    .iter()
                    .any(|w| w.contains("No executor")),
                "Empty capability map should skip validation entirely"
            );
        }

        #[test]
        fn test_executor_hint_set_when_single_executor_matches() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc { transcode video to hevc { crf: 20 } }
                }"#,
            );
            let caps = ffmpeg_capabilities();
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert_eq!(
                result.plans[0].executor_hint.as_deref(),
                Some("ffmpeg-executor"),
                "Should set executor_hint when a single executor matches"
            );
        }

        #[test]
        fn test_mkv_container_uses_matroska_format_name() {
            let mut file = test_file();
            file.container = Container::Mkv;
            let policy = test_policy(r#"policy "test" { phase init { container mkv } }"#);
            let result = evaluate(&policy, &file);
            // File is already MKV so no convert action, but verify the mapping
            // works by converting an MP4 file
            file.container = Container::Mp4;
            let caps = ffmpeg_capabilities();
            let result2 = evaluate_with_caps(&policy, &file, &caps);
            // ffmpeg_capabilities includes "matroska", and Container::Mkv
            // maps to "matroska" via ffmpeg_format_name(), so no warning
            assert!(
                !result2.plans[0]
                    .warnings
                    .iter()
                    .any(|w| w.contains("No executor")),
                "MKV should map to 'matroska' and be supported, warnings: {:?}",
                result2.plans[0].warnings
            );
            // Also verify the original result has no actions (already MKV)
            assert!(result.plans[0].actions.is_empty());
        }

        #[test]
        fn test_executor_hint_not_set_with_multiple_matches() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc { transcode video to hevc { crf: 20 } }
                }"#,
            );
            let mut caps = CapabilityMap::new();
            caps.register(ExecutorCapabilitiesEvent::new(
                "executor-a",
                CodecCapabilities::new(vec![], vec!["hevc".into()]),
                vec![],
                vec![],
            ));
            caps.register(ExecutorCapabilitiesEvent::new(
                "executor-b",
                CodecCapabilities::new(vec![], vec!["hevc".into()]),
                vec![],
                vec![],
            ));
            let result = evaluate_with_caps(&policy, &file, &caps);

            assert!(
                result.plans[0].executor_hint.is_none(),
                "Should not set hint when multiple executors match"
            );
        }
    }

    mod hw_fallback {
        use super::*;
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

        fn caps_without_gpu() -> CapabilityMap {
            let mut map = CapabilityMap::new();
            map.register(ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                CodecCapabilities::new(
                    vec!["h264".into(), "hevc".into()],
                    vec!["h264".into(), "hevc".into()],
                ),
                vec!["matroska".into()],
                vec![], // no hw_accels
            ));
            map
        }

        fn caps_with_nvenc() -> CapabilityMap {
            let mut map = CapabilityMap::new();
            map.register(ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                CodecCapabilities::new(
                    vec!["h264".into(), "hevc".into()],
                    vec!["h264".into(), "hevc".into()],
                ),
                vec!["matroska".into()],
                vec!["cuda".into()],
            ));
            map
        }

        #[test]
        fn test_hw_fallback_false_skips_when_backend_unavailable() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc {
                        transcode video to hevc {
                            crf: 20
                            hw: nvenc
                            hw_fallback: false
                        }
                    }
                }"#,
            );
            let caps = caps_without_gpu();
            let result = evaluate_with_caps(&policy, &file, &caps);
            assert!(
                result.plans[0].is_skipped(),
                "Should skip when nvenc unavailable and hw_fallback is false"
            );
            assert!(result.plans[0]
                .skip_reason
                .as_ref()
                .expect("skip reason")
                .contains("nvenc"));
        }

        #[test]
        fn test_hw_fallback_true_falls_through_to_software() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc {
                        transcode video to hevc {
                            crf: 20
                            hw: nvenc
                            hw_fallback: true
                        }
                    }
                }"#,
            );
            let caps = caps_without_gpu();
            let result = evaluate_with_caps(&policy, &file, &caps);
            assert!(
                !result.plans[0].is_skipped(),
                "Should fall through to software when hw_fallback is true"
            );
            assert_eq!(result.plans[0].actions.len(), 1);
        }

        #[test]
        fn test_hw_fallback_default_falls_through() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc {
                        transcode video to hevc {
                            crf: 20
                            hw: nvenc
                        }
                    }
                }"#,
            );
            let caps = caps_without_gpu();
            let result = evaluate_with_caps(&policy, &file, &caps);
            assert!(
                !result.plans[0].is_skipped(),
                "Default hw_fallback (None) should allow software fallback"
            );
        }

        #[test]
        fn test_hw_available_runs_normally() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc {
                        transcode video to hevc {
                            crf: 20
                            hw: nvenc
                            hw_fallback: false
                        }
                    }
                }"#,
            );
            let caps = caps_with_nvenc();
            let result = evaluate_with_caps(&policy, &file, &caps);
            assert!(
                !result.plans[0].is_skipped(),
                "Should run normally when nvenc is available"
            );
            assert_eq!(result.plans[0].actions.len(), 1);
        }

        #[test]
        fn test_hw_fallback_skip_allows_downstream_depends_on() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc {
                        transcode video to hevc {
                            crf: 20
                            hw: nvenc
                            hw_fallback: false
                        }
                    }
                    phase cleanup {
                        depends_on: [tc]
                        when exists(audio where lang == eng) { warn "has eng" }
                    }
                }"#,
            );
            let caps = caps_without_gpu();
            let result = evaluate_with_caps(&policy, &file, &caps);
            assert!(result.plans[0].is_skipped(), "tc should be skipped");
            assert!(
                !result.plans[1].is_skipped(),
                "cleanup should still run via depends_on"
            );
        }

        #[test]
        fn test_hw_fields_threaded_to_action_params() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy = test_policy(
                r#"policy "test" {
                    phase tc {
                        transcode video to hevc {
                            crf: 20
                            hw: nvenc
                            hw_fallback: true
                        }
                    }
                }"#,
            );
            let caps = caps_without_gpu();
            let result = evaluate_with_caps(&policy, &file, &caps);
            let action = &result.plans[0].actions[0];
            match &action.parameters {
                ActionParams::Transcode { settings, .. } => {
                    assert_eq!(settings.hw.as_deref(), Some("nvenc"));
                    assert_eq!(settings.hw_fallback, Some(true));
                }
                other => panic!("Expected Transcode params, got {other:?}"),
            }
        }
    }

    mod field_filters {
        use super::*;

        #[test]
        fn test_keep_with_lang_field_japanese_content() {
            // Japanese content should keep both eng and jpn audio
            let mut file = test_file();
            file.plugin_metadata.insert(
                "radarr".into(),
                serde_json::json!({"original_language": "jpn"}),
            );

            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        keep audio where lang == eng
                             or lang == plugin.radarr.original_language
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            let removes: Vec<_> = result.plans[0]
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::RemoveTrack)
                .collect();
            // Track 3 (commentary eng) should be kept (lang==eng matches)
            // Track 1 (eng) kept, Track 2 (jpn) kept via field ref
            assert!(
                removes.is_empty(),
                "Japanese content should keep eng+jpn audio, got removes: {removes:?}"
            );
        }

        #[test]
        fn test_keep_with_lang_field_english_content() {
            // English content: field resolves to "eng", keeps only eng
            let mut file = test_file();
            file.plugin_metadata.insert(
                "radarr".into(),
                serde_json::json!({"original_language": "eng"}),
            );

            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        keep audio where lang == eng
                             or lang == plugin.radarr.original_language
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            let removes: Vec<_> = result.plans[0]
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::RemoveTrack)
                .collect();
            // Track 2 (jpn) should be removed; both filter arms resolve to "eng"
            assert_eq!(removes.len(), 1);
            assert_eq!(removes[0].track_index, Some(2));
        }

        #[test]
        fn test_skip_when_field_comparison() {
            let mut file = test_file();
            file.plugin_metadata.insert(
                "radarr".into(),
                serde_json::json!({"original_language": "eng"}),
            );

            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        skip when plugin.radarr.original_language == "eng"
                        keep audio where lang in [eng]
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            assert!(result.plans[0].is_skipped());
        }
    }

    mod conditional_keep_remove {
        use super::*;

        #[test]
        fn test_conditional_keep_removes_non_matching_tracks() {
            let file = test_file();
            // when condition is true (has jpn audio), keep only eng audio
            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        when exists(audio where lang == jpn) {
                            keep audio where lang in [eng]
                        }
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            let removes: Vec<_> = result.plans[0]
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::RemoveTrack)
                .collect();
            // Track 2 (jpn) should be removed
            assert_eq!(removes.len(), 1);
            assert_eq!(removes[0].track_index, Some(2));
        }

        #[test]
        fn test_conditional_remove_removes_matching_tracks() {
            let file = test_file();
            // when condition is true (is multi-language), remove commentary
            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        when audio_is_multi_language {
                            remove audio where commentary
                        }
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            let removes: Vec<_> = result.plans[0]
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::RemoveTrack)
                .collect();
            // Track 3 (commentary) should be removed
            assert_eq!(removes.len(), 1);
            assert_eq!(removes[0].track_index, Some(3));
        }

        #[test]
        fn test_conditional_keep_in_else_branch() {
            let file = test_file();
            // when condition is false (no french), else branch keeps eng only
            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        when exists(audio where lang == fre) {
                            warn "has french"
                        } else {
                            keep audio where lang in [eng]
                        }
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            let removes: Vec<_> = result.plans[0]
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::RemoveTrack)
                .collect();
            // Track 2 (jpn) should be removed via else branch
            assert_eq!(removes.len(), 1);
            assert_eq!(removes[0].track_index, Some(2));
        }

        #[test]
        fn test_conditional_keep_skipped_when_condition_false() {
            let file = test_file();
            // when condition is false (no french), then branch is skipped
            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        when exists(audio where lang == fre) {
                            keep audio where lang in [eng]
                        }
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            // Condition is false, no then actions execute, no removes
            assert!(
                result.plans[0].actions.is_empty(),
                "no actions when condition is false and no else branch"
            );
        }

        #[test]
        fn test_rules_block_with_keep_remove() {
            let file = test_file();
            let policy = test_policy(
                r#"policy "test" {
                    phase norm {
                        rules first {
                            rule "multi-lang" {
                                when audio_is_multi_language {
                                    keep audio where lang in [eng]
                                    remove subtitles where commentary
                                }
                            }
                        }
                    }
                }"#,
            );
            let result = evaluate(&policy, &file);
            let removes: Vec<_> = result.plans[0]
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::RemoveTrack)
                .collect();
            // Track 2 (jpn audio) removed by keep, track 5 (commentary sub) removed
            assert_eq!(removes.len(), 2);
            let indices: Vec<_> = removes.iter().map(|r| r.track_index.unwrap()).collect();
            assert!(indices.contains(&2), "jpn audio should be removed");
            assert!(indices.contains(&5), "commentary sub should be removed");
        }
    }

    #[test]
    fn test_safeguard_failed_blocks_run_if_completed() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize {
                    container mkv
                }
                phase post_tc {
                    depends_on: [containerize]
                    run_if containerize.completed
                    container mkv
                }
            }"#,
        )
        .unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "containerize".to_string(),
            EvaluationOutcome::SafeguardFailed,
        );

        let plan = evaluate_single_phase("post_tc", &policy, &file, &outcomes, None);
        let plan = plan.expect("phase should be evaluated");
        assert!(plan.is_skipped());
        assert!(plan
            .skip_reason
            .as_ref()
            .expect("should have skip reason")
            .contains("run_if"));
    }

    #[test]
    fn test_safeguard_failed_blocks_run_if_modified() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize {
                    container mkv
                }
                phase post_tc {
                    depends_on: [containerize]
                    run_if containerize.modified
                    container mkv
                }
            }"#,
        )
        .unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "containerize".to_string(),
            EvaluationOutcome::SafeguardFailed,
        );

        let plan = evaluate_single_phase("post_tc", &policy, &file, &outcomes, None);
        let plan = plan.expect("phase should be evaluated");
        assert!(plan.is_skipped());
        assert!(plan
            .skip_reason
            .as_ref()
            .expect("should have skip reason")
            .contains("run_if"));
    }

    #[test]
    fn test_safeguard_failed_satisfies_depends_on() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize {
                    container mkv
                }
                phase cleanup {
                    depends_on: [containerize]
                    container mkv
                }
            }"#,
        )
        .unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "containerize".to_string(),
            EvaluationOutcome::SafeguardFailed,
        );

        let plan = evaluate_single_phase("cleanup", &policy, &file, &outcomes, None);
        let plan = plan.expect("phase should be evaluated");
        assert!(
            !plan.is_skipped(),
            "depends_on should be satisfied by SafeguardFailed"
        );
    }

    #[test]
    fn test_execution_failed_blocks_run_if_completed() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize {
                    container mkv
                }
                phase post_tc {
                    depends_on: [containerize]
                    run_if containerize.completed
                    container mkv
                }
            }"#,
        )
        .unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "containerize".to_string(),
            EvaluationOutcome::ExecutionFailed,
        );

        let plan = evaluate_single_phase("post_tc", &policy, &file, &outcomes, None);
        let plan = plan.expect("phase should be evaluated");
        assert!(plan.is_skipped());
        assert!(plan
            .skip_reason
            .as_ref()
            .expect("should have skip reason")
            .contains("run_if"));
    }

    #[test]
    fn test_execution_failed_blocks_run_if_modified() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize {
                    container mkv
                }
                phase post_tc {
                    depends_on: [containerize]
                    run_if containerize.modified
                    container mkv
                }
            }"#,
        )
        .unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "containerize".to_string(),
            EvaluationOutcome::ExecutionFailed,
        );

        let plan = evaluate_single_phase("post_tc", &policy, &file, &outcomes, None);
        let plan = plan.expect("phase should be evaluated");
        assert!(plan.is_skipped());
        assert!(plan
            .skip_reason
            .as_ref()
            .expect("should have skip reason")
            .contains("run_if"));
    }

    #[test]
    fn test_execution_failed_satisfies_depends_on() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize {
                    container mkv
                }
                phase cleanup {
                    depends_on: [containerize]
                    container mkv
                }
            }"#,
        )
        .unwrap();

        let file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "containerize".to_string(),
            EvaluationOutcome::ExecutionFailed,
        );

        let plan = evaluate_single_phase("cleanup", &policy, &file, &outcomes, None);
        let plan = plan.expect("phase should be evaluated");
        assert!(
            !plan.is_skipped(),
            "depends_on should be satisfied by ExecutionFailed"
        );
    }

    // ---- Phase output cross-phase field access (issue #196) ----

    #[test]
    fn skip_when_verify_outcome_not_ok_skips_phase() {
        use voom_domain::plan::PhaseOutput;

        let policy = voom_dsl::compile_policy(
            r#"policy "p" {
                phase verify {
                    verify quick
                }
                phase backup {
                    depends_on: [verify]
                    skip when verify.outcome != "ok"
                    container mkv
                }
            }"#,
        )
        .expect("policy must compile");

        let file = MediaFile::new(PathBuf::from("/m/x.mkv"));

        let lookup = |name: &str| -> Option<PhaseOutput> {
            (name == "verify").then(|| {
                PhaseOutput::new()
                    .with_completed(true)
                    .with_outcome("error")
                    .with_error_count(1)
            })
        };
        let result = evaluate_with_evaluation_context(
            &policy,
            &file,
            EvaluationContext {
                capabilities: None,
                phase_output_lookup: Some(&lookup),
            },
        );

        let backup_plan = result
            .plans
            .iter()
            .find(|p| p.phase_name == "backup")
            .expect("backup plan present");
        assert!(
            backup_plan.is_skipped(),
            "backup phase should be skipped when verify.outcome != ok, got actions: {:?}",
            backup_plan.actions
        );
    }

    #[test]
    fn skip_when_verify_outcome_ok_runs_phase() {
        use voom_domain::plan::PhaseOutput;

        let policy = voom_dsl::compile_policy(
            r#"policy "p" {
                phase verify {
                    verify quick
                }
                phase backup {
                    depends_on: [verify]
                    skip when verify.outcome != "ok"
                    container mkv
                }
            }"#,
        )
        .expect("policy must compile");

        let file = MediaFile::new(PathBuf::from("/m/x.mkv"));

        let lookup = |name: &str| -> Option<PhaseOutput> {
            (name == "verify").then(|| PhaseOutput::new().with_completed(true).with_outcome("ok"))
        };
        let result = evaluate_with_evaluation_context(
            &policy,
            &file,
            EvaluationContext {
                capabilities: None,
                phase_output_lookup: Some(&lookup),
            },
        );

        let backup_plan = result
            .plans
            .iter()
            .find(|p| p.phase_name == "backup")
            .expect("backup plan present");
        assert!(
            !backup_plan.is_skipped(),
            "backup phase should run when verify.outcome == ok"
        );
    }

    #[test]
    fn evaluate_without_phase_outputs_treats_phase_field_as_missing() {
        // Without a lookup, FieldCompare resolves to None and the condition
        // evaluates to false — so `skip when verify.outcome != "ok"` does not
        // trigger and the phase runs.
        let policy = voom_dsl::compile_policy(
            r#"policy "p" {
                phase verify {
                    verify quick
                }
                phase backup {
                    depends_on: [verify]
                    skip when verify.outcome != "ok"
                    container mkv
                }
            }"#,
        )
        .expect("policy must compile");

        let file = MediaFile::new(PathBuf::from("/m/x.mkv"));
        let result = evaluate_with_evaluation_context(&policy, &file, EvaluationContext::default());

        let backup_plan = result
            .plans
            .iter()
            .find(|p| p.phase_name == "backup")
            .expect("backup plan present");
        assert!(
            !backup_plan.is_skipped(),
            "without lookup, FieldCompare returns false and phase runs"
        );
    }
}
