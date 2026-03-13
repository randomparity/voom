//! AST types for the VOOM DSL.
//!
//! These types represent the parsed structure of a `.voom` policy file.
//! The parser converts pest's CST (concrete syntax tree) into these typed AST nodes.

use serde::Serialize;

/// Source location span for error reporting.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl Span {
    #[must_use] 
    pub fn new(start: usize, end: usize, line: usize, col: usize) -> Self {
        Self {
            start,
            end,
            line,
            col,
        }
    }
}

/// Root AST node representing an entire policy file.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyAst {
    pub name: String,
    pub config: Option<ConfigNode>,
    pub phases: Vec<PhaseNode>,
    pub span: Span,
}

/// Configuration block at the top of a policy.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigNode {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub on_error: Option<String>,
    pub commentary_patterns: Vec<String>,
}

/// A single phase within a policy.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseNode {
    pub name: String,
    pub skip_when: Option<ConditionNode>,
    pub depends_on: Vec<String>,
    pub run_if: Option<RunIfNode>,
    pub on_error: Option<String>,
    pub operations: Vec<SpannedOperation>,
    pub span: Span,
}

/// Phase dependency trigger.
#[derive(Debug, Clone, Serialize)]
pub struct RunIfNode {
    pub phase: String,
    pub trigger: String, // "modified" or "completed"
}

/// An operation wrapped with its source span for precise error reporting.
#[derive(Debug, Clone, Serialize)]
pub struct SpannedOperation {
    pub node: OperationNode,
    pub span: Span,
}

/// An operation within a phase.
#[derive(Debug, Clone, Serialize)]
pub enum OperationNode {
    Container(String),
    Keep {
        target: String,
        filter: Option<FilterNode>,
    },
    Remove {
        target: String,
        filter: Option<FilterNode>,
    },
    Order(Vec<String>),
    Defaults(Vec<(String, String)>),
    Actions {
        target: String,
        settings: Vec<(String, Value)>,
    },
    Transcode {
        target: String,
        codec: String,
        settings: Vec<(String, Value)>,
    },
    Synthesize {
        name: String,
        settings: Vec<SynthSetting>,
    },
    When(WhenNode),
    Rules {
        mode: String,
        rules: Vec<RuleNode>,
    },
}

/// A synthesize setting.
#[derive(Debug, Clone, Serialize)]
pub enum SynthSetting {
    Codec(String),
    Channels(Value),
    Source(FilterNode),
    Bitrate(String),
    SkipIfExists(FilterNode),
    CreateIf(ConditionNode),
    Title(String),
    Language(String),
    Position(Value),
}

/// A when/else conditional block.
#[derive(Debug, Clone, Serialize)]
pub struct WhenNode {
    pub condition: ConditionNode,
    pub then_actions: Vec<ActionNode>,
    pub else_actions: Vec<ActionNode>,
}

/// A named rule within a rules block.
#[derive(Debug, Clone, Serialize)]
pub struct RuleNode {
    pub name: String,
    pub when: WhenNode,
}

/// Condition expressions used in `when` and `skip when`.
#[derive(Debug, Clone, Serialize)]
pub enum ConditionNode {
    Exists(TrackQueryNode),
    Count(TrackQueryNode, CompareOp, f64),
    FieldCompare(Vec<String>, CompareOp, Value),
    FieldExists(Vec<String>),
    AudioIsMultiLanguage,
    IsDubbed,
    IsOriginal,
    And(Vec<ConditionNode>),
    Or(Vec<ConditionNode>),
    Not(Box<ConditionNode>),
}

/// Track query used in `exists()/count()` conditions.
#[derive(Debug, Clone, Serialize)]
pub struct TrackQueryNode {
    pub target: String,
    pub filter: Option<FilterNode>,
}

/// Filter expressions used in `where` clauses.
#[derive(Debug, Clone, Serialize)]
pub enum FilterNode {
    LangIn(Vec<String>),
    LangCompare(CompareOp, String),
    CodecIn(Vec<String>),
    CodecCompare(CompareOp, String),
    Channels(CompareOp, f64),
    Commentary,
    Forced,
    Default,
    Font,
    TitleContains(String),
    TitleMatches(String),
    And(Vec<FilterNode>),
    Or(Vec<FilterNode>),
    Not(Box<FilterNode>),
}

/// Track reference used in actions like `set_default`.
#[derive(Debug, Clone, Serialize)]
pub struct TrackRefNode {
    pub target: String,
    pub filter: Option<FilterNode>,
}

/// An action within a when/else block.
#[derive(Debug, Clone, Serialize)]
pub enum ActionNode {
    Skip(Option<String>),
    Warn(String),
    Fail(String),
    SetDefault(TrackRefNode),
    SetForced(TrackRefNode),
    SetLanguage(TrackRefNode, ValueOrField),
    SetTag(String, ValueOrField),
}

/// Either a literal value or a field access path.
#[derive(Debug, Clone, Serialize)]
pub enum ValueOrField {
    Value(Value),
    Field(Vec<String>),
}

/// Comparison operators.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

/// A DSL value: string, number, boolean, identifier, or list.
#[derive(Debug, Clone, Serialize)]
pub enum Value {
    String(String),
    Number(f64, String), // parsed value + original text (e.g. "192k")
    Bool(bool),
    Ident(String),
    List(Vec<Value>),
}
