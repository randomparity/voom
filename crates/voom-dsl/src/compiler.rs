//! Compiler: AST → `CompiledPolicy`.
//!
//! Transforms a validated [`PolicyAst`] into a [`CompiledPolicy`] structure
//! that uses domain types and is ready for evaluation by the policy evaluator plugin.
//!
//! The `.unwrap()` calls in this module operate on grammar-guaranteed structures
//! from the pest parser — the AST shape is validated before compilation.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use crate::compiled::{
    ClearActionsSettings, CompiledAction, CompiledCompareOp, CompiledCondition,
    CompiledConditional, CompiledConfig, CompiledDefault, CompiledFilter, CompiledOperation,
    CompiledPhase, CompiledPolicy, CompiledRegex, CompiledRule, CompiledRunIf, CompiledSynthesize,
    CompiledTranscodeSettings, CompiledValueOrField, DefaultStrategy, ErrorStrategy, RulesMode,
    RunIfTrigger, SynthChannels, SynthLanguage, SynthPosition, TrackTarget,
};
use voom_domain::utils::codecs;

use crate::ast::*;
use crate::errors::DslError;

/// Safely convert an f64 to u32, returning None for negative, fractional, or out-of-range values.
fn safe_u32(n: f64) -> Option<u32> {
    if n >= 0.0 && n <= u32::MAX as f64 && n.fract() == 0.0 {
        Some(n as u32)
    } else {
        None
    }
}

/// Compile a pre-parsed and validated AST into a [`CompiledPolicy`].
pub(crate) fn compile_ast(ast: &PolicyAst) -> std::result::Result<CompiledPolicy, DslError> {
    let config = compile_config(ast.config.as_ref());
    let phases: Vec<CompiledPhase> = ast
        .phases
        .iter()
        .map(compile_phase)
        .collect::<std::result::Result<_, _>>()?;
    let phase_order = topological_sort(ast)?;

    Ok(CompiledPolicy::new(
        ast.name.clone(),
        config,
        phases,
        phase_order,
        String::new(),
    ))
}

fn compile_config(config: Option<&ConfigNode>) -> CompiledConfig {
    match config {
        Some(c) => CompiledConfig::new(
            c.audio_languages.clone(),
            c.subtitle_languages.clone(),
            c.on_error
                .as_deref()
                .and_then(parse_error_strategy)
                .unwrap_or(ErrorStrategy::Abort),
            c.commentary_patterns.clone(),
            c.keep_backups.unwrap_or(false),
        ),
        None => CompiledConfig::new(vec![], vec![], ErrorStrategy::Abort, vec![], false),
    }
}

/// Parse an error strategy string. Returns `None` for unrecognized values.
/// Used by both the compiler (to convert) and validator (to check validity).
pub(crate) fn parse_error_strategy(value: &str) -> Option<ErrorStrategy> {
    match value {
        "continue" => Some(ErrorStrategy::Continue),
        "skip" => Some(ErrorStrategy::Skip),
        "abort" => Some(ErrorStrategy::Abort),
        _ => None,
    }
}

/// Parse a default strategy string. Returns `None` for unrecognized values.
/// Used by both the compiler (to convert) and validator (to check validity).
pub(crate) fn parse_default_strategy(value: &str) -> Option<DefaultStrategy> {
    match value {
        "first_per_language" => Some(DefaultStrategy::FirstPerLanguage),
        "none" => Some(DefaultStrategy::None),
        "first" => Some(DefaultStrategy::First),
        "all" => Some(DefaultStrategy::All),
        _ => None,
    }
}

