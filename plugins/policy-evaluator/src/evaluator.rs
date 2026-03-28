//! Core policy evaluation engine.
//!
//! Takes a [`CompiledPolicy`] and a [`MediaFile`] and produces a [`Plan`]
//! for each phase. The evaluator processes operations in order, generating
//! [`PlannedAction`]s that describe what changes need to be made.

use std::collections::{HashMap, HashSet};

use voom_domain::capability_map::CapabilityMap;
use voom_domain::errors::VoomError;
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};
use voom_domain::safeguard::{SafeguardKind, SafeguardViolation};
use voom_dsl::compiled::*;

use crate::condition::{evaluate_condition, resolve_value_or_field};
use crate::filter::{track_matches, tracks_for_target};

/// Result of evaluating a full policy against a file.
#[non_exhaustive]
#[derive(Debug)]
pub struct EvaluationResult {
    pub plans: Vec<Plan>,
}

/// Outcome of a single phase evaluation.
///
/// This is internal to the evaluator and distinct from `voom_domain::plan::PhaseOutcome`,
/// which represents execution outcomes. This type tracks evaluation-time outcomes
/// (e.g., whether a phase produced modifications) for dependency resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvaluationOutcome {
    Executed { modified: bool },
    Skipped,
}

/// Evaluate a compiled policy against a media file, producing plans for all phases.
#[must_use]
pub fn evaluate(policy: &CompiledPolicy, file: &MediaFile) -> EvaluationResult {
    let mut plans = Vec::new();
    let mut phase_outcomes: HashMap<String, EvaluationOutcome> = HashMap::new();

    for phase_name in &policy.phase_order {
        let phase = match policy.phases.iter().find(|p| &p.name == phase_name) {
            Some(p) => p,
            None => continue,
        };

        let plan = evaluate_phase(phase, policy, file, &phase_outcomes);

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

    EvaluationResult { plans }
}

/// Evaluate a single phase against a file.
fn evaluate_phase(
    phase: &CompiledPhase,
    policy: &CompiledPolicy,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
) -> Plan {
    let mut plan = Plan::new(file.clone(), policy.name.clone(), phase.name.clone());
    plan.policy_hash = if policy.source_hash.is_empty() {
        None
    } else {
        Some(policy.source_hash.clone())
    };

    if let Some(ref cond) = phase.skip_when {
        if evaluate_condition(cond, file) {
            plan.skip_reason = Some("skip_when condition met".into());
            return plan;
        }
    }

    if let Some(ref run_if) = phase.run_if {
        let should_run = match phase_outcomes.get(&run_if.phase) {
            Some(outcome) => match run_if.trigger {
                RunIfTrigger::Modified => {
                    matches!(outcome, EvaluationOutcome::Executed { modified: true })
                }
                RunIfTrigger::Completed => matches!(outcome, EvaluationOutcome::Executed { .. }),
            },
            None => false, // Referenced phase hasn't run
        };
        if !should_run {
            plan.skip_reason = Some(format!(
                "run_if {}.{} not satisfied",
                run_if.phase,
                match run_if.trigger {
                    RunIfTrigger::Modified => "modified",
                    RunIfTrigger::Completed => "completed",
                }
            ));
            return plan;
        }
    }

    for dep in &phase.depends_on {
        if phase_outcomes.get(dep).is_none() {
            plan.skip_reason = Some(format!("dependency '{dep}' not yet executed"));
            return plan;
        }
    }

    let mut ctx = PhaseContext {
        plan: &mut plan,
        file,
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
            }
        }
    }

    validate_plan(&mut plan, file);

    plan
}

/// Post-evaluation safeguard: verify that critical track types (video, audio)
/// are not entirely removed across all operations in the plan. This catches
/// multi-operation scenarios where individual keep/remove operations each
/// leave some tracks, but the combination removes all.
fn validate_plan(plan: &mut Plan, file: &MediaFile) {
    if plan.is_skipped() {
        return;
    }

    let removed_indices: HashSet<u32> = plan
        .actions
        .iter()
        .filter(|a| a.operation == OperationType::RemoveTrack)
        .filter_map(|a| a.track_index)
        .collect();

    if removed_indices.is_empty() {
        return;
    }

    let filename = file_name(file);

    validate_track_type(
        plan,
        file,
        &removed_indices,
        &filename,
        TrackType::is_video,
        SafeguardKind::NoVideoTrack,
        "video",
    );
    validate_track_type(
        plan,
        file,
        &removed_indices,
        &filename,
        TrackType::is_audio,
        SafeguardKind::NoAudioTrack,
        "audio",
    );
}

fn validate_track_type(
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
    plan.warnings.push(msg.clone());
    plan.safeguard_violations
        .push(SafeguardViolation::new(kind, msg, &plan.phase_name));
}

