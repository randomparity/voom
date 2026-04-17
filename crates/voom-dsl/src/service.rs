//! High-level policy service API.
//!
//! Provides opaque result types for policy validation and formatting so that
//! downstream consumers (e.g. web servers) do not need to depend on the DSL's
//! internal AST, error types, or parser API.

use serde::Serialize;

use crate::errors::DslError;
use crate::{format_policy, parse_policy, validate};

/// Location information for a policy error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ErrorInfo {
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl ErrorInfo {
    /// Create a new `ErrorInfo`.
    pub fn new(message: impl Into<String>, line: Option<usize>, column: Option<usize>) -> Self {
        Self {
            message: message.into(),
            line,
            column,
            suggestion: None,
        }
    }

    /// Create a new `ErrorInfo` with a suggestion.
    pub fn with_suggestion(
        message: impl Into<String>,
        line: Option<usize>,
        column: Option<usize>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            line,
            column,
            suggestion: Some(suggestion.into()),
        }
    }
}

/// Result of validating a policy source string.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ErrorInfo>,
}

/// Result of formatting a policy source string.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FormatResult {
    pub formatted: Option<String>,
    pub errors: Vec<ErrorInfo>,
}

fn error_info_from_dsl(err: &DslError) -> ErrorInfo {
    match err {
        DslError::Parse {
            line,
            col,
            message,
            suggestion,
        } => ErrorInfo {
            message: format!("parse error at line {line}, col {col}: {message}"),
            line: Some(*line),
            column: Some(*col),
            suggestion: suggestion.clone(),
        },
        DslError::Build { line, col, message } => ErrorInfo {
            message: format!("AST build error at line {line}, col {col}: {message}"),
            line: Some(*line),
            column: Some(*col),
            suggestion: None,
        },
        DslError::Validation {
            line,
            col,
            message,
            suggestion,
        } => ErrorInfo {
            message: format!("validation error at line {line}, col {col}: {message}"),
            line: Some(*line),
            column: Some(*col),
            suggestion: suggestion.clone(),
        },
        DslError::Compile { message } => ErrorInfo {
            message: format!("compilation error: {message}"),
            line: None,
            column: None,
            suggestion: None,
        },
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
#[must_use]
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

    #[test]
    fn error_info_from_dsl_extracts_suggestion() {
        let err = DslError::validation_with_suggestion(
            3,
            10,
            "unknown codec \"h265\"",
            "did you mean \"hevc\"?",
        );
        let info = error_info_from_dsl(&err);
        assert_eq!(
            info.message,
            "validation error at line 3, col 10: unknown codec \"h265\""
        );
        assert!(!info.message.contains("suggestion"));
        assert_eq!(info.suggestion.as_deref(), Some("did you mean \"hevc\"?"));
    }

    #[test]
    fn error_info_from_dsl_no_suggestion() {
        let err = DslError::validation(5, 1, "duplicate phase name");
        let info = error_info_from_dsl(&err);
        assert_eq!(
            info.message,
            "validation error at line 5, col 1: duplicate phase name"
        );
        assert!(info.suggestion.is_none());
    }

    #[test]
    fn error_info_serialization_omits_none_suggestion() {
        let info = ErrorInfo::new("bad syntax", Some(1), Some(5));
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("suggestion").is_none());
    }

    #[test]
    fn error_info_serialization_includes_suggestion() {
        let info = ErrorInfo {
            message: "unknown codec".into(),
            line: Some(2),
            column: Some(3),
            suggestion: Some("did you mean \"hevc\"?".into()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["suggestion"], "did you mean \"hevc\"?");
    }
}