fn compile_phase(phase: &PhaseNode) -> std::result::Result<CompiledPhase, DslError> {
    let skip_when = phase
        .skip_when
        .as_ref()
        .map(compile_condition)
        .transpose()?;

    let run_if = phase.run_if.as_ref().map(|r| {
        let trigger = match r.trigger.as_str() {
            "modified" => RunIfTrigger::Modified,
            "completed" => RunIfTrigger::Completed,
            other => {
                unreachable!("grammar restricts run_if_trigger to modified|completed, got: {other}")
            }
        };
        CompiledRunIf::new(r.phase.clone(), trigger)
    });

    let operations: Vec<CompiledOperation> = phase
        .operations
        .iter()
        .map(|spanned| compile_operation(&spanned.node))
        .collect::<std::result::Result<_, _>>()?;

    Ok(CompiledPhase {
        name: phase.name.clone(),
        depends_on: phase.depends_on.clone(),
        skip_when,
        run_if,
        on_error: phase
            .on_error
            .as_deref()
            .and_then(parse_error_strategy)
            .unwrap_or(ErrorStrategy::Abort),
        operations,
    })
}

fn compile_operation(op: &OperationNode) -> std::result::Result<CompiledOperation, DslError> {
    match op {
        OperationNode::Container(name) => Ok(CompiledOperation::SetContainer(name.clone())),
        OperationNode::Keep { target, filter } => Ok(CompiledOperation::Keep {
            target: parse_track_target(target),
            filter: filter.as_ref().map(compile_filter).transpose()?,
        }),
        OperationNode::Remove { target, filter } => Ok(CompiledOperation::Remove {
            target: parse_track_target(target),
            filter: filter.as_ref().map(compile_filter).transpose()?,
        }),
        OperationNode::Order(items) => Ok(CompiledOperation::ReorderTracks(items.clone())),
        OperationNode::Defaults(items) => {
            let defaults = items
                .iter()
                .map(|(kind, value)| {
                    CompiledDefault::new(
                        parse_track_target(kind),
                        parse_default_strategy(value).unwrap_or(DefaultStrategy::None),
                    )
                })
                .collect();
            Ok(CompiledOperation::SetDefaults(defaults))
        }
        OperationNode::Actions { target, settings } => {
            let bool_setting = |key: &str| -> bool {
                settings
                    .iter()
                    .find(|(k, _)| k == key)
                    .and_then(|(_, v)| {
                        if let Value::Bool(b) = v {
                            Some(*b)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(false)
            };
            Ok(CompiledOperation::ClearActions {
                target: parse_track_target(target),
                settings: ClearActionsSettings::new(
                    bool_setting("clear_all_default"),
                    bool_setting("clear_all_forced"),
                    bool_setting("clear_all_titles"),
                ),
            })
        }
        OperationNode::Transcode {
            target,
            codec,
            settings,
        } => Ok(compile_transcode(target, codec, settings)),
        OperationNode::Synthesize { name, settings } => Ok(CompiledOperation::Synthesize(
            Box::new(compile_synthesize(name, settings)?),
        )),
        OperationNode::ClearTags => Ok(CompiledOperation::ClearTags),
        OperationNode::SetTag { tag, value } => Ok(CompiledOperation::SetTag {
            tag: tag.clone(),
            value: compile_value_or_field(value),
        }),
        OperationNode::DeleteTag(tag) => Ok(CompiledOperation::DeleteTag(tag.clone())),
        OperationNode::When(when) => Ok(CompiledOperation::Conditional(compile_conditional(when)?)),
        OperationNode::Rules { mode, rules } => {
            let compiled_rules: Vec<CompiledRule> = rules
                .iter()
                .map(|r| {
                    Ok(CompiledRule::new(
                        r.name.clone(),
                        compile_conditional(&r.when)?,
                    ))
                })
                .collect::<std::result::Result<_, DslError>>()?;
            Ok(CompiledOperation::Rules {
                mode: match mode.as_str() {
                    "first" => RulesMode::First,
                    "all" => RulesMode::All,
                    _ => unreachable!("validator rejects unknown rules modes"),
                },
                rules: compiled_rules,
            })
        }
    }
}

fn compile_transcode(target: &str, codec: &str, settings: &[(String, Value)]) -> CompiledOperation {
    let canonical = codecs::normalize_codec(codec)
        .map(|s| s.to_string())
        .unwrap_or_else(|| codec.to_string());

    let get =
        |key: &str| -> Option<&Value> { settings.iter().find(|(k, _)| k == key).map(|(_, v)| v) };

    let preserve = match get("preserve") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) | Value::Ident(s) => Some(s.clone()),
                Value::Number(_, s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };

    let crf = match get("crf") {
        Some(Value::Number(n, _)) => safe_u32(*n),
        _ => None,
    };

    let preset = match get("preset") {
        Some(Value::String(s) | Value::Ident(s)) => Some(s.clone()),
        _ => None,
    };

    let bitrate = match get("bitrate") {
        Some(Value::String(s) | Value::Ident(s)) => Some(s.clone()),
        Some(Value::Number(_, s)) => Some(s.clone()),
        _ => None,
    };

    let channels = match get("channels") {
        Some(Value::Number(n, _)) => safe_u32(*n),
        _ => None,
    };

    let hw = match get("hw") {
        Some(Value::String(s) | Value::Ident(s)) => Some(s.clone()),
        _ => None,
    };

    let hw_fallback = match get("hw_fallback") {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    };

    let mut settings = CompiledTranscodeSettings::new(preserve, crf, preset, bitrate, channels);
    settings.hw = hw;
    settings.hw_fallback = hw_fallback;

    CompiledOperation::Transcode {
        target: parse_track_target(target),
        codec: canonical,
        settings,
    }
}

fn compile_synthesize(
    name: &str,
    settings: &[SynthSetting],
) -> std::result::Result<CompiledSynthesize, DslError> {
    let mut codec = None;
    let mut channels = None;
    let mut source = None;
    let mut bitrate = None;
    let mut skip_if_exists = None;
    let mut create_if = None;
    let mut title = None;
    let mut language = None;
    let mut position = None;

    for setting in settings {
        match setting {
            SynthSetting::Codec(c) => {
                codec = Some(
                    codecs::normalize_codec(c)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| c.clone()),
                );
            }
            SynthSetting::Channels(v) => {
                channels = Some(match v {
                    Value::Number(n, _) => SynthChannels::Count(safe_u32(*n).unwrap_or(0)),
                    Value::Ident(s) => SynthChannels::Named(s.clone()),
                    _ => SynthChannels::Named(format!("{v:?}")),
                });
            }
            SynthSetting::Source(f) => source = Some(compile_filter(f)?),
            SynthSetting::Bitrate(b) => bitrate = Some(b.clone()),
            SynthSetting::SkipIfExists(f) => skip_if_exists = Some(compile_filter(f)?),
            SynthSetting::CreateIf(c) => create_if = Some(compile_condition(c)?),
            SynthSetting::Title(t) => title = Some(t.clone()),
            SynthSetting::Language(l) => {
                language = Some(if l == "inherit" {
                    SynthLanguage::Inherit
                } else {
                    SynthLanguage::Fixed(l.clone())
                });
            }
            SynthSetting::Position(v) => {
                position = Some(match v {
                    Value::Number(n, _) => SynthPosition::Index(safe_u32(*n).unwrap_or(0)),
                    Value::Ident(s) => SynthPosition::Named(s.clone()),
                    _ => SynthPosition::Named(format!("{v:?}")),
                });
            }
        }
    }

    let mut synth = CompiledSynthesize::new(name.to_string());
    synth.codec = codec;
    synth.channels = channels;
    synth.source = source;
    synth.bitrate = bitrate;
    synth.skip_if_exists = skip_if_exists;
    synth.create_if = create_if;
    synth.title = title;
    synth.language = language;
    synth.position = position;
    Ok(synth)
}

fn compile_conditional(when: &WhenNode) -> std::result::Result<CompiledConditional, DslError> {
    let condition = compile_condition(&when.condition)?;
    let then_actions: Vec<CompiledAction> = when
        .then_actions
        .iter()
        .map(compile_action)
        .collect::<std::result::Result<_, _>>()?;
    let else_actions: Vec<CompiledAction> = when
        .else_actions
        .iter()
        .map(compile_action)
        .collect::<std::result::Result<_, _>>()?;

    Ok(CompiledConditional::new(
        condition,
        then_actions,
        else_actions,
    ))
}

fn compile_condition(cond: &ConditionNode) -> std::result::Result<CompiledCondition, DslError> {
    match cond {
        ConditionNode::Exists(query) => Ok(CompiledCondition::Exists {
            target: parse_track_target(&query.target),
            filter: query.filter.as_ref().map(compile_filter).transpose()?,
        }),
        ConditionNode::Count(query, op, value) => Ok(CompiledCondition::Count {
            target: parse_track_target(&query.target),
            filter: query.filter.as_ref().map(compile_filter).transpose()?,
            op: compile_compare_op(op),
            value: *value,
        }),
        ConditionNode::FieldCompare(path, op, value) => Ok(CompiledCondition::FieldCompare {
            path: path.clone(),
            op: compile_compare_op(op),
            value: value_to_json(value),
        }),
        ConditionNode::FieldExists(path) => {
            Ok(CompiledCondition::FieldExists { path: path.clone() })
        }
        ConditionNode::AudioIsMultiLanguage => Ok(CompiledCondition::AudioIsMultiLanguage),
        ConditionNode::IsDubbed => Ok(CompiledCondition::IsDubbed),
        ConditionNode::IsOriginal => Ok(CompiledCondition::IsOriginal),
        ConditionNode::And(items) => {
            let compiled: Vec<CompiledCondition> = items
                .iter()
                .map(compile_condition)
                .collect::<std::result::Result<_, _>>()?;
            Ok(CompiledCondition::And(compiled))
        }
        ConditionNode::Or(items) => {
            let compiled: Vec<CompiledCondition> = items
                .iter()
                .map(compile_condition)
                .collect::<std::result::Result<_, _>>()?;
            Ok(CompiledCondition::Or(compiled))
        }
        ConditionNode::Not(inner) => {
            Ok(CompiledCondition::Not(Box::new(compile_condition(inner)?)))
        }
    }
}

fn compile_filter(filter: &FilterNode) -> std::result::Result<CompiledFilter, DslError> {
    match filter {
        FilterNode::LangIn(langs) => Ok(CompiledFilter::LangIn(langs.clone())),
        FilterNode::LangCompare(op, lang) => Ok(CompiledFilter::LangCompare(
            compile_compare_op(op),
            lang.clone(),
        )),
        FilterNode::CodecCompare(op, codec) => {
            let normalized = codecs::normalize_codec(codec)
                .map(|s| s.to_string())
                .unwrap_or_else(|| codec.clone());
            Ok(CompiledFilter::CodecCompare(
                compile_compare_op(op),
                normalized,
            ))
        }
        FilterNode::CodecIn(codec_list) => {
            let normalized: Vec<String> = codec_list
                .iter()
                .map(|c| {
                    codecs::normalize_codec(c)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| c.clone())
                })
                .collect();
            Ok(CompiledFilter::CodecIn(normalized))
        }
        FilterNode::Channels(op, val) => Ok(CompiledFilter::Channels(compile_compare_op(op), *val)),
        FilterNode::Commentary => Ok(CompiledFilter::Commentary),
        FilterNode::Forced => Ok(CompiledFilter::Forced),
        FilterNode::Default => Ok(CompiledFilter::Default),
        FilterNode::Font => Ok(CompiledFilter::Font),
        FilterNode::TitleContains(s) => Ok(CompiledFilter::TitleContains(s.clone())),
        FilterNode::TitleMatches(s) => {
            let compiled = CompiledRegex::new(s)
                .map_err(|e| DslError::compile(format!("invalid regex pattern '{s}': {e}")))?;
            Ok(CompiledFilter::TitleMatches(compiled))
        }
        FilterNode::And(items) => {
            let compiled: Vec<CompiledFilter> = items
                .iter()
                .map(compile_filter)
                .collect::<std::result::Result<_, _>>()?;
            Ok(CompiledFilter::And(compiled))
        }
        FilterNode::Or(items) => {
            let compiled: Vec<CompiledFilter> = items
                .iter()
                .map(compile_filter)
                .collect::<std::result::Result<_, _>>()?;
            Ok(CompiledFilter::Or(compiled))
        }
        FilterNode::Not(inner) => Ok(CompiledFilter::Not(Box::new(compile_filter(inner)?))),
    }
}

