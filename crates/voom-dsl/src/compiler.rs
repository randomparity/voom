//! Compiler: AST → CompiledPolicy.
//!
//! Transforms a validated [`PolicyAst`] into a [`CompiledPolicy`] structure
//! that uses domain types and is ready for evaluation by the policy evaluator plugin.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use voom_domain::utils::codecs;

use crate::ast::*;
use crate::errors::{DslError, ValidationErrors};
use crate::validator;

/// A compiled policy ready for evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledPolicy {
    pub name: String,
    pub config: CompiledConfig,
    pub phases: Vec<CompiledPhase>,
    /// Topologically sorted phase execution order.
    pub phase_order: Vec<String>,
    /// xxHash64 of the policy source text.
    #[serde(default)]
    pub source_hash: String,
}

/// Compiled configuration block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledConfig {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub on_error: ErrorStrategy,
    pub commentary_patterns: Vec<String>,
}

/// Error handling strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorStrategy {
    Continue,
    Abort,
    Skip,
}

/// A compiled phase with resolved references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledPhase {
    pub name: String,
    pub depends_on: Vec<String>,
    pub skip_when: Option<CompiledCondition>,
    pub run_if: Option<CompiledRunIf>,
    pub on_error: ErrorStrategy,
    pub operations: Vec<CompiledOperation>,
}

/// Compiled run_if trigger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledRunIf {
    pub phase: String,
    pub trigger: RunIfTrigger,
}

/// Run-if trigger type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunIfTrigger {
    Modified,
    Completed,
}

/// A compiled operation within a phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledOperation {
    SetContainer(String),
    Keep {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    Remove {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    ReorderTracks(Vec<String>),
    SetDefaults(Vec<CompiledDefault>),
    ClearActions {
        target: TrackTarget,
        settings: HashMap<String, serde_json::Value>,
    },
    Transcode {
        target: TrackTarget,
        codec: String,
        settings: HashMap<String, serde_json::Value>,
    },
    Synthesize(CompiledSynthesize),
    Conditional(CompiledConditional),
    Rules {
        mode: RulesMode,
        rules: Vec<CompiledRule>,
    },
}

/// Track target category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackTarget {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Any,
}

/// Defaults strategy for a track type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledDefault {
    pub target: TrackTarget,
    pub strategy: DefaultStrategy,
}

/// Default track selection strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefaultStrategy {
    FirstPerLanguage,
    None,
    First,
    All,
}

/// Synthesize operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledSynthesize {
    pub name: String,
    pub codec: Option<String>,
    pub channels: Option<serde_json::Value>,
    pub source: Option<CompiledFilter>,
    pub bitrate: Option<String>,
    pub skip_if_exists: Option<CompiledFilter>,
    pub create_if: Option<CompiledCondition>,
    pub title: Option<String>,
    pub language: Option<SynthLanguage>,
    pub position: Option<serde_json::Value>,
}

/// Language setting for synthesize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SynthLanguage {
    Inherit,
    Fixed(String),
}

/// A conditional (when/else) block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledConditional {
    pub condition: CompiledCondition,
    pub then_actions: Vec<CompiledAction>,
    pub else_actions: Vec<CompiledAction>,
}

/// A named rule within a rules block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledRule {
    pub name: String,
    pub conditional: CompiledConditional,
}

/// Rules evaluation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RulesMode {
    First,
    All,
}

/// A compiled condition expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledCondition {
    Exists {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    Count {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
        op: CompiledCompareOp,
        value: f64,
    },
    FieldCompare {
        path: Vec<String>,
        op: CompiledCompareOp,
        value: serde_json::Value,
    },
    FieldExists {
        path: Vec<String>,
    },
    AudioIsMultiLanguage,
    IsDubbed,
    IsOriginal,
    And(Vec<CompiledCondition>),
    Or(Vec<CompiledCondition>),
    Not(Box<CompiledCondition>),
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompiledCompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

/// A compiled filter expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledFilter {
    LangIn(Vec<String>),
    LangCompare(CompiledCompareOp, String),
    CodecIn(Vec<String>),
    CodecCompare(CompiledCompareOp, String),
    Channels(CompiledCompareOp, f64),
    Commentary,
    Forced,
    Default,
    Font,
    TitleContains(String),
    TitleMatches(String),
    And(Vec<CompiledFilter>),
    Or(Vec<CompiledFilter>),
    Not(Box<CompiledFilter>),
}

/// A compiled action within a conditional block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledAction {
    Skip(Option<String>),
    Warn(String),
    Fail(String),
    SetDefault {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    SetForced {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    SetLanguage {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
        value: CompiledValueOrField,
    },
    SetTag {
        tag: String,
        value: CompiledValueOrField,
    },
}

/// Either a literal value or a field access path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledValueOrField {
    Value(serde_json::Value),
    Field(Vec<String>),
}

/// Compile a policy source string into a [`CompiledPolicy`].
///
/// Parses, validates, then compiles. Returns all validation errors at once.
pub fn compile(source: &str) -> std::result::Result<CompiledPolicy, CompileError> {
    let ast = crate::parser::parse_policy(source).map_err(CompileError::Parse)?;
    validator::validate(&ast).map_err(CompileError::Validation)?;
    let mut policy = compile_ast(&ast).map_err(CompileError::Compile)?;
    policy.source_hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(source.as_bytes()));
    Ok(policy)
}

