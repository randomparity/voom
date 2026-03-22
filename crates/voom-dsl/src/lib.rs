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

#![allow(clippy::missing_errors_doc)]

pub mod ast;
pub mod compiler;
pub mod errors;
pub mod formatter;
pub mod parser;
pub mod service;
pub mod validator;

pub use ast::{
    ActionNode, CompareOp, ConditionNode, ConfigNode, FilterNode, OperationNode, PhaseNode,
    PolicyAst, RuleNode, RunIfNode, Span, SpannedOperation, SynthSetting, TrackQueryNode,
    TrackRefNode, Value, ValueOrField, WhenNode,
};
pub use errors::{DslError, DslPipelineError, ValidationErrors};
pub use formatter::format_policy;
pub use parser::parse_policy;
pub use validator::validate;
pub use voom_domain::compiled::CompiledPolicy;

/// Run the full parse → validate → compile pipeline in one call.
///
/// This is a convenience wrapper around [`parse_policy`], [`validate`], and
/// compilation that returns a single [`DslPipelineError`] so
/// callers do not need to handle three different error types.
///
/// # Errors
///
/// Returns [`DslPipelineError::Parse`] if the source cannot be parsed,
/// [`DslPipelineError::Validation`] if semantic validation fails, or
/// [`DslPipelineError::Compile`] if AST-to-domain-type compilation fails.
///
/// # Example
///
/// ```
/// use voom_dsl::compile_policy;
///
/// let policy = compile_policy(r#"policy "example" {
///     phase init {
///         container mkv
///     }
/// }"#).unwrap();
/// assert_eq!(policy.name, "example");
/// ```
pub fn compile_policy(source: &str) -> Result<CompiledPolicy, DslPipelineError> {
    let ast = parse_policy(source).map_err(DslPipelineError::Parse)?;
    validate(&ast).map_err(DslPipelineError::Validation)?;
    let mut policy = compiler::compile_ast(&ast).map_err(DslPipelineError::Compile)?;
    policy.source_hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(source.as_bytes()));
    Ok(policy)
}
