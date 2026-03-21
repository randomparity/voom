//! Core policy evaluation engine.
//!
//! Takes a [`CompiledPolicy`] and a [`MediaFile`] and produces a [`Plan`]
//! for each phase. The evaluator processes operations in order, generating
//! [`PlannedAction`]s that describe what changes need to be made.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use uuid::Uuid;
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{OperationType, Plan, PlannedAction};
use voom_dsl::compiler::*;

use crate::condition::{evaluate_condition, resolve_value_or_field};
use crate::filter::track_matches;

/// Result of evaluating a full policy against a file.
#[derive(Debug)]
pub struct EvaluationResult {
    pub plans: Vec<Plan>,
    pub phase_outcomes: HashMap<String, EvaluationOutcome>,
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
    Failed,
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

    EvaluationResult {
        plans,
        phase_outcomes,
    }
}

/// Evaluate a single phase against a file.
fn evaluate_phase(
    phase: &CompiledPhase,
    policy: &CompiledPolicy,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
) -> Plan {
    let mut plan = Plan {
        id: Uuid::new_v4(),
        file: file.clone(),
        policy_name: policy.name.clone(),
        phase_name: phase.name.clone(),
        actions: Vec::new(),
        warnings: Vec::new(),
        skip_reason: None,
        policy_hash: if policy.source_hash.is_empty() {
            None
        } else {
            Some(policy.source_hash.clone())
        },
        evaluated_at: Utc::now(),
    };

    // Check skip_when condition
    if let Some(ref cond) = phase.skip_when {
        if evaluate_condition(cond, file) {
            plan.skip_reason = Some("skip_when condition met".into());
            return plan;
        }
    }

    // Check run_if dependency
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

    // Check all depends_on phases completed
    for dep in &phase.depends_on {
        match phase_outcomes.get(dep) {
            Some(EvaluationOutcome::Failed) => {
                plan.skip_reason = Some(format!("dependency '{dep}' failed"));
                return plan;
            }
            None => {
                plan.skip_reason = Some(format!("dependency '{dep}' not yet executed"));
                return plan;
            }
            _ => {} // Skipped or Executed is OK
        }
    }

    // Process operations
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

    plan
}

struct PhaseContext<'a> {
    plan: &'a mut Plan,
    file: &'a MediaFile,
}

/// Emit planned actions for a single operation into the plan.
fn emit_operation(op: &CompiledOperation, ctx: &mut PhaseContext) -> Result<(), String> {
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
        ctx.plan.actions.push(PlannedAction {
            operation: OperationType::ConvertContainer,
            track_index: None,
            parameters: serde_json::json!({
                "target": container,
            }),
            description: format!(
                "Convert container from {} to {container}",
                ctx.file.container.as_str()
            ),
        });
    }
}

fn emit_keep(target: &TrackTarget, filter: Option<&CompiledFilter>, ctx: &mut PhaseContext) {
    let tracks = tracks_for_target(ctx.file, target);
    for track in &tracks {
        let should_remove = match filter {
            Some(f) => !track_matches(track, f),
            None => false, // "keep audio" with no filter keeps all
        };
        if should_remove {
            ctx.plan.actions.push(PlannedAction {
                operation: OperationType::RemoveTrack,
                track_index: Some(track.index),
                parameters: serde_json::json!({
                    "reason": "does not match keep filter",
                }),
                description: format!(
                    "Remove {} track {} ({}, {})",
                    target_str(target),
                    track.index,
                    track.codec,
                    track.language
                ),
            });
        }
    }
}

fn emit_remove(target: &TrackTarget, filter: Option<&CompiledFilter>, ctx: &mut PhaseContext) {
    let tracks = tracks_for_target(ctx.file, target);
    for track in &tracks {
        let should_remove = match filter {
            Some(f) => track_matches(track, f),
            None => true, // "remove audio" with no filter removes all
        };
        if should_remove {
            ctx.plan.actions.push(PlannedAction {
                operation: OperationType::RemoveTrack,
                track_index: Some(track.index),
                parameters: serde_json::json!({
                    "reason": "matches remove filter",
                }),
                description: format!(
                    "Remove {} track {} ({}, {})",
                    target_str(target),
                    track.index,
                    track.codec,
                    track.language
                ),
            });
        }
    }
}

fn emit_reorder(order: &[String], ctx: &mut PhaseContext) {
    ctx.plan.actions.push(PlannedAction {
        operation: OperationType::ReorderTracks,
        track_index: None,
        parameters: serde_json::json!({
            "order": order,
        }),
        description: format!("Reorder tracks: {}", order.join(", ")),
    });
}