/// Compile a pre-parsed and validated AST into a [`CompiledPolicy`].
pub fn compile_ast(ast: &PolicyAst) -> std::result::Result<CompiledPolicy, DslError> {
    let config = compile_config(ast.config.as_ref());
    let phases: Vec<CompiledPhase> = ast
        .phases
        .iter()
        .map(compile_phase)
        .collect::<std::result::Result<_, _>>()?;
    let phase_order = topological_sort(ast)?;

    Ok(CompiledPolicy {
        name: ast.name.clone(),
        config,
        phases,
        phase_order,
        source_hash: String::new(),
    })
}

/// Errors from the compile pipeline.
#[derive(Debug)]
pub enum CompileError {
    Parse(DslError),
    Validation(ValidationErrors),
    Compile(DslError),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Parse(e) => write!(f, "{e}"),
            CompileError::Validation(e) => write!(f, "{e}"),
            CompileError::Compile(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CompileError {}

fn compile_config(config: Option<&ConfigNode>) -> CompiledConfig {
    match config {
        Some(c) => CompiledConfig {
            audio_languages: c.audio_languages.clone(),
            subtitle_languages: c.subtitle_languages.clone(),
            on_error: parse_error_strategy(c.on_error.as_deref()),
            commentary_patterns: c.commentary_patterns.clone(),
        },
        None => CompiledConfig {
            audio_languages: vec![],
            subtitle_languages: vec![],
            on_error: ErrorStrategy::Abort,
            commentary_patterns: vec![],
        },
    }
}

fn parse_error_strategy(value: Option<&str>) -> ErrorStrategy {
    match value {
        Some("continue") => ErrorStrategy::Continue,
        Some("skip") => ErrorStrategy::Skip,
        _ => ErrorStrategy::Abort,
    }
}

fn compile_phase(phase: &PhaseNode) -> std::result::Result<CompiledPhase, DslError> {
    let skip_when = phase
        .skip_when
        .as_ref()
        .map(compile_condition)
        .transpose()?;

    let run_if = phase.run_if.as_ref().map(|r| CompiledRunIf {
        phase: r.phase.clone(),
        trigger: match r.trigger.as_str() {
            "modified" => RunIfTrigger::Modified,
            _ => RunIfTrigger::Completed,
        },
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
        on_error: parse_error_strategy(phase.on_error.as_deref()),
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
                .map(|(kind, value)| CompiledDefault {
                    target: parse_track_target(kind),
                    strategy: match value.as_str() {
                        "first_per_language" => DefaultStrategy::FirstPerLanguage,
                        "none" => DefaultStrategy::None,
                        "first" => DefaultStrategy::First,
                        "all" => DefaultStrategy::All,
                        _ => DefaultStrategy::None,
                    },
                })
                .collect();
            Ok(CompiledOperation::SetDefaults(defaults))
        }
        OperationNode::Actions { target, settings } => {
            let map = settings
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            Ok(CompiledOperation::ClearActions {
                target: parse_track_target(target),
                settings: map,
            })
        }
        OperationNode::Transcode {
            target,
            codec,
            settings,
        } => {
            let canonical = codecs::normalize_codec(codec)
                .map(|s| s.to_string())
                .unwrap_or_else(|| codec.clone());
            let map = settings
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            Ok(CompiledOperation::Transcode {
                target: parse_track_target(target),
                codec: canonical,
                settings: map,
            })
        }
        OperationNode::Synthesize { name, settings } => Ok(CompiledOperation::Synthesize(
            compile_synthesize(name, settings)?,
        )),
        OperationNode::When(when) => Ok(CompiledOperation::Conditional(compile_conditional(when)?)),
        OperationNode::Rules { mode, rules } => {
            let compiled_rules: Vec<CompiledRule> = rules
                .iter()
                .map(|r| {
                    Ok(CompiledRule {
                        name: r.name.clone(),
                        conditional: compile_conditional(&r.when)?,
                    })
                })
                .collect::<std::result::Result<_, DslError>>()?;
            Ok(CompiledOperation::Rules {
                mode: match mode.as_str() {
                    "first" => RulesMode::First,
                    _ => RulesMode::All,
                },
                rules: compiled_rules,
            })
        }
    }
}

fn compile_synthesize(
    name: &str,
    settings: &[SynthSetting],
) -> std::result::Result<CompiledSynthesize, DslError> {
    let mut synth = CompiledSynthesize {
        name: name.to_string(),
        codec: None,
        channels: None,
        source: None,
        bitrate: None,
        skip_if_exists: None,
        create_if: None,
        title: None,
        language: None,
        position: None,
    };

    for setting in settings {
        match setting {
            SynthSetting::Codec(c) => {
                synth.codec = Some(
                    codecs::normalize_codec(c)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| c.clone()),
                );
            }
            SynthSetting::Channels(v) => synth.channels = Some(value_to_json(v)),
            SynthSetting::Source(f) => synth.source = Some(compile_filter(f)?),
            SynthSetting::Bitrate(b) => synth.bitrate = Some(b.clone()),
            SynthSetting::SkipIfExists(f) => synth.skip_if_exists = Some(compile_filter(f)?),
            SynthSetting::CreateIf(c) => synth.create_if = Some(compile_condition(c)?),
            SynthSetting::Title(t) => synth.title = Some(t.clone()),
            SynthSetting::Language(l) => {
                synth.language = Some(if l == "inherit" {
                    SynthLanguage::Inherit
                } else {
                    SynthLanguage::Fixed(l.clone())
                });
            }
            SynthSetting::Position(v) => synth.position = Some(value_to_json(v)),
        }
    }

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

    Ok(CompiledConditional {
        condition,
        then_actions,
        else_actions,
    })
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
        FilterNode::TitleMatches(s) => Ok(CompiledFilter::TitleMatches(s.clone())),
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
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Number(n, _) => serde_json::json!(n),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Ident(s) => serde_json::Value::String(s.clone()),
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
}
