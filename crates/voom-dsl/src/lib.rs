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
pub mod bundled;
pub mod compiled;
pub mod compiler;
mod composition;
pub mod errors;
pub mod formatter;
pub mod parser;
pub mod service;
pub mod validator;

#[cfg(any(test, feature = "proptest"))]
pub mod testing;

pub use ast::{
    ActionNode, CompareOp, ConditionNode, ConfigNode, ErrorStrategyNode, FilterNode, OperationNode,
    PhaseNode, PolicyAst, RuleNode, RunIfNode, RunIfTriggerNode, Span, SpannedOperation,
    SynthSetting, TrackQueryNode, TrackRefNode, Value, ValueOrField, WhenNode,
};
pub use bundled::{bundled_policy, bundled_policy_names};
pub use compiled::{
    CompiledMetadata, CompiledPhaseComposition, CompiledPolicy, PhaseCompositionKind,
};
pub use errors::{DslError, DslPipelineError, DslWarning, ValidationErrors};
pub use formatter::format_policy;
pub use parser::parse_policy;
pub use validator::{validate, validate_with_warnings};

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
    if ast.extends.is_some() {
        return Err(DslPipelineError::Compile(DslError::compile(
            "policy extends requires composition resolution; use compile_policy_with_bundled(source) or compile_policy_file(path)",
        )));
    }
    validate(&ast).map_err(DslPipelineError::Validation)?;
    let mut policy = compiler::compile_ast(&ast).map_err(DslPipelineError::Compile)?;
    policy.source_hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(source.as_bytes()));
    Ok(policy)
}

/// Resolve bundled policy inheritance before validation and compilation.
///
/// Use this for in-memory policy sources that may extend bundled policies.
///
/// # Errors
///
/// Returns [`DslPipelineError::Parse`] if any policy source cannot be parsed,
/// [`DslPipelineError::Validation`] if the merged AST fails semantic validation,
/// or [`DslPipelineError::Compile`] if inheritance cannot be resolved or compiled.
pub fn compile_policy_with_bundled(source: &str) -> Result<CompiledPolicy, DslPipelineError> {
    let resolved = composition::resolve_policy_with_bundled(source)?;
    compile_resolved_policy(resolved, Some(source))
}

fn compile_resolved_policy(
    resolved: composition::ResolvedPolicyAst,
    standalone_source: Option<&str>,
) -> Result<CompiledPolicy, DslPipelineError> {
    validate(&resolved.ast).map_err(DslPipelineError::Validation)?;
    let mut policy =
        compiler::compile_resolved_ast(&resolved).map_err(DslPipelineError::Compile)?;
    let source = if let Some(source) = standalone_source
        && resolved.extends_chain.is_empty()
        && resolved.phase_sources.is_empty()
    {
        std::borrow::Cow::Borrowed(source)
    } else {
        std::borrow::Cow::Owned(format_policy(&resolved.ast))
    };
    policy.source_hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(source.as_bytes()));
    Ok(policy)
}

/// Resolve file-relative policy inheritance before validation and compilation.
///
/// # Errors
///
/// Returns [`DslPipelineError::Parse`] if any policy source cannot be parsed,
/// [`DslPipelineError::Validation`] if the merged AST fails semantic validation,
/// or [`DslPipelineError::Compile`] if inheritance cannot be resolved or compiled.
pub fn compile_policy_file(path: &std::path::Path) -> Result<CompiledPolicy, DslPipelineError> {
    let resolved = composition::resolve_policy_file(path)?;
    let source = if resolved.extends_chain.is_empty() && resolved.phase_sources.is_empty() {
        Some(std::fs::read_to_string(path).map_err(|err| {
            DslPipelineError::Compile(DslError::compile(format!(
                "failed to read policy file {}: {err}",
                path.display()
            )))
        })?)
    } else {
        None
    };
    compile_resolved_policy(resolved, source.as_deref())
}