fn emit_set_defaults(defaults: &[CompiledDefault], ctx: &mut PhaseContext) {
    for default in defaults {
        let tracks = tracks_for_target(ctx.file, &default.target);
        match default.strategy {
            DefaultStrategy::None => {
                for track in &tracks {
                    if track.is_default {
                        ctx.plan.actions.push(PlannedAction {
                            operation: OperationType::ClearDefault,
                            track_index: Some(track.index),
                            parameters: serde_json::json!({}),
                            description: format!(
                                "Clear default flag on {} track {}",
                                target_str(&default.target),
                                track.index
                            ),
                        });
                    }
                }
            }
            DefaultStrategy::First => {
                if let Some((first_track, rest)) = tracks.split_first() {
                    if !first_track.is_default {
                        ctx.plan.actions.push(PlannedAction {
                            operation: OperationType::SetDefault,
                            track_index: Some(first_track.index),
                            parameters: serde_json::json!({}),
                            description: format!(
                                "Set default on {} track {}",
                                target_str(&default.target),
                                first_track.index
                            ),
                        });
                    }
                    for track in rest {
                        if track.is_default {
                            ctx.plan.actions.push(PlannedAction {
                                operation: OperationType::ClearDefault,
                                track_index: Some(track.index),
                                parameters: serde_json::json!({}),
                                description: format!(
                                    "Clear default flag on {} track {}",
                                    target_str(&default.target),
                                    track.index
                                ),
                            });
                        }
                    }
                }
            }
            DefaultStrategy::FirstPerLanguage => {
                let mut seen_langs: HashSet<String> = HashSet::new();
                for track in &tracks {
                    let is_first = seen_langs.insert(track.language.clone());
                    if is_first && !track.is_default {
                        ctx.plan.actions.push(PlannedAction {
                            operation: OperationType::SetDefault,
                            track_index: Some(track.index),
                            parameters: serde_json::json!({}),
                            description: format!(
                                "Set default on {} track {} (first for lang '{}')",
                                target_str(&default.target),
                                track.index,
                                track.language
                            ),
                        });
                    } else if !is_first && track.is_default {
                        ctx.plan.actions.push(PlannedAction {
                            operation: OperationType::ClearDefault,
                            track_index: Some(track.index),
                            parameters: serde_json::json!({}),
                            description: format!(
                                "Clear default flag on {} track {} (not first for lang '{}')",
                                target_str(&default.target),
                                track.index,
                                track.language
                            ),
                        });
                    }
                }
            }
            DefaultStrategy::All => {
                for track in &tracks {
                    if !track.is_default {
                        ctx.plan.actions.push(PlannedAction {
                            operation: OperationType::SetDefault,
                            track_index: Some(track.index),
                            parameters: serde_json::json!({}),
                            description: format!(
                                "Set default on {} track {}",
                                target_str(&default.target),
                                track.index
                            ),
                        });
                    }
                }
            }
        }
    }
}

fn emit_clear_actions(
    target: &TrackTarget,
    settings: &HashMap<String, serde_json::Value>,
    ctx: &mut PhaseContext,
) {
    let tracks = tracks_for_target(ctx.file, target);
    let clear_default = settings
        .get("clear_all_default")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let clear_forced = settings
        .get("clear_all_forced")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let clear_titles = settings
        .get("clear_all_titles")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    for track in &tracks {
        if clear_default && track.is_default {
            ctx.plan.actions.push(PlannedAction {
                operation: OperationType::ClearDefault,
                track_index: Some(track.index),
                parameters: serde_json::json!({}),
                description: format!(
                    "Clear default flag on {} track {}",
                    target_str(target),
                    track.index
                ),
            });
        }
        if clear_forced && track.is_forced {
            ctx.plan.actions.push(PlannedAction {
                operation: OperationType::ClearForced,
                track_index: Some(track.index),
                parameters: serde_json::json!({}),
                description: format!(
                    "Clear forced flag on {} track {}",
                    target_str(target),
                    track.index
                ),
            });
        }
        if clear_titles && !track.title.is_empty() {
            ctx.plan.actions.push(PlannedAction {
                operation: OperationType::SetTitle,
                track_index: Some(track.index),
                parameters: serde_json::json!({"title": ""}),
                description: format!(
                    "Clear title on {} track {}",
                    target_str(target),
                    track.index
                ),
            });
        }
    }
}

