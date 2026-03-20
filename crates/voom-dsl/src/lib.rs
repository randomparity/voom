//! VOOM DSL parser, validator, compiler, and formatter.
//!
//! Parses `.voom` policy files into a typed AST using a PEG grammar (pest),
//! validates semantic correctness, compiles to domain types, and provides
//! a pretty-printer for round-trip formatting.
//!
//! # Example
//!
//! ```
//! use voom_dsl::parse_policy;
//!
//! let input = r#"policy "example" {
//!     phase init {
//!         container mkv
//!     }
//! }"#;
//!
//! let ast = parse_policy(input).unwrap();
//! assert_eq!(ast.name, "example");
//! ```

pub mod ast;
pub mod compiler;
pub mod errors;
pub mod formatter;
pub mod parser;
pub mod validator;

pub use ast::{
    ActionNode, CompareOp, ConditionNode, ConfigNode, FilterNode, OperationNode, PhaseNode,
    PolicyAst, RuleNode, RunIfNode, Span, SpannedOperation, SynthSetting, TrackQueryNode,
    TrackRefNode, Value, ValueOrField, WhenNode,
};
pub use compiler::{compile, CompileError, CompiledPolicy};
pub use errors::{DslError, ValidationErrors};
pub use formatter::format_policy;
pub use parser::parse_policy;
pub use validator::validate;
