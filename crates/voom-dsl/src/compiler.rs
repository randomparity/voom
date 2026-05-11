//! Compiler: AST → `CompiledPolicy`.
//!
//! Transforms a validated [`PolicyAst`] into a [`CompiledPolicy`] structure
//! that uses domain types and is ready for evaluation by the policy evaluator plugin.
//!
//! The `.unwrap()` calls in this module operate on grammar-guaranteed structures
//! from the pest parser — the AST shape is validated before compilation.
#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};

use crate::compiled::{
    ClearActionsSettings, CompiledAction, CompiledCompareOp, CompiledCondition,
    CompiledConditional, CompiledConfig, CompiledDefault, CompiledFilter, CompiledMetadata,
    CompiledOperation, CompiledPhase, CompiledPhaseComposition, CompiledPolicy, CompiledRegex,
    CompiledRule, CompiledRunIf, CompiledSynthesize, CompiledTranscodeSettings,
    CompiledValueOrField, DefaultStrategy, ErrorStrategy, LoudnessNormalization, LoudnessPreset,
    PhaseCompositionKind, RulesMode, RunIfTrigger, SynthLanguage, SynthPosition, TrackTarget,
    TranscodeChannels,
};
use voom_domain::plan::{CropSettings, SampleStrategy, TranscodeFallback};
use voom_domain::utils::codecs;

use crate::ast::{
    ActionNode, CompareOp, ConditionNode, ConfigNode, ErrorStrategyNode, FilterNode, MetadataNode,
    NormalizeSetting, OperationNode, PhaseNode, PolicyAst, RunIfTriggerNode, SynthSetting, Value,
    ValueOrField, VerifyMode, WhenNode,
};
use crate::composition::{PhaseComposition, ResolvedPolicyAst};
use crate::errors::DslError;

/// Safely convert an f64 to u32, returning None for negative, fractional, or out-of-range values.
fn safe_u32(n: f64) -> Option<u32> {
    if n >= 0.0 && n <= f64::from(u32::MAX) && n.fract() == 0.0 {
        // Bounded above by the explicit range check; safe to cast.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(n as u32)
    } else {
        None
    }
}

/// Compile a pre-parsed and validated AST into a [`CompiledPolicy`].
pub(crate) fn compile_ast(ast: &PolicyAst) -> std::result::Result<CompiledPolicy, DslError> {
    compile_ast_with_composition(ast, Vec::new(), &HashMap::new())
}

/// Compile a composition-resolved AST into a [`CompiledPolicy`].
pub(crate) fn compile_resolved_ast(
    resolved: &ResolvedPolicyAst,
) -> std::result::Result<CompiledPolicy, DslError> {
    compile_ast_with_composition(
        &resolved.ast,
        resolved.extends_chain.clone(),
        &resolved.phase_sources,
    )
}

fn compile_ast_with_composition(
    ast: &PolicyAst,
    extends_chain: Vec<String>,
    phase_sources: &HashMap<String, PhaseComposition>,
) -> std::result::Result<CompiledPolicy, DslError> {
    let metadata = compile_metadata(ast.metadata.as_ref(), extends_chain);
    let config = compile_config(ast.config.as_ref());
    let phases: Vec<CompiledPhase> = ast
        .phases
        .iter()
        .map(|phase| compile_phase(phase, phase_sources.get(&phase.name)))
        .collect::<std::result::Result<_, _>>()?;
    let phase_order = topological_sort(ast)?;

    Ok(CompiledPolicy::new(
        ast.name.clone(),
        metadata,
        config,
        phases,
        phase_order,
        String::new(),
    ))
}

fn compile_metadata(
    metadata: Option<&MetadataNode>,
    extends_chain: Vec<String>,
) -> CompiledMetadata {
    let Some(metadata) = metadata else {
        return CompiledMetadata {
            extends_chain,
            ..CompiledMetadata::default()
        };
    };

    CompiledMetadata {
        version: metadata.version.clone(),
        author: metadata.author.clone(),
        description: metadata.description.clone(),
        requires_voom: metadata.requires_voom.clone(),
        requires_tools: metadata.requires_tools.clone().unwrap_or_default(),
        test_fixtures: metadata.test_fixtures.clone().unwrap_or_default(),
        extends_chain,
    }
}

fn compile_config(config: Option<&ConfigNode>) -> CompiledConfig {
    match config {
        Some(c) => CompiledConfig::new(
            c.audio_languages.clone(),
            c.subtitle_languages.clone(),
            c.on_error
                .map(compile_error_strategy)
                .unwrap_or(ErrorStrategy::Abort),
            c.commentary_patterns.clone(),
            c.keep_backups.unwrap_or(false),
        ),
        None => CompiledConfig::new(vec![], vec![], ErrorStrategy::Abort, vec![], false),
    }
}