fn emit_transcode(
    target: &TrackTarget,
    codec: &str,
    settings: &HashMap<String, serde_json::Value>,
    ctx: &mut PhaseContext,
) {
    let tracks = tracks_for_target(ctx.file, target);

    // Check preserve list
    let preserve: Vec<String> = settings
        .get("preserve")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let operation = match target {
        TrackTarget::Video => OperationType::TranscodeVideo,
        _ => OperationType::TranscodeAudio,
    };

    for track in &tracks {
        // Skip if already the target codec
        if track.codec == codec {
            continue;
        }
        // Skip if codec is in preserve list
        if preserve.iter().any(|p| p == &track.codec) {
            continue;
        }

        let mut params = settings.clone();
        params.insert("codec".into(), serde_json::Value::String(codec.into()));

        ctx.plan.actions.push(PlannedAction {
            operation,
            track_index: Some(track.index),
            parameters: serde_json::json!(params),
            description: format!(
                "Transcode {} track {} from {} to {codec}",
                target_str(target),
                track.index,
                track.codec
            ),
        });
    }
}

fn emit_synthesize(synth: &CompiledSynthesize, ctx: &mut PhaseContext) {
    // Check create_if condition
    if let Some(ref cond) = synth.create_if {
        if !evaluate_condition(cond, ctx.file) {
            return;
        }
    }

    // Check skip_if_exists
    if let Some(ref skip_filter) = synth.skip_if_exists {
        let audio_tracks = ctx.file.audio_tracks();
        if audio_tracks.iter().any(|t| track_matches(t, skip_filter)) {
            return;
        }
    }

    // Find source track
    let source_index = if let Some(ref source_filter) = synth.source {
        ctx.file
            .audio_tracks()
            .iter()
            .find(|t| track_matches(t, source_filter))
            .map(|t| t.index)
    } else {
        ctx.file.audio_tracks().first().map(|t| t.index)
    };

    let mut params = serde_json::Map::new();
    if let Some(ref codec) = synth.codec {
        params.insert("codec".into(), serde_json::Value::String(codec.clone()));
    }
    if let Some(ref channels) = synth.channels {
        params.insert("channels".into(), channels.clone());
    }
    if let Some(ref bitrate) = synth.bitrate {
        params.insert("bitrate".into(), serde_json::Value::String(bitrate.clone()));
    }
    if let Some(ref title) = synth.title {
        params.insert("title".into(), serde_json::Value::String(title.clone()));
    }
    if let Some(ref lang) = synth.language {
        match lang {
            SynthLanguage::Inherit => {
                if let Some(idx) = source_index {
                    if let Some(src) = ctx.file.tracks.iter().find(|t| t.index == idx) {
                        params.insert(
                            "language".into(),
                            serde_json::Value::String(src.language.clone()),
                        );
                    }
                }
            }
            SynthLanguage::Fixed(l) => {
                params.insert("language".into(), serde_json::Value::String(l.clone()));
            }
        }
    }
    if let Some(ref position) = synth.position {
        params.insert("position".into(), position.clone());
    }
    if let Some(idx) = source_index {
        params.insert("source_track".into(), serde_json::json!(idx));
    }

    ctx.plan.actions.push(PlannedAction {
        operation: OperationType::SynthesizeAudio,
        track_index: source_index,
        parameters: serde_json::Value::Object(params),
        description: format!("Synthesize audio: {}", synth.name),
    });
}

fn emit_clear_tags(ctx: &mut PhaseContext) {
    if ctx.file.tags.is_empty() {
        return;
    }
    let mut tag_keys: Vec<String> = ctx.file.tags.keys().cloned().collect();
    tag_keys.sort();
    ctx.plan.actions.push(PlannedAction {
        operation: OperationType::ClearContainerTags,
        track_index: None,
        parameters: serde_json::json!({ "tags": tag_keys }),
        description: format!("Clear all container tags ({})", tag_keys.join(", ")),
    });
}

fn emit_set_tag(
    tag: &str,
    value: &CompiledValueOrField,
    ctx: &mut PhaseContext,
) -> Result<(), String> {
    let val = resolve_value_or_field(value, ctx.file)
        .ok_or_else(|| format!("Cannot resolve tag value for '{tag}'"))?;
    ctx.plan.actions.push(PlannedAction {
        operation: OperationType::SetContainerTag,
        track_index: None,
        parameters: serde_json::json!({ "tag": tag, "value": val }),
        description: format!("Set container tag '{tag}' = '{val}'"),
    });
    Ok(())
}

