//! Error types for DSL parsing, validation, and compilation.

use thiserror::Error;

/// A parse error with source location and optional suggestion.
#[derive(Debug, Error)]
pub enum DslError {
    #[error("parse error at line {line}, col {col}: {message}")]
    Parse {
        line: usize,
        col: usize,
        message: String,
        suggestion: Option<String>,
    },

    #[error("AST build error at line {line}, col {col}: {message}")]
    Build {
        line: usize,
        col: usize,
        message: String,
    },

    #[error("validation error at line {line}, col {col}: {message}")]
    Validation {
        line: usize,
        col: usize,
        message: String,
        suggestion: Option<String>,
    },

    #[error("compilation error: {message}")]
    Compile { message: String },
}

impl DslError {
    pub fn parse(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            line,
            col,
            message: message.into(),
            suggestion: None,
        }
    }

    pub fn parse_with_suggestion(
        line: usize,
        col: usize,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self::Parse {
            line,
            col,
            message: message.into(),
            suggestion: Some(suggestion.into()),
        }
    }

    pub fn build(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::Build {
            line,
            col,
            message: message.into(),
        }
    }

    pub fn validation(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::Validation {
            line,
            col,
            message: message.into(),
            suggestion: None,
        }
    }

    pub fn validation_with_suggestion(
        line: usize,
        col: usize,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self::Validation {
            line,
            col,
            message: message.into(),
            suggestion: Some(suggestion.into()),
        }
    }

    pub fn compile(message: impl Into<String>) -> Self {
        Self::Compile {
            message: message.into(),
        }
    }
}

/// A collection of validation errors.
#[derive(Debug)]
pub struct ValidationErrors {
    pub errors: Vec<DslError>,
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{} validation error(s):", self.errors.len())?;
        for err in &self.errors {
            writeln!(f, "  - {err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

pub type Result<T> = std::result::Result<T, DslError>;

/// A unified error for the full parse → validate → compile pipeline.
///
/// Use this with [`crate::compile_policy`] to run the entire DSL pipeline with a
/// single error type, while still being able to distinguish which stage failed.
///
/// # Example
///
/// ```
/// use voom_dsl::{compile_policy, errors::DslPipelineError};
///
/// match compile_policy("not valid DSL") {
///     Ok(policy) => println!("compiled: {}", policy.name),
///     Err(DslPipelineError::Parse(e)) => eprintln!("parse error: {e}"),
///     Err(DslPipelineError::Validation(e)) => eprintln!("validation errors: {e}"),
///     Err(DslPipelineError::Compile(e)) => eprintln!("compile error: {e}"),
/// }
/// ```
#[derive(Debug)]
pub enum DslPipelineError {
    /// The source could not be parsed into an AST.
    Parse(DslError),
    /// The AST failed semantic validation (may contain multiple errors).
    Validation(ValidationErrors),
    /// The validated AST could not be compiled to a [`CompiledPolicy`](crate::CompiledPolicy).
    Compile(DslError),
}

impl std::fmt::Display for DslPipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DslPipelineError::Parse(e) => write!(f, "parse error: {e}"),
            DslPipelineError::Validation(e) => write!(f, "validation failed: {e}"),
            DslPipelineError::Compile(e) => write!(f, "compile error: {e}"),
        }
    }
}

impl std::error::Error for DslPipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DslPipelineError::Parse(e) => Some(e),
            DslPipelineError::Validation(e) => Some(e),
            DslPipelineError::Compile(e) => Some(e),
        }
    }
}

impl From<DslError> for DslPipelineError {
    /// Converts a [`DslError`] into the appropriate pipeline variant.
    ///
    /// `DslError::Parse` and `DslError::Build` map to `DslPipelineError::Parse`;
    /// `DslError::Validation` maps to `DslPipelineError::Validation` (wrapping a
    /// single-element [`ValidationErrors`]); `DslError::Compile` maps to
    /// `DslPipelineError::Compile`.
    fn from(e: DslError) -> Self {
        match e {
            DslError::Parse { .. } | DslError::Build { .. } => DslPipelineError::Parse(e),
            DslError::Validation { .. } => {
                DslPipelineError::Validation(ValidationErrors { errors: vec![e] })
            }
            DslError::Compile { .. } => DslPipelineError::Compile(e),
        }
    }
}

impl From<ValidationErrors> for DslPipelineError {
    fn from(e: ValidationErrors) -> Self {
        DslPipelineError::Validation(e)
    }
}