/// Parse an error strategy string. Returns `None` for unrecognized values.
/// Used by compiler unit tests that cover each public DSL token.
#[cfg(test)]
pub(crate) fn parse_error_strategy(value: &str) -> Option<ErrorStrategy> {
    match value {
        "continue" => Some(ErrorStrategy::Continue),
        "skip" => Some(ErrorStrategy::Skip),
        "abort" => Some(ErrorStrategy::Abort),
        "quarantine" => Some(ErrorStrategy::Quarantine),
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

fn compile_phase(
    phase: &PhaseNode,
    composition: Option<&PhaseComposition>,
) -> std::result::Result<CompiledPhase, DslError> {
    if phase.extend {
        return Err(DslError::compile(format!(
            "phase \"{}\" uses extend but policy composition was not resolved",
            phase.name
        )));
    }

    let skip_when = phase
        .skip_when
        .as_ref()
        .map(compile_condition)
        .transpose()?;

    let run_if = phase
        .run_if
        .as_ref()
        .map(|r| CompiledRunIf::new(r.phase.clone(), compile_run_if_trigger(r.trigger)));

    let operations: Vec<CompiledOperation> = phase
        .operations
        .iter()
        .map(|spanned| compile_operation(&spanned.node))
        .collect::<std::result::Result<_, _>>()?;

    Ok(CompiledPhase {
        name: phase.name.clone(),
        depends_on: phase.depends_on.clone().unwrap_or_default(),
        skip_when,
        run_if,
        on_error: phase
            .on_error
            .map(compile_error_strategy)
            .unwrap_or(ErrorStrategy::Abort),
        composition: compile_phase_composition(composition),
        operations,
    })
}

fn compile_phase_composition(composition: Option<&PhaseComposition>) -> CompiledPhaseComposition {
    match composition {
        None | Some(PhaseComposition::Local) => CompiledPhaseComposition {
            kind: PhaseCompositionKind::Local,
            source: None,
            added_operations: 0,
        },
        Some(PhaseComposition::Inherited { source }) => CompiledPhaseComposition {
            kind: PhaseCompositionKind::Inherited,
            source: Some(source.clone()),
            added_operations: 0,
        },
        Some(PhaseComposition::Extended {
            source,
            added_operations,
        }) => CompiledPhaseComposition {
            kind: PhaseCompositionKind::Extended,
            source: Some(source.clone()),
            added_operations: *added_operations,
        },
        Some(PhaseComposition::Overridden { source }) => CompiledPhaseComposition {
            kind: PhaseCompositionKind::Overridden,
            source: Some(source.clone()),
            added_operations: 0,
        },
    }
}

fn compile_error_strategy(strategy: ErrorStrategyNode) -> ErrorStrategy {
    match strategy {
        ErrorStrategyNode::Continue => ErrorStrategy::Continue,
        ErrorStrategyNode::Skip => ErrorStrategy::Skip,
        ErrorStrategyNode::Abort => ErrorStrategy::Abort,
        ErrorStrategyNode::Quarantine => ErrorStrategy::Quarantine,
    }
}

fn compile_run_if_trigger(trigger: RunIfTriggerNode) -> RunIfTrigger {
    match trigger {
        RunIfTriggerNode::Modified => RunIfTrigger::Modified,
        RunIfTriggerNode::Completed => RunIfTrigger::Completed,
    }
}

fn compile_operation(op: &OperationNode) -> std::result::Result<CompiledOperation, DslError> {
    match op {
        OperationNode::Container(name) => {
            let container = voom_domain::media::Container::from_extension(name);
            if container == voom_domain::media::Container::Other {
                return Err(DslError::compile(format!(
                    "unknown container '{name}'; expected one of: {}",
                    voom_domain::media::Container::known_extensions().join(", ")
                )));
            }
            Ok(CompiledOperation::SetContainer(container))
        }
        OperationNode::Keep {
            target,
            filter,
            normalize,
        } => {
            let target = parse_track_target(target);
            let filter = filter.as_ref().map(compile_filter).transpose()?;
            if let Some(normalize) = normalize {
                Ok(CompiledOperation::NormalizeAudio {
                    target,
                    filter,
                    settings: compile_normalize(normalize)?,
                })
            } else {
                Ok(CompiledOperation::Keep { target, filter })
            }
        }
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
        OperationNode::Verify { mode } => Ok(CompiledOperation::Verify {
            mode: match mode {
                VerifyMode::Quick => voom_domain::verification::VerificationMode::Quick,
                VerifyMode::Thorough => voom_domain::verification::VerificationMode::Thorough,
                VerifyMode::Hash => voom_domain::verification::VerificationMode::Hash,
            },
        }),
    }
}

fn compile_transcode(target: &str, codec: &str, settings: &[(String, Value)]) -> CompiledOperation {
    let canonical = codecs::normalize_codec(codec)
        .map_or_else(|| codec.to_string(), std::string::ToString::to_string);

    let get =
        |key: &str| -> Option<&Value> { settings.iter().find(|(k, _)| k == key).map(|(_, v)| v) };
    let get_str = |key: &str| -> Option<String> {
        match get(key) {
            Some(Value::Ident(s) | Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    };

    let preserve = match get("preserve") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) | Value::Ident(s) | Value::Number(_, s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };

    let crf = match get("crf") {
        Some(Value::Number(n, _)) => safe_u32(*n),
        _ => None,
    };

    let bitrate = match get("bitrate") {
        Some(Value::String(s) | Value::Ident(s) | Value::Number(_, s)) => Some(s.clone()),
        _ => None,
    };

    let target_vmaf = match get("target_vmaf") {
        Some(Value::Number(n, _)) => safe_u32(*n),
        _ => None,
    };

    let max_bitrate = match get("max_bitrate") {
        Some(Value::String(s) | Value::Ident(s) | Value::Number(_, s)) => Some(s.clone()),
        _ => None,
    };

    let min_bitrate = match get("min_bitrate") {
        Some(Value::String(s) | Value::Ident(s) | Value::Number(_, s)) => Some(s.clone()),
        _ => None,
    };

    let channels = match get("channels") {
        Some(Value::Number(n, _)) => safe_u32(*n).map(TranscodeChannels::Count),
        Some(Value::Ident(s) | Value::String(s)) => Some(TranscodeChannels::Named(s.clone())),
        _ => None,
    };

    let hw_fallback = match get("hw_fallback") {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    };

    let max_resolution = match get("max_resolution") {
        Some(Value::Ident(s) | Value::String(s)) => Some(s.clone()),
        Some(Value::Number(_, raw)) => Some(raw.clone()),
        _ => None,
    };

    let vmaf_overrides = parse_vmaf_overrides(settings);

    let mut compiled_settings =
        CompiledTranscodeSettings::new(preserve, crf, get_str("preset"), bitrate, channels);
    compiled_settings.target_vmaf = target_vmaf;
    compiled_settings.max_bitrate = max_bitrate;
    compiled_settings.min_bitrate = min_bitrate;
    compiled_settings.sample_strategy = parse_sample_strategy(get("sample_strategy"));
    compiled_settings.fallback = parse_transcode_fallback(get("fallback"));
    compiled_settings.vmaf_overrides = vmaf_overrides;
    compiled_settings.hw = get_str("hw");
    compiled_settings.hw_fallback = hw_fallback;
    compiled_settings.max_resolution = max_resolution;
    compiled_settings.scale_algorithm = get_str("scale_algorithm");
    compiled_settings.hdr_mode = get_str("hdr_mode");
    compiled_settings.preserve_hdr = match get("preserve_hdr") {
        Some(Value::Bool(value)) => Some(*value),
        _ => None,
    };
    compiled_settings.tonemap = get_str("tonemap");
    compiled_settings.hdr_color_metadata = get_str("hdr_color_metadata");
    compiled_settings.dolby_vision = get_str("dolby_vision");
    compiled_settings.tune = get_str("tune");
    compiled_settings.crop = compile_crop_settings(get, &get_str).map(Box::new);
    compiled_settings.loudness = get("normalize")
        .and_then(value_to_normalize)
        .or_else(|| get_str("normalize").and_then(|name| normalize_from_preset(&name)));

    CompiledOperation::Transcode {
        target: parse_track_target(target),
        codec: canonical,
        settings: compiled_settings,
    }
}

fn compile_crop_settings<'a>(
    get: impl Fn(&str) -> Option<&'a Value>,
    get_str: &impl Fn(&str) -> Option<String>,
) -> Option<CropSettings> {
    if get_str("crop").as_deref() != Some("auto") {
        return None;
    }

    let mut crop = CropSettings::auto();
    crop.sample_duration_secs = get_u32(get("crop_sample_duration"));
    crop.sample_count = get_u32(get("crop_sample_count"));
    crop.threshold = get_u32(get("crop_threshold")).and_then(|v| u8::try_from(v).ok());
    crop.minimum_crop = get_u32(get("crop_minimum"));
    crop.preserve_bottom_pixels = get_u32(get("crop_preserve_bottom_pixels"));
    crop.aspect_lock = match get("crop_aspect_lock") {
        Some(Value::List(items)) => items.iter().filter_map(value_as_string).collect(),
        _ => Vec::new(),
    };
    Some(crop)
}

fn get_u32(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::Number(n, _)) => safe_u32(*n),
        _ => None,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::Ident(s) | Value::String(s) | Value::Number(_, s) => Some(s.clone()),
        _ => None,
    }
}