fn emit_delete_tag(tag: &str, ctx: &mut PhaseContext) {
    if ctx.file.tags.contains_key(tag) {
        ctx.plan.actions.push(PlannedAction {
            operation: OperationType::DeleteContainerTag,
            track_index: None,
            parameters: serde_json::json!({ "tag": tag }),
            description: format!("Delete container tag '{tag}'"),
        });
    } else {
        tracing::debug!(tag, "delete_tag: tag not present in file, skipping");
    }
}

fn emit_conditional(cond: &CompiledConditional, ctx: &mut PhaseContext) -> Result<(), String> {
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
) -> Result<(), String> {
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

fn emit_action(action: &CompiledAction, ctx: &mut PhaseContext) -> Result<(), String> {
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
            return Err(expanded);
        }
        CompiledAction::SetDefault { target, filter } => {
            let tracks = tracks_for_target(ctx.file, target);
            for track in &tracks {
                let matches = match filter {
                    Some(f) => track_matches(track, f),
                    None => true,
                };
                if matches && !track.is_default {
                    ctx.plan.actions.push(PlannedAction {
                        operation: OperationType::SetDefault,
                        track_index: Some(track.index),
                        parameters: serde_json::json!({}),
                        description: format!(
                            "Set default on {} track {}",
                            target_str(target),
                            track.index
                        ),
                    });
                }
            }
        }
        CompiledAction::SetForced { target, filter } => {
            let tracks = tracks_for_target(ctx.file, target);
            for track in &tracks {
                let matches = match filter {
                    Some(f) => track_matches(track, f),
                    None => true,
                };
                if matches && !track.is_forced {
                    ctx.plan.actions.push(PlannedAction {
                        operation: OperationType::SetForced,
                        track_index: Some(track.index),
                        parameters: serde_json::json!({}),
                        description: format!(
                            "Set forced on {} track {}",
                            target_str(target),
                            track.index
                        ),
                    });
                }
            }
        }
        CompiledAction::SetLanguage {
            target,
            filter,
            value,
        } => {
            let lang = resolve_value_or_field(value, ctx.file)
                .ok_or_else(|| "Cannot resolve language value".to_string())?;
            let tracks = tracks_for_target(ctx.file, target);
            for track in &tracks {
                let matches = match filter {
                    Some(f) => track_matches(track, f),
                    None => true,
                };
                if matches && track.language != lang {
                    ctx.plan.actions.push(PlannedAction {
                        operation: OperationType::SetLanguage,
                        track_index: Some(track.index),
                        parameters: serde_json::json!({"language": lang}),
                        description: format!(
                            "Set language on {} track {} to '{lang}'",
                            target_str(target),
                            track.index
                        ),
                    });
                }
            }
        }
        CompiledAction::SetTag { tag, value } => {
            let val = resolve_value_or_field(value, ctx.file)
                .ok_or_else(|| format!("Cannot resolve tag value for '{tag}'"))?;
            ctx.plan.actions.push(PlannedAction {
                operation: OperationType::SetContainerTag,
                track_index: None,
                parameters: serde_json::json!({ "tag": tag, "value": val }),
                description: format!("Set container tag '{tag}' = '{val}'"),
            });
        }
    }
    Ok(())
}

/// Expand `{filename}` and `{path}` templates in a string.
fn expand_template(template: &str, file: &MediaFile) -> String {
    let filename = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    template
        .replace("{filename}", &filename)
        .replace("{path}", &file.path.to_string_lossy())
}

fn tracks_for_target<'a>(file: &'a MediaFile, target: &TrackTarget) -> Vec<&'a Track> {
    match target {
        TrackTarget::Video => file.video_tracks(),
        TrackTarget::Audio => file.audio_tracks(),
        TrackTarget::Subtitle => file.subtitle_tracks(),
        TrackTarget::Attachment => file.tracks_of_type(TrackType::Attachment),
        TrackTarget::Any => file.tracks.iter().collect(),
    }
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
        voom_dsl::compile(source).expect("Failed to compile test policy")
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
        assert!(result.plans[0].warnings.len() >= 1);
    }

    #[test]
    fn test_full_production_policy() {
        let source =
            include_str!("../../../crates/voom-dsl/tests/fixtures/production-normalize.voom");
        let policy = voom_dsl::compile(source).unwrap();
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
        let tags = plan.actions[0].parameters["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2);
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
        assert_eq!(plan.actions[0].parameters["tag"], "title");
        assert_eq!(plan.actions[0].parameters["value"], "My Movie");
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
        assert_eq!(plan.actions[0].parameters["tag"], "encoder");
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
}
