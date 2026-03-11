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

    #[error("unexpected rule {rule} at line {line}, col {col}")]
    UnexpectedRule {
        rule: String,
        line: usize,
        col: usize,
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

    pub fn unexpected_rule(rule: impl Into<String>, line: usize, col: usize) -> Self {
        Self::UnexpectedRule {
            rule: rule.into(),
            line,
            col,
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