fn parse_sample_strategy(value: Option<&Value>) -> Option<SampleStrategy> {
    match value {
        Some(Value::Ident(name)) if name == "full" => Some(SampleStrategy::Full),
        Some(Value::Call { name, args }) if name == "scenes" || name == "uniform" => {
            let count = args
                .iter()
                .find(|(key, _)| key == "count")
                .and_then(|(_, value)| match value {
                    Value::Number(n, _) => safe_u32(*n),
                    _ => None,
                })?;
            let duration = args.iter().find(|(key, _)| key == "duration").and_then(
                |(_, value)| match value {
                    Value::String(s) | Value::Ident(s) | Value::Number(_, s) => Some(s.clone()),
                    _ => None,
                },
            )?;
            if name == "scenes" {
                Some(SampleStrategy::Scenes { count, duration })
            } else {
                Some(SampleStrategy::Uniform { count, duration })
            }
        }
        _ => None,
    }
}

fn parse_transcode_fallback(value: Option<&Value>) -> Option<TranscodeFallback> {
    let Some(Value::Object(items)) = value else {
        return None;
    };
    let crf = items
        .iter()
        .find(|(key, _)| key == "crf")
        .and_then(|(_, value)| match value {
            Value::Number(n, _) => safe_u32(*n),
            _ => None,
        })?;
    let preset =
        items
            .iter()
            .find(|(key, _)| key == "preset")
            .and_then(|(_, value)| match value {
                Value::String(s) | Value::Ident(s) => Some(s.clone()),
                _ => None,
            })?;
    Some(TranscodeFallback::new(crf, preset))
}