fn compile_action(action: &ActionNode) -> std::result::Result<CompiledAction, DslError> {
    match action {
        ActionNode::Skip(phase) => Ok(CompiledAction::Skip(phase.clone())),
        ActionNode::Warn(msg) => Ok(CompiledAction::Warn(msg.clone())),
        ActionNode::Fail(msg) => Ok(CompiledAction::Fail(msg.clone())),
        ActionNode::SetDefault(track_ref) => Ok(CompiledAction::SetDefault {
            target: parse_track_target(&track_ref.target),
            filter: track_ref.filter.as_ref().map(compile_filter).transpose()?,
        }),
        ActionNode::SetForced(track_ref) => Ok(CompiledAction::SetForced {
            target: parse_track_target(&track_ref.target),
            filter: track_ref.filter.as_ref().map(compile_filter).transpose()?,
        }),
        ActionNode::SetLanguage(track_ref, val) => Ok(CompiledAction::SetLanguage {
            target: parse_track_target(&track_ref.target),
            filter: track_ref.filter.as_ref().map(compile_filter).transpose()?,
            value: compile_value_or_field(val),
        }),
        ActionNode::SetTag(tag, val) => Ok(CompiledAction::SetTag {
            tag: tag.clone(),
            value: compile_value_or_field(val),
        }),
    }
}

