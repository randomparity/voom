//! AST types for the VOOM DSL.
//!
//! These types represent the parsed structure of a `.voom` policy file.
//! The parser converts pest's CST (concrete syntax tree) into these typed AST nodes.

use serde::Serialize;
use std::fmt;

/// Source location span for error reporting.
#[non_exhaustive]
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
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct PolicyAst {
    pub name: String,
    pub extends: Option<ExtendsSource>,
    pub metadata: Option<MetadataNode>,
    pub config: Option<ConfigNode>,
    pub phases: Vec<PhaseNode>,
    pub span: Span,
}

/// Source of a parent policy extended by this policy.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum ExtendsSource {
    Bundled(String),
    File(String),
}

/// Policy metadata block.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct MetadataNode {
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub requires_voom: Option<String>,
    pub requires_tools: Option<Vec<String>>,
    pub test_fixtures: Option<Vec<String>>,
    pub span: Span,
}

/// Configuration block at the top of a policy.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct ConfigNode {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub on_error: Option<ErrorStrategyNode>,
    pub commentary_patterns: Vec<String>,
    pub keep_backups: Option<bool>,
    pub span: Span,
}

/// A single phase within a policy.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct PhaseNode {
    pub name: String,
    pub extend: bool,
    pub skip_when: Option<ConditionNode>,
    pub depends_on: Vec<String>,
    pub run_if: Option<RunIfNode>,
    pub on_error: Option<ErrorStrategyNode>,
    pub operations: Vec<SpannedOperation>,
    pub span: Span,
}

/// AST-level `on_error` strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorStrategyNode {
    Continue,
    Skip,
    Abort,
    Quarantine,
}

impl ErrorStrategyNode {
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "continue" => Some(Self::Continue),
            "skip" => Some(Self::Skip),
            "abort" => Some(Self::Abort),
            "quarantine" => Some(Self::Quarantine),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Skip => "skip",
            Self::Abort => "abort",
            Self::Quarantine => "quarantine",
        }
    }
}

impl fmt::Display for ErrorStrategyNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// AST-level `run_if` trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunIfTriggerNode {
    Modified,
    Completed,
}

impl RunIfTriggerNode {
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "modified" => Some(Self::Modified),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Completed => "completed",
        }
    }
}

impl fmt::Display for RunIfTriggerNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Phase dependency trigger.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct RunIfNode {
    pub phase: String,
    pub trigger: RunIfTriggerNode,
}

/// An operation wrapped with its source span for precise error reporting.
#[non_exhaustive]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        normalize: Option<NormalizeSetting>,
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
    ClearTags,
    SetTag {
        tag: String,
        value: ValueOrField,
    },
    DeleteTag(String),
    When(WhenNode),
    Rules {
        mode: String,
        rules: Vec<RuleNode>,
    },
    /// A `verify <mode>` operation.
    Verify {
        mode: VerifyMode,
    },
}

/// Verification mode for a `verify` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VerifyMode {
    Quick,
    Thorough,
    Hash,
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
    Normalize(NormalizeSetting),
}

/// Audio loudness normalization setting.
#[derive(Debug, Clone, Serialize)]
pub struct NormalizeSetting {
    pub preset: String,
    pub settings: Vec<(String, Value)>,
}

/// A when/else conditional block.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct WhenNode {
    pub condition: ConditionNode,
    pub then_actions: Vec<ActionNode>,
    pub else_actions: Vec<ActionNode>,
    /// Source location of the `when` keyword for error attribution.
    pub span: Span,
}

/// A named rule within a rules block.
#[non_exhaustive]
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
#[non_exhaustive]
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
    LangField(CompareOp, Vec<String>),
    CodecIn(Vec<String>),
    CodecCompare(CompareOp, String),
    CodecField(CompareOp, Vec<String>),
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
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct TrackRefNode {
    pub target: String,
    pub filter: Option<FilterNode>,
}

/// An action within a when/else block.
#[derive(Debug, Clone, Serialize)]
pub enum ActionNode {
    Keep {
        target: String,
        filter: Option<FilterNode>,
    },
    Remove {
        target: String,
        filter: Option<FilterNode>,
    },
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
    Object(Vec<(String, Value)>),
    Call {
        name: String,
        args: Vec<(String, Value)>,
    },
}