struct PhaseContext<'a> {
    plan: &'a mut Plan,
    file: &'a MediaFile,
}

/// Emit planned actions for a single operation into the plan.
fn emit_operation(op: &CompiledOperation, ctx: &mut PhaseContext) -> Result<(), VoomError> {
    match op {
        CompiledOperation::SetContainer(container) => {
            emit_set_container(container, ctx);
        }
        CompiledOperation::Keep { target, filter } => {
            emit_keep(target, filter.as_ref(), ctx);
        }
        CompiledOperation::Remove { target, filter } => {
            emit_remove(target, filter.as_ref(), ctx);
        }
        CompiledOperation::ReorderTracks(order) => {
            emit_reorder(order, ctx);
        }
        CompiledOperation::SetDefaults(defaults) => {
            emit_set_defaults(defaults, ctx);
        }
        CompiledOperation::ClearActions { target, settings } => {
            emit_clear_actions(target, settings, ctx);
        }
        CompiledOperation::Transcode {
            target,
            codec,
            settings,
        } => {
            emit_transcode(target, codec, settings, ctx);
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
            emit_rules(mode, rules, ctx)?;
        }
    }
    Ok(())
}

fn emit_set_container(container: &str, ctx: &mut PhaseContext) {
    let target = Container::from_extension(container);
    if ctx.file.container != target {
        ctx.plan.actions.push(PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container { container: target },
            format!(
                "Convert container from {} to {container}",
                ctx.file.container.as_str()
            ),
        ));
    }
}

