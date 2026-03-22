//! High-level policy service API.
//!
//! Provides opaque result types for policy validation and formatting so that
//! downstream consumers (e.g. web servers) do not need to depend on the DSL's
//! internal AST, error types, or parser API.

use crate::errors::DslError;
use crate::{format_policy, parse_policy, validate};

/// Location information for a policy error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

/// Result of validating a policy source string.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ErrorInfo>,
}

/// Result of formatting a policy source string.
#[derive(Debug, Clone)]
pub struct FormatResult {
    pub formatted: Option<String>,
    pub errors: Vec<ErrorInfo>,
}

fn error_info_from_dsl(err: &DslError) -> ErrorInfo {
    let (line, column) = match err {
        DslError::Parse { line, col, .. }
        | DslError::Build { line, col, .. }
        | DslError::Validation { line, col, .. } => (Some(*line), Some(*col)),
        DslError::Compile { .. } => (None, None),
    };
    ErrorInfo {
        message: err.to_string(),
        line,
        column,
    }
}

/// Validate a policy source string.
///
/// Returns a [`ValidationResult`] with `valid: true` if the source parses and
/// passes semantic validation, or `valid: false` with error details otherwise.
pub fn validate_source(source: &str) -> ValidationResult {
    match parse_policy(source) {
        Ok(ast) => match validate(&ast) {
            Ok(()) => ValidationResult {
                valid: true,
                errors: vec![],
            },
            Err(validation_errors) => ValidationResult {
                valid: false,
                errors: validation_errors
                    .errors
                    .iter()
                    .map(error_info_from_dsl)
                    .collect(),
            },
        },
        Err(e) => ValidationResult {
            valid: false,
            errors: vec![error_info_from_dsl(&e)],
        },
    }
}

/// Format a policy source string.
///
/// Returns a [`FormatResult`] with the formatted source if parsing succeeds,
/// or error details if the source cannot be parsed.
pub fn format_source(source: &str) -> FormatResult {
    match parse_policy(source) {
        Ok(ast) => FormatResult {
            formatted: Some(format_policy(&ast)),
            errors: vec![],
        },
        Err(e) => FormatResult {
            formatted: None,
            errors: vec![error_info_from_dsl(&e)],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_POLICY: &str = r#"policy "test" {
  phase clean {
    keep audio where codec in [aac, opus]
  }
}"#;

    #[test]
    fn validate_source_valid_policy() {
        let result = validate_source(VALID_POLICY);
        assert!(result.valid);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn validate_source_invalid_syntax() {
        let result = validate_source("not valid DSL");
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].message.contains("expected"));
    }

    #[test]
    fn format_source_valid_policy() {
        let result = format_source(VALID_POLICY);
        assert!(result.formatted.is_some());
        assert!(result.errors.is_empty());
        assert!(result.formatted.unwrap().contains("policy"));
    }

    #[test]
    fn format_source_invalid_syntax() {
        let result = format_source("not valid");
        assert!(result.formatted.is_none());
        assert!(!result.errors.is_empty());
    }
}