fn parse_vmaf_overrides(settings: &[(String, Value)]) -> Option<HashMap<String, u32>> {
    let mut overrides = HashMap::new();
    for (key, value) in settings {
        let Some(path) = key.strip_prefix("target_vmaf_when ") else {
            continue;
        };
        let Some(content_type) = path.strip_prefix("content.") else {
            continue;
        };
        if let Value::Number(n, _) = value {
            if let Some(target) = safe_u32(*n) {
                overrides.insert(content_type.to_string(), target);
            }
        }
    }
    if overrides.is_empty() {
        None
    } else {
        Some(overrides)
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
    let mut loudness = None;

    for setting in settings {
        match setting {
            SynthSetting::Codec(c) => {
                codec = Some(
                    codecs::normalize_codec(c)
                        .map_or_else(|| c.clone(), std::string::ToString::to_string),
                );
            }
            SynthSetting::Channels(v) => {
                channels = match v {
                    Value::Number(n, _) => safe_u32(*n).map(TranscodeChannels::Count),
                    Value::Ident(s) | Value::String(s) => Some(TranscodeChannels::Named(s.clone())),
                    _ => None,
                };
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
            SynthSetting::Normalize(setting) => loudness = Some(compile_normalize(setting)?),
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
    synth.loudness = loudness;
    Ok(synth)
}

fn value_to_normalize(value: &Value) -> Option<LoudnessNormalization> {
    match value {
        Value::Ident(name) | Value::String(name) => normalize_from_preset(name),
        _ => None,
    }
}

fn normalize_from_preset(name: &str) -> Option<LoudnessNormalization> {
    LoudnessPreset::parse(name).map(LoudnessPreset::defaults)
}

fn compile_normalize(
    setting: &NormalizeSetting,
) -> std::result::Result<LoudnessNormalization, DslError> {
    let Some(preset) = LoudnessPreset::parse(&setting.preset) else {
        return Err(DslError::compile(format!(
            "unknown loudness preset '{}'",
            setting.preset
        )));
    };
    let mut loudness = preset.defaults();
    for (key, value) in &setting.settings {
        match (key.as_str(), value) {
            ("target_lufs", Value::Number(n, _)) => loudness.target_lufs = *n,
            ("true_peak_db", Value::Number(n, _)) => loudness.true_peak_db = *n,
            ("lra_max", Value::Number(n, _)) => loudness.lra_max = Some(*n),
            ("tolerance_lufs", Value::Number(n, _)) => loudness.tolerance_lufs = *n,
            _ => {}
        }
    }
    Ok(loudness)
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
        FilterNode::LangField(op, path) => Ok(CompiledFilter::LangField(
            compile_compare_op(op),
            path.clone(),
        )),
        FilterNode::CodecCompare(op, codec) => {
            let normalized = codecs::normalize_codec(codec)
                .map_or_else(|| codec.clone(), std::string::ToString::to_string);
            Ok(CompiledFilter::CodecCompare(
                compile_compare_op(op),
                normalized,
            ))
        }
        FilterNode::CodecField(op, path) => Ok(CompiledFilter::CodecField(
            compile_compare_op(op),
            path.clone(),
        )),
        FilterNode::CodecIn(codec_list) => {
            let normalized: Vec<String> = codec_list
                .iter()
                .map(|c| {
                    codecs::normalize_codec(c)
                        .map_or_else(|| c.clone(), std::string::ToString::to_string)
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
        ActionNode::Keep { target, filter } => Ok(CompiledAction::Keep {
            target: parse_track_target(target),
            filter: filter.as_ref().map(compile_filter).transpose()?,
        }),
        ActionNode::Remove { target, filter } => Ok(CompiledAction::Remove {
            target: parse_track_target(target),
            filter: filter.as_ref().map(compile_filter).transpose()?,
        }),
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
        Value::Object(items) => {
            let map = items
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect();
            serde_json::Value::Object(map)
        }
        Value::Call { name, args } => {
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), serde_json::Value::String(name.clone()));
            map.insert(
                "args".to_string(),
                serde_json::Value::Object(
                    args.iter()
                        .map(|(key, value)| (key.clone(), value_to_json(value)))
                        .collect(),
                ),
            );
            serde_json::Value::Object(map)
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
        for dep in phase.depends_on.as_deref().unwrap_or(&[]) {
            adj.entry(dep.as_str())
                .or_default()
                .push(phase.name.as_str());
            *in_degree.entry(phase.name.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    queue.sort_unstable(); // deterministic ordering

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
        let phase_names: HashSet<&str> = ast.phases.iter().map(|p| p.name.as_str()).collect();
        let result_set: HashSet<&str> = result.iter().map(std::string::String::as_str).collect();
        let mut stuck: Vec<&str> = phase_names.difference(&result_set).copied().collect();
        stuck.sort_unstable();
        let has_unknown_dep = stuck.iter().any(|&name| {
            ast.phases.iter().find(|p| p.name == name).is_some_and(|p| {
                p.depends_on
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .any(|d| !phase_names.contains(d.as_str()))
            })
        });
        return if has_unknown_dep {
            Err(DslError::compile(format!(
                "phases [{}] reference unknown dependencies",
                stuck.join(", "),
            )))
        } else {
            Err(DslError::compile(format!(
                "circular dependency involving phases: [{}]",
                stuck.join(", "),
            )))
        };
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
            CompiledOperation::SetContainer(container) => {
                assert_eq!(*container, voom_domain::media::Container::Mkv);
            }
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
                    CompiledValueOrField::Field(_) => {
                        panic!("expected Value, got Field")
                    }
                }
            }
            other => panic!("expected SetTag, got {other:?}"),
        }
    }

    #[test]
    fn test_compile_lang_field_filter() {
        let policy = compile(
            r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.radarr.original_language
            }
        }"#,
        )
        .unwrap();
        match &policy.phases[0].operations[0] {
            CompiledOperation::Keep { filter, .. } => match filter.as_ref().unwrap() {
                CompiledFilter::LangField(CompiledCompareOp::Eq, path) => {
                    assert_eq!(path, &["plugin", "radarr", "original_language"]);
                }
                other => panic!("expected LangField, got {other:?}"),
            },
            other => panic!("expected Keep, got {other:?}"),
        }
    }

    #[test]
    fn test_compile_codec_field_filter() {
        let policy = compile(
            r#"policy "test" {
            phase norm {
                keep audio where codec != plugin.detector.codec
            }
        }"#,
        )
        .unwrap();
        match &policy.phases[0].operations[0] {
            CompiledOperation::Keep { filter, .. } => match filter.as_ref().unwrap() {
                CompiledFilter::CodecField(CompiledCompareOp::Ne, path) => {
                    assert_eq!(path, &["plugin", "detector", "codec"]);
                }
                other => panic!("expected CodecField, got {other:?}"),
            },
            other => panic!("expected Keep, got {other:?}"),
        }
    }

    #[test]
    fn test_compile_english_optimized_policy() {
        let input = include_str!("../../../docs/examples/english-optimized.voom");
        let policy = compile(input).unwrap();
        assert_eq!(policy.name, "english-optimized");
        assert_eq!(policy.phases.len(), 11);
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

    #[test]
    fn test_compile_keep_in_when_block() {
        let policy = compile(
            r#"policy "test" {
            phase validate {
                when exists(audio where lang == jpn) {
                    keep audio where lang in [eng, jpn]
                }
            }
        }"#,
        )
        .unwrap();
        match &policy.phases[0].operations[0] {
            CompiledOperation::Conditional(cond) => {
                assert_eq!(cond.then_actions.len(), 1);
                match &cond.then_actions[0] {
                    CompiledAction::Keep { target, filter } => {
                        assert_eq!(*target, TrackTarget::Audio);
                        assert!(filter.is_some());
                    }
                    other => panic!("expected Keep action, got {other:?}"),
                }
            }
            other => panic!("expected Conditional, got {other:?}"),
        }
    }

    #[test]
    fn test_compile_remove_in_when_block() {
        let policy = compile(
            r#"policy "test" {
            phase validate {
                when is_dubbed {
                    remove audio where commentary
                }
            }
        }"#,
        )
        .unwrap();
        match &policy.phases[0].operations[0] {
            CompiledOperation::Conditional(cond) => {
                assert_eq!(cond.then_actions.len(), 1);
                match &cond.then_actions[0] {
                    CompiledAction::Remove { target, filter } => {
                        assert_eq!(*target, TrackTarget::Audio);
                        assert!(matches!(
                            filter.as_ref().unwrap(),
                            CompiledFilter::Commentary
                        ));
                    }
                    other => panic!("expected Remove action, got {other:?}"),
                }
            }
            other => panic!("expected Conditional, got {other:?}"),
        }
    }

    // ---- safe_u32 boundary tests (issue #236, cluster A) ----
    // These tests target the five cargo-mutants survivors on
    // crates/voom-dsl/src/compiler.rs:29. Each value below is chosen so the
    // original predicate `n >= 0.0 && n <= u32::MAX && n.fract() == 0.0`
    // returns a different result than at least one mutant variant.

    #[test]
    fn safe_u32_accepts_zero() {
        // Kills `>= -> <` (0.0 >= 0.0 is true; 0.0 < 0.0 is false)
        // and `== -> !=` (0.0.fract() == 0.0 is true; != is false).
        assert_eq!(safe_u32(0.0), Some(0));
    }

    #[test]
    fn safe_u32_accepts_u32_max() {
        // Kills `<= -> >` at u32::MAX (MAX <= MAX true; MAX > MAX false).
        let n = f64::from(u32::MAX);
        assert_eq!(safe_u32(n), Some(u32::MAX));
    }

    #[test]
    fn safe_u32_rejects_negative() {
        // Kills `&& -> ||` (first conjunction): with ||, -1.0 satisfies the
        // chain via the second branch and returns Some.
        assert_eq!(safe_u32(-1.0), None);
    }

    #[test]
    fn safe_u32_rejects_overflow() {
        // Kills `&& -> ||` (second conjunction): with ||, n.fract() == 0.0
        // alone admits overflow values that should be rejected.
        let n = f64::from(u32::MAX) + 1.0;
        assert_eq!(safe_u32(n), None);
    }

    #[test]
    fn safe_u32_rejects_fractional_in_range() {
        // Reinforces both `&& -> ||` mutants and the `== -> !=` mutant.
        assert_eq!(safe_u32(0.5), None);
        assert_eq!(safe_u32(1.5), None);
    }

    #[test]
    fn safe_u32_typical_value_round_trips() {
        // Sanity guard that protects later refactors of `safe_u32`.
        assert_eq!(safe_u32(42.0), Some(42));
    }

    // ---- parse_error_strategy / parse_default_strategy arms (issue #236, cluster B) ----
    // Each arm needs a test that returns Some(<variant>) for the literal it
    // matches; the cargo-mutants "delete match arm" mutation falls through
    // to the wildcard `_ => None` arm and would fail the assertion.

    #[test]
    fn parse_error_strategy_continue() {
        assert_eq!(
            parse_error_strategy("continue"),
            Some(ErrorStrategy::Continue),
        );
    }

    #[test]
    fn parse_error_strategy_skip() {
        assert_eq!(parse_error_strategy("skip"), Some(ErrorStrategy::Skip));
    }

    #[test]
    fn parse_error_strategy_abort() {
        assert_eq!(parse_error_strategy("abort"), Some(ErrorStrategy::Abort));
    }

    #[test]
    fn parse_error_strategy_quarantine() {
        assert_eq!(
            parse_error_strategy("quarantine"),
            Some(ErrorStrategy::Quarantine),
        );
    }

    #[test]
    fn parse_error_strategy_unknown_returns_none() {
        assert_eq!(parse_error_strategy("nonsense"), None);
    }

    #[test]
    fn compiles_verify_thorough() {
        let policy = compile(
            r#"policy "p" {
            phase v {
                verify thorough
            }
        }"#,
        )
        .unwrap();
        let op = &policy.phases[0].operations[0];
        assert!(matches!(
            op,
            CompiledOperation::Verify {
                mode: voom_domain::verification::VerificationMode::Thorough
            }
        ));
    }

    #[test]
    fn compiles_verify_quick_and_hash() {
        let policy = compile(
            r#"policy "p" {
            phase q { verify quick }
            phase h { verify hash }
        }"#,
        )
        .unwrap();
        let q = &policy.phases[0].operations[0];
        let h = &policy.phases[1].operations[0];
        assert!(matches!(
            q,
            CompiledOperation::Verify {
                mode: voom_domain::verification::VerificationMode::Quick
            }
        ));
        assert!(matches!(
            h,
            CompiledOperation::Verify {
                mode: voom_domain::verification::VerificationMode::Hash
            }
        ));
    }

    #[test]
    fn compiles_on_error_quarantine_in_config() {
        let policy = compile(
            r#"policy "p" {
            config { on_error: quarantine }
            phase v { verify quick }
        }"#,
        )
        .unwrap();
        assert_eq!(policy.config.on_error, ErrorStrategy::Quarantine);
    }

    #[test]
    fn compiles_on_error_quarantine_in_phase() {
        let policy = compile(
            r#"policy "p" {
            phase v {
                on_error: quarantine
                verify quick
            }
        }"#,
        )
        .unwrap();
        assert_eq!(policy.phases[0].on_error, ErrorStrategy::Quarantine);
    }

    #[test]
    fn parse_default_strategy_first_per_language() {
        assert_eq!(
            parse_default_strategy("first_per_language"),
            Some(DefaultStrategy::FirstPerLanguage),
        );
    }

    #[test]
    fn parse_default_strategy_none() {
        assert_eq!(parse_default_strategy("none"), Some(DefaultStrategy::None));
    }

    #[test]
    fn parse_default_strategy_first() {
        assert_eq!(
            parse_default_strategy("first"),
            Some(DefaultStrategy::First)
        );
    }

    #[test]
    fn parse_default_strategy_all() {
        assert_eq!(parse_default_strategy("all"), Some(DefaultStrategy::All));
    }

    #[test]
    fn parse_default_strategy_unknown_returns_none() {
        assert_eq!(parse_default_strategy("nonsense"), None);
    }

    // ---- compile_transcode setting-extraction tests (issue #236, phase 2) ----
    // Each test exercises one match arm in compile_transcode by constructing
    // a single-entry settings vec and asserting the resulting field. The
    // single-entry settings also distinguish the `== to !=` mutant on the
    // `get` closure (line 232): with one element, `find(|(k,_)| k != key)`
    // returns None while the original returns Some.
    fn transcode_with(key: &str, val: Value) -> CompiledTranscodeSettings {
        let settings = vec![(key.to_string(), val)];
        let CompiledOperation::Transcode { settings, .. } =
            compile_transcode("video", "hevc", &settings)
        else {
            unreachable!("compile_transcode always returns Transcode")
        };
        settings
    }

    #[test]
    fn compile_transcode_preserve_list() {
        let s = transcode_with(
            "preserve",
            Value::List(vec![
                Value::String("metadata".into()),
                Value::String("subtitles".into()),
            ]),
        );
        assert_eq!(s.preserve, vec!["metadata", "subtitles"]);
    }

    #[test]
    fn compile_transcode_crf_number() {
        let s = transcode_with("crf", Value::Number(23.0, "23".into()));
        assert_eq!(s.crf, Some(23));
    }

    #[test]
    fn compile_transcode_preset_string() {
        let s = transcode_with("preset", Value::String("fast".into()));
        assert_eq!(s.preset, Some("fast".into()));
    }

    #[test]
    fn compile_transcode_bitrate_string() {
        let s = transcode_with("bitrate", Value::String("8M".into()));
        assert_eq!(s.bitrate, Some("8M".into()));
    }

    #[test]
    fn compile_transcode_vmaf_settings() {
        let settings = vec![
            ("target_vmaf".to_string(), Value::Number(93.0, "93".into())),
            ("min_bitrate".to_string(), Value::String("2M".into())),
            ("max_bitrate".to_string(), Value::String("8M".into())),
            (
                "sample_strategy".to_string(),
                Value::Call {
                    name: "scenes".into(),
                    args: vec![
                        ("count".into(), Value::Number(5.0, "5".into())),
                        ("duration".into(), Value::Number(4.0, "4s".into())),
                    ],
                },
            ),
            (
                "fallback".to_string(),
                Value::Object(vec![
                    ("crf".into(), Value::Number(24.0, "24".into())),
                    ("preset".into(), Value::String("medium".into())),
                ]),
            ),
            (
                "target_vmaf_when content.animation".to_string(),
                Value::Number(88.0, "88".into()),
            ),
        ];
        let CompiledOperation::Transcode { settings, .. } =
            compile_transcode("video", "hevc", &settings)
        else {
            unreachable!("compile_transcode always returns Transcode")
        };

        assert_eq!(settings.target_vmaf, Some(93));
        assert_eq!(settings.min_bitrate.as_deref(), Some("2M"));
        assert_eq!(settings.max_bitrate.as_deref(), Some("8M"));
        assert_eq!(
            settings.sample_strategy,
            Some(SampleStrategy::Scenes {
                count: 5,
                duration: "4s".into()
            })
        );
        let fallback = settings.fallback.unwrap();
        assert_eq!(fallback.crf, 24);
        assert_eq!(fallback.preset, "medium");
        assert_eq!(settings.vmaf_overrides.unwrap()["animation"], 88);
    }

    #[test]
    fn compile_transcode_channels_number() {
        let s = transcode_with("channels", Value::Number(6.0, "6".into()));
        assert_eq!(s.channels, Some(TranscodeChannels::Count(6)));
    }

    #[test]
    fn compile_transcode_channels_named() {
        let s = transcode_with("channels", Value::Ident("stereo".into()));
        assert_eq!(s.channels, Some(TranscodeChannels::Named("stereo".into())));
    }

    #[test]
    fn compile_transcode_crop_auto() {
        let settings = vec![
            ("crop".to_string(), Value::Ident("auto".into())),
            (
                "crop_sample_duration".to_string(),
                Value::Number(30.0, "30".into()),
            ),
            (
                "crop_sample_count".to_string(),
                Value::Number(4.0, "4".into()),
            ),
            (
                "crop_threshold".to_string(),
                Value::Number(18.0, "18".into()),
            ),
            ("crop_minimum".to_string(), Value::Number(6.0, "6".into())),
            (
                "crop_preserve_bottom_pixels".to_string(),
                Value::Number(40.0, "40".into()),
            ),
            (
                "crop_aspect_lock".to_string(),
                Value::List(vec![
                    Value::String("16/9".into()),
                    Value::String("4/3".into()),
                ]),
            ),
        ];
        let CompiledOperation::Transcode { settings, .. } =
            compile_transcode("video", "hevc", &settings)
        else {
            unreachable!("compile_transcode always returns Transcode")
        };
        let crop = settings.crop.expect("crop settings should compile");

        assert_eq!(crop.sample_duration_secs, Some(30));
        assert_eq!(crop.sample_count, Some(4));
        assert_eq!(crop.threshold, Some(18));
        assert_eq!(crop.minimum_crop, Some(6));
        assert_eq!(crop.preserve_bottom_pixels, Some(40));
        assert_eq!(crop.aspect_lock, vec!["16/9", "4/3"]);
    }

    #[test]
    fn compile_transcode_hw_fallback_bool() {
        let s = transcode_with("hw_fallback", Value::Bool(true));
        assert_eq!(s.hw_fallback, Some(true));
    }

    #[test]
    fn compile_transcode_max_resolution_ident() {
        let s = transcode_with("max_resolution", Value::Ident("1080p".into()));
        assert_eq!(s.max_resolution, Some("1080p".into()));
    }

    #[test]
    fn compile_transcode_max_resolution_number_raw() {
        let s = transcode_with("max_resolution", Value::Number(1080.0, "1080".into()));
        assert_eq!(s.max_resolution, Some("1080".into()));
    }

    #[test]
    fn compile_keep_audio_loudness_normalize() {
        let policy = crate::compile_policy(
            r#"policy "loudness" {
                phase audio {
                    keep audio where lang == eng {
                        normalize: ebu_r128 {
                            target_lufs: -23
                            true_peak_db: -1.0
                            lra_max: 18
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let CompiledOperation::NormalizeAudio { settings, .. } = &policy.phases[0].operations[0]
        else {
            panic!("expected NormalizeAudio operation");
        };
        assert_eq!(settings.target_lufs, -23.0);
        assert_eq!(settings.true_peak_db, -1.0);
        assert_eq!(settings.lra_max, Some(18.0));
    }
}