fn emit_remove_track(track: &Track, target: &TrackTarget, reason: &str, ctx: &mut PhaseContext) {
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

fn emit_keep(target: &TrackTarget, filter: Option<&CompiledFilter>, ctx: &mut PhaseContext) {
    let tracks = tracks_for_target(ctx.file, target);
    if tracks.is_empty() {
        return;
    }

    let actions_before = ctx.plan.actions.len();
    let mut kept = 0u32;
    for track in &tracks {
        let should_remove = match filter {
            Some(f) => !track_matches(track, f),
            None => false, // "keep audio" with no filter keeps all
        };
        if should_remove {
            emit_remove_track(track, target, "does not match keep filter", ctx);
        } else {
            kept += 1;
        }
    }

    if kept == 0 {
        // Safeguard: retract all RemoveTrack actions we just added
        ctx.plan.actions.truncate(actions_before);
        let label = target_str(target);
        let filename = file_name(ctx.file);
        let msg = format!(
            "Safeguard: kept all {label} tracks in {filename} \
             — no tracks matched the keep filter, would have removed all"
        );
        ctx.plan.warnings.push(msg.clone());
        ctx.plan.safeguard_violations.push(SafeguardViolation::new(
            SafeguardKind::AllTracksRemoved,
            msg,
            &ctx.plan.phase_name,
        ));
    }
}

fn emit_remove(target: &TrackTarget, filter: Option<&CompiledFilter>, ctx: &mut PhaseContext) {
    let tracks = tracks_for_target(ctx.file, target);
    if tracks.is_empty() {
        return;
    }

    let is_critical = matches!(target, TrackTarget::Video | TrackTarget::Audio);
    let actions_before = ctx.plan.actions.len();
    let mut kept = 0u32;
    for track in &tracks {
        let should_remove = match filter {
            Some(f) => track_matches(track, f),
            None => true, // "remove audio" with no filter removes all
        };
        if should_remove {
            emit_remove_track(track, target, "matches remove filter", ctx);
        } else {
            kept += 1;
        }
    }

    if kept == 0 && is_critical {
        // Safeguard: retract all RemoveTrack actions for critical track types
        ctx.plan.actions.truncate(actions_before);
        let label = target_str(target);
        let filename = file_name(ctx.file);
        let msg = format!(
            "Safeguard: kept all {label} tracks in {filename} \
             — remove operation would have removed all"
        );
        ctx.plan.warnings.push(msg.clone());
        ctx.plan.safeguard_violations.push(SafeguardViolation::new(
            SafeguardKind::AllTracksRemoved,
            msg,
            &ctx.plan.phase_name,
        ));
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

fn emit_set_default(target: &TrackTarget, track: &Track, detail: &str, ctx: &mut PhaseContext) {
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

fn emit_clear_default(target: &TrackTarget, track: &Track, detail: &str, ctx: &mut PhaseContext) {
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

fn emit_clear_forced(target: &TrackTarget, track: &Track, ctx: &mut PhaseContext) {
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
fn emit_clear_title(target: &TrackTarget, track: &Track, ctx: &mut PhaseContext) {
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

fn emit_set_defaults(defaults: &[CompiledDefault], ctx: &mut PhaseContext) {
    for default in defaults {
        let tracks = tracks_for_target(ctx.file, &default.target);
        match default.strategy {
            DefaultStrategy::None => {
                for track in &tracks {
                    if track.is_default {
                        emit_clear_default(&default.target, track, "", ctx);
                    }
                }
            }
            DefaultStrategy::First => {
                if let Some((first_track, rest)) = tracks.split_first() {
                    if !first_track.is_default {
                        emit_set_default(&default.target, first_track, "", ctx);
                    }
                    for track in rest {
                        if track.is_default {
                            emit_clear_default(&default.target, track, "", ctx);
                        }
                    }
                }
            }
            DefaultStrategy::FirstPerLanguage => {
                let mut seen_langs: HashSet<String> = HashSet::new();
                for track in &tracks {
                    let is_first = seen_langs.insert(track.language.clone());
                    if is_first && !track.is_default {
                        emit_set_default(
                            &default.target,
                            track,
                            &format!(" (first for lang '{}')", track.language),
                            ctx,
                        );
                    } else if !is_first && track.is_default {
                        emit_clear_default(
                            &default.target,
                            track,
                            &format!(" (not first for lang '{}')", track.language),
                            ctx,
                        );
                    }
                }
            }
            DefaultStrategy::All => {
                for track in &tracks {
                    if !track.is_default {
                        emit_set_default(&default.target, track, "", ctx);
                    }
                }
            }
        }
    }
}

fn emit_clear_actions(
    target: &TrackTarget,
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
    target: &TrackTarget,
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

    let crf = settings.crf;
    let preset = settings.preset.clone();
    let bitrate = settings.bitrate.clone();
    let channels = settings.channels;

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
                crf,
                preset: preset.clone(),
                bitrate: bitrate.clone(),
                channels,
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
        if !evaluate_condition(cond, ctx.file) {
            return;
        }
    }

    let audio_tracks = ctx.file.audio_tracks();

    // Check skip_if_exists
    if let Some(ref skip_filter) = synth.skip_if_exists {
        if audio_tracks.iter().any(|t| track_matches(t, skip_filter)) {
            return;
        }
    }

    // Find source track
    let source_index = if let Some(ref source_filter) = synth.source {
        audio_tracks
            .iter()
            .find(|t| track_matches(t, source_filter))
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

    let channels = synth.channels.as_ref().map(|c| match c {
        SynthChannels::Count(n) => *n,
        SynthChannels::Named(s) => match s.as_str() {
            "mono" => 1,
            "stereo" => 2,
            "5.1" | "surround" => 6,
            "7.1" => 8,
            other => {
                tracing::warn!(preset = other, "unknown channel preset, defaulting to 2");
                2
            }
        },
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
    let val = resolve_value_or_field(value, ctx.file)
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
    let matched = evaluate_condition(&cond.condition, ctx.file);
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
    mode: &RulesMode,
    rules: &[CompiledRule],
    ctx: &mut PhaseContext,
) -> Result<(), VoomError> {
    for rule in rules {
        let matched = evaluate_condition(&rule.conditional.condition, ctx.file);
        if matched {
            for action in &rule.conditional.then_actions {
                emit_action(action, ctx)?;
            }
            if *mode == RulesMode::First {
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
            emit_flag_action(ctx, target, filter, FlagKind::Default);
        }
        CompiledAction::SetForced { target, filter } => {
            emit_flag_action(ctx, target, filter, FlagKind::Forced);
        }
        CompiledAction::SetLanguage {
            target,
            filter,
            value,
        } => {
            let lang = resolve_value_or_field(value, ctx.file).ok_or_else(|| {
                VoomError::Validation("Cannot resolve language value".to_string())
            })?;
            let tracks = tracks_for_target(ctx.file, target);
            for track in &tracks {
                if filter_matches(track, filter) && track.language != lang {
                    ctx.plan.actions.push(PlannedAction::track_op(
                        OperationType::SetLanguage,
                        track.index,
                        ActionParams::Language {
                            language: lang.clone(),
                        },
                        format!(
                            "Set language on {} track {} to '{lang}'",
                            target_str(target),
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

fn filter_matches(track: &Track, filter: &Option<CompiledFilter>) -> bool {
    match filter {
        Some(f) => track_matches(track, f),
        None => true,
    }
}

enum FlagKind {
    Default,
    Forced,
}

fn emit_flag_action(
    ctx: &mut PhaseContext,
    target: &TrackTarget,
    filter: &Option<CompiledFilter>,
    kind: FlagKind,
) {
    let (op, label, is_set_fn): (OperationType, &str, fn(&Track) -> bool) = match kind {
        FlagKind::Default => (OperationType::SetDefault, "default", |t| t.is_default),
        FlagKind::Forced => (OperationType::SetForced, "forced", |t| t.is_forced),
    };
    let tracks = tracks_for_target(ctx.file, target);
    for track in &tracks {
        if filter_matches(track, filter) && !is_set_fn(track) {
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

fn target_str(target: &TrackTarget) -> &'static str {
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
pub(crate) fn validate_against_capabilities(plans: &mut [Plan], capabilities: &CapabilityMap) {
    if capabilities.is_empty() {
        return;
    }

    for plan in plans.iter_mut() {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }

        let mut all_encoders: Option<HashSet<&str>> = None;

        for action in &plan.actions {
            match &action.parameters {
                ActionParams::Transcode { codec, .. } => {
                    let executors = capabilities.encoders_for(codec);
                    if executors.is_empty() {
                        plan.warnings
                            .push(format!("No executor supports encoder '{codec}'"));
                    }
                    intersect_executors(&mut all_encoders, &executors);
                }
                ActionParams::Synthesize { codec: Some(c), .. } => {
                    let executors = capabilities.encoders_for(c);
                    if executors.is_empty() {
                        plan.warnings
                            .push(format!("No executor supports encoder '{c}'"));
                    }
                    intersect_executors(&mut all_encoders, &executors);
                }
                ActionParams::Container { container } => {
                    let fmt = container.ffmpeg_format_name().unwrap_or(container.as_str());
                    if !capabilities.has_format(fmt) {
                        plan.warnings
                            .push(format!("No executor supports format '{fmt}'"));
                    }
                }
                _ => {}
            }
        }

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
        let mut file = test_file();
        file.container = Container::Mkv;
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
    fn test_validate_plan_no_safeguard_when_tracks_survive() {
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
    fn test_validate_plan_retracts_when_all_video_removed() {
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
        // validate_plan should catch this since emit_remove doesn't guard video
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

    // --- Capability validation tests ---

    mod capability_validation {
        use super::*;
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

        fn ffmpeg_capabilities() -> CapabilityMap {
            let mut map = CapabilityMap::new();
            map.register(ExecutorCapabilitiesEvent::new(
                "ffmpeg-executor",
                CodecCapabilities::new(
                    vec!["h264".into(), "hevc".into(), "aac".into()],
                    vec!["h264".into(), "hevc".into(), "aac".into()],
                ),
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
            let mut result = evaluate(&policy, &file);
            let caps = ffmpeg_capabilities();
            validate_against_capabilities(&mut result.plans, &caps);

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
            let mut result = evaluate(&policy, &file);
            let caps = ffmpeg_capabilities();
            validate_against_capabilities(&mut result.plans, &caps);

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
            let file = test_file();
            let policy = test_policy(r#"policy "test" { phase init { container webm } }"#);
            let mut result = evaluate(&policy, &file);
            let caps = ffmpeg_capabilities();
            validate_against_capabilities(&mut result.plans, &caps);

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
            let mut result = evaluate(&policy, &file);
            let caps = ffmpeg_capabilities();
            validate_against_capabilities(&mut result.plans, &caps);

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
            let mut result = evaluate(&policy, &file);
            assert!(result.plans[0].is_empty());

            // Use an empty capability map — would cause warnings if validation ran
            let caps = CapabilityMap::new();
            validate_against_capabilities(&mut result.plans, &caps);
            assert!(result.plans[0].warnings.is_empty());
        }

        #[test]
        fn test_empty_capability_map_skips_validation() {
            let mut file = test_file();
            file.tracks[0] = Track::new(0, TrackType::Video, "h264".into());
            let policy =
                test_policy(r#"policy "test" { phase tc { transcode video to hevc {} } }"#);
            let mut result = evaluate(&policy, &file);
            assert!(!result.plans[0].is_empty());

            let caps = CapabilityMap::new();
            validate_against_capabilities(&mut result.plans, &caps);

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
            let mut result = evaluate(&policy, &file);
            let caps = ffmpeg_capabilities();
            validate_against_capabilities(&mut result.plans, &caps);

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
            let mut result = evaluate(&policy, &file);
            // File is already MKV so no convert action, but verify the mapping
            // works by converting an MP4 file
            file.container = Container::Mp4;
            let mut result2 = evaluate(&policy, &file);
            let caps = ffmpeg_capabilities();
            validate_against_capabilities(&mut result2.plans, &caps);
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
            let mut result = evaluate(&policy, &file);

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
            validate_against_capabilities(&mut result.plans, &caps);

            assert!(
                result.plans[0].executor_hint.is_none(),
                "Should not set hint when multiple executors match"
            );
        }
    }
}