fn compile_value_or_field(vof: &ValueOrField) -> CompiledValueOrField {
    match vof {
        ValueOrField::Value(v) => CompiledValueOrField::Value(value_to_json(v)),
        ValueOrField::Field(path) => CompiledValueOrField::Field(path.clone()),
    }
}

fn compile_compare_op(op: &CompareOp) -> CompiledCompareOp {
    match op {
        CompareOp::Eq => CompiledCompareOp::Eq,
        CompareOp::Ne => CompiledCompareOp::Ne,
        CompareOp::Lt => CompiledCompareOp::Lt,
        CompareOp::Le => CompiledCompareOp::Le,
        CompareOp::Gt => CompiledCompareOp::Gt,
        CompareOp::Ge => CompiledCompareOp::Ge,
        CompareOp::In => CompiledCompareOp::In,
    }
}

fn parse_track_target(target: &str) -> TrackTarget {
    match target {
        "video" => TrackTarget::Video,
        "audio" => TrackTarget::Audio,
        "subtitle" | "subtitles" => TrackTarget::Subtitle,
        "attachment" | "attachments" => TrackTarget::Attachment,
        "track" => TrackTarget::Any,
        _ => unreachable!("validator rejects unknown track targets"),
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::String(s) | Value::Ident(s) => serde_json::Value::String(s.clone()),
        Value::Number(n, _) => serde_json::json!(n),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::List(items) => {
            let arr: Vec<serde_json::Value> = items.iter().map(value_to_json).collect();
            serde_json::Value::Array(arr)
        }
    }
}

/// Topologically sort phases based on their dependencies.
fn topological_sort(ast: &PolicyAst) -> std::result::Result<Vec<String>, DslError> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for phase in &ast.phases {
        adj.entry(phase.name.as_str()).or_default();
        in_degree.entry(phase.name.as_str()).or_insert(0);
        for dep in &phase.depends_on {
            adj.entry(dep.as_str())
                .or_default()
                .push(phase.name.as_str());
            *in_degree.entry(phase.name.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    queue.sort(); // deterministic ordering

    let mut result = Vec::new();
    while let Some(node) = queue.pop() {
        result.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            for &neighbor in neighbors {
                let deg = in_degree.get_mut(neighbor).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    // Insert sorted for determinism
                    let pos = queue.partition_point(|&x| x > neighbor);
                    queue.insert(pos, neighbor);
                }
            }
        }
    }

    if result.len() != ast.phases.len() {
        return Err(DslError::compile(
            "cannot determine phase order due to circular dependencies",
        ));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: run the full parse → validate → compile pipeline.
    fn compile(
        source: &str,
    ) -> std::result::Result<CompiledPolicy, crate::errors::DslPipelineError> {
        crate::compile_policy(source)
    }

    #[test]
    fn test_compile_minimal() {
        let policy = compile(
            r#"policy "test" {
            phase init {
                container mkv
            }
        }"#,
        )
        .unwrap();

        assert_eq!(policy.name, "test");
        assert_eq!(policy.phases.len(), 1);
        assert_eq!(policy.phases[0].name, "init");
        assert_eq!(policy.phase_order, vec!["init"]);
        match &policy.phases[0].operations[0] {
            CompiledOperation::SetContainer(name) => assert_eq!(name, "mkv"),
            _ => panic!("expected SetContainer"),
        }
    }

    #[test]
    fn test_compile_with_config() {
        let policy = compile(
            r#"policy "test" {
            config {
                languages audio: [eng, und]
                languages subtitle: [eng]
                on_error: continue
                commentary_patterns: ["commentary"]
            }
            phase init { container mkv }
        }"#,
        )
        .unwrap();

        assert_eq!(policy.config.audio_languages, vec!["eng", "und"]);
        assert_eq!(policy.config.subtitle_languages, vec!["eng"]);
        assert_eq!(policy.config.on_error, ErrorStrategy::Continue);
        assert_eq!(policy.config.commentary_patterns, vec!["commentary"]);
    }

    #[test]
    fn test_compile_phase_order() {
        let policy = compile(
            r#"policy "test" {
            phase c {
                depends_on: [a, b]
                container mkv
            }
            phase a {
                container mkv
            }
            phase b {
                depends_on: [a]
                container mkv
            }
        }"#,
        )
        .unwrap();

        // a has no deps, b depends on a, c depends on a and b
        assert_eq!(policy.phase_order[0], "a");
        assert_eq!(policy.phase_order[1], "b");
        assert_eq!(policy.phase_order[2], "c");
    }

    #[test]
    fn test_compile_codec_normalization() {
        let policy = compile(
            r#"policy "test" {
            phase tc {
                transcode video to h265 {
                    crf: 20
                }
            }
        }"#,
        )
        .unwrap();

        match &policy.phases[0].operations[0] {
            CompiledOperation::Transcode { codec, .. } => {
                assert_eq!(codec, "hevc"); // h265 → hevc
            }
            _ => panic!("expected Transcode"),
        }
    }

    #[test]
    fn test_compile_keep_remove() {
        let policy = compile(
            r#"policy "test" {
            phase norm {
                keep audio where lang in [eng, jpn]
                remove attachments where not font
            }
        }"#,
        )
        .unwrap();

        match &policy.phases[0].operations[0] {
            CompiledOperation::Keep { target, filter } => {
                assert_eq!(*target, TrackTarget::Audio);
                assert!(filter.is_some());
            }
            _ => panic!("expected Keep"),
        }
        match &policy.phases[0].operations[1] {
            CompiledOperation::Remove { target, filter } => {
                assert_eq!(*target, TrackTarget::Attachment);
                assert!(filter.is_some());
            }
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn test_compile_conditional() {
        let policy = compile(
            r#"policy "test" {
            phase validate {
                when exists(audio where lang == jpn) {
                    warn "has japanese audio"
                }
            }
        }"#,
        )
        .unwrap();

        match &policy.phases[0].operations[0] {
            CompiledOperation::Conditional(cond) => {
                assert_eq!(cond.then_actions.len(), 1);
                match &cond.then_actions[0] {
                    CompiledAction::Warn(msg) => assert!(msg.contains("japanese")),
                    _ => panic!("expected Warn"),
                }
            }
            _ => panic!("expected Conditional"),
        }
    }

    #[test]
    fn test_compile_run_if() {
        let policy = compile(
            r#"policy "test" {
            phase tc {
                container mkv
            }
            phase validate {
                depends_on: [tc]
                run_if tc.modified
                when exists(audio where lang == eng) {
                    warn "has english"
                }
            }
        }"#,
        )
        .unwrap();

        let run_if = policy.phases[1].run_if.as_ref().unwrap();
        assert_eq!(run_if.phase, "tc");
        assert_eq!(run_if.trigger, RunIfTrigger::Modified);
    }

    #[test]
    fn test_compile_defaults() {
        let policy = compile(
            r#"policy "test" {
            phase norm {
                defaults {
                    audio: first_per_language
                    subtitle: none
                }
            }
        }"#,
        )
        .unwrap();

        match &policy.phases[0].operations[0] {
            CompiledOperation::SetDefaults(defaults) => {
                assert_eq!(defaults.len(), 2);
                assert_eq!(defaults[0].target, TrackTarget::Audio);
                assert_eq!(defaults[0].strategy, DefaultStrategy::FirstPerLanguage);
                assert_eq!(defaults[1].target, TrackTarget::Subtitle);
                assert_eq!(defaults[1].strategy, DefaultStrategy::None);
            }
            _ => panic!("expected SetDefaults"),
        }
    }

    #[test]
    fn test_compile_rejects_unknown_codec() {
        let err = compile(
            r#"policy "test" {
            phase tc {
                transcode video to h256 {
                    crf: 20
                }
            }
        }"#,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown codec"));
    }

    #[test]
    fn test_compile_production_policy() {
        let input = include_str!("../tests/fixtures/production-normalize.voom");
        let policy = compile(input).unwrap();
        assert_eq!(policy.name, "production-normalize");
        assert_eq!(policy.phases.len(), 6);
        // Check topological order respects dependencies
        let pos = |name: &str| policy.phase_order.iter().position(|p| p == name).unwrap();
        assert!(pos("containerize") < pos("normalize"));
        assert!(pos("normalize") < pos("audio_compat"));
        assert!(pos("transcode") < pos("validate"));
        assert!(pos("audio_compat") < pos("validate"));
    }

    #[test]
    fn test_compiled_policy_has_source_hash() {
        let source = r#"policy "test" {
            phase init {
                container mkv
            }
        }"#;
        let policy = compile(source).unwrap();
        assert!(!policy.source_hash.is_empty());
        assert_eq!(policy.source_hash.len(), 16); // 64-bit hex

        // Same source produces same hash
        let policy2 = compile(source).unwrap();
        assert_eq!(policy.source_hash, policy2.source_hash);
    }

    #[test]
    fn test_compile_serde_roundtrip() {
        let input = include_str!("../tests/fixtures/production-normalize.voom");
        let policy = compile(input).unwrap();
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: CompiledPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, policy.name);
        assert_eq!(deserialized.phases.len(), policy.phases.len());
        assert_eq!(deserialized.phase_order, policy.phase_order);
    }

    #[test]
    fn test_compile_clear_tags() {
        let policy = compile(
            r#"policy "test" {
            phase clean {
                clear_tags
            }
        }"#,
        )
        .unwrap();
        assert!(matches!(
            &policy.phases[0].operations[0],
            CompiledOperation::ClearTags
        ));
    }

    #[test]
    fn test_compile_set_tag() {
        let policy = compile(
            r#"policy "test" {
            phase clean {
                set_tag "title" "My Movie"
            }
        }"#,
        )
        .unwrap();
        match &policy.phases[0].operations[0] {
            CompiledOperation::SetTag { tag, value } => {
                assert_eq!(tag, "title");
                match value {
                    CompiledValueOrField::Value(v) => assert_eq!(v, "My Movie"),
                    other => panic!("expected Value, got {other:?}"),
                }
            }
            other => panic!("expected SetTag, got {other:?}"),
        }
    }

    #[test]
    fn test_compile_delete_tag() {
        let policy = compile(
            r#"policy "test" {
            phase clean {
                delete_tag "encoder"
            }
        }"#,
        )
        .unwrap();
        match &policy.phases[0].operations[0] {
            CompiledOperation::DeleteTag(tag) => assert_eq!(tag, "encoder"),
            other => panic!("expected DeleteTag, got {other:?}"),
        }
    }
}
