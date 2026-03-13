//! Policy-related API endpoints (validate, format).

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::WebError;

/// Maximum policy source size (1 MiB).
const MAX_POLICY_SIZE: usize = 1_024 * 1_024;

#[derive(Debug, Deserialize)]
pub struct PolicyInput {
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub valid: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<PolicyError>,
}

#[derive(Debug, Serialize)]
pub struct PolicyError {
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct FormatResponse {
    pub formatted: String,
}

/// POST /api/policy/validate -- validate DSL source
pub async fn validate_policy(
    Json(input): Json<PolicyInput>,
) -> Result<Json<ValidateResponse>, WebError> {
    if input.source.len() > MAX_POLICY_SIZE {
        return Err(WebError::BadRequest(format!(
            "Policy source exceeds maximum size of {} bytes",
            MAX_POLICY_SIZE
        )));
    }

    match voom_dsl::parse_policy(&input.source) {
        Ok(ast) => {
            // Run semantic validation
            match voom_dsl::validate(&ast) {
                Ok(()) => Ok(Json(ValidateResponse {
                    valid: true,
                    errors: vec![],
                })),
                Err(validation_errors) => {
                    let errors = validation_errors
                        .errors
                        .iter()
                        .map(|e| PolicyError {
                            message: e.to_string(),
                            line: None,
                            column: None,
                        })
                        .collect();
                    Ok(Json(ValidateResponse {
                        valid: false,
                        errors,
                    }))
                }
            }
        }
        Err(e) => Ok(Json(ValidateResponse {
            valid: false,
            errors: vec![PolicyError {
                message: e.to_string(),
                line: None,
                column: None,
            }],
        })),
    }
}

/// POST /api/policy/format -- format DSL source
pub async fn format_policy(
    Json(input): Json<PolicyInput>,
) -> Result<Json<FormatResponse>, WebError> {
    if input.source.len() > MAX_POLICY_SIZE {
        return Err(WebError::BadRequest(format!(
            "Policy source exceeds maximum size of {} bytes",
            MAX_POLICY_SIZE
        )));
    }

    let ast = voom_dsl::parse_policy(&input.source)
        .map_err(|e| WebError::BadRequest(format!("Parse error: {e}")))?;

    let formatted = voom_dsl::format_policy(&ast);
    Ok(Json(FormatResponse { formatted }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_POLICY: &str = r#"policy "test" {
  phase clean {
    keep audio where codec in [aac, opus]
  }
}"#;

    #[tokio::test]
    async fn validate_valid_policy_returns_valid_true() {
        let input = PolicyInput {
            source: VALID_POLICY.to_string(),
        };
        let result = validate_policy(Json(input)).await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        assert!(response.valid);
        assert!(response.errors.is_empty());
    }

    #[tokio::test]
    async fn validate_invalid_policy_returns_errors() {
        let input = PolicyInput {
            source: "this is not valid DSL".to_string(),
        };
        let result = validate_policy(Json(input)).await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        assert!(!response.valid);
        assert!(!response.errors.is_empty());
    }

    #[tokio::test]
    async fn validate_oversized_policy_returns_bad_request() {
        let input = PolicyInput {
            source: "x".repeat(MAX_POLICY_SIZE + 1),
        };
        let result = validate_policy(Json(input)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, WebError::BadRequest(_)));
    }

    #[tokio::test]
    async fn format_valid_policy_returns_formatted() {
        let input = PolicyInput {
            source: VALID_POLICY.to_string(),
        };
        let result = format_policy(Json(input)).await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        assert!(!response.formatted.is_empty());
        assert!(response.formatted.contains("policy"));
    }

    #[tokio::test]
    async fn format_invalid_policy_returns_error() {
        let input = PolicyInput {
            source: "not valid".to_string(),
        };
        let result = format_policy(Json(input)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, WebError::BadRequest(_)));
    }

    #[tokio::test]
    async fn format_oversized_policy_returns_bad_request() {
        let input = PolicyInput {
            source: "x".repeat(MAX_POLICY_SIZE + 1),
        };
        let result = format_policy(Json(input)).await;
        assert!(result.is_err());
    }

    #[test]
    fn policy_input_deserialize() {
        let input: PolicyInput =
            serde_json::from_str(r#"{"source":"policy \"x\" {}"}"#).unwrap();
        assert_eq!(input.source, r#"policy "x" {}"#);
    }

    #[test]
    fn validate_response_serialization_valid() {
        let response = ValidateResponse {
            valid: true,
            errors: vec![],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["valid"], true);
        // errors should be skipped when empty
        assert!(json.get("errors").is_none());
    }

    #[test]
    fn validate_response_serialization_with_errors() {
        let response = ValidateResponse {
            valid: false,
            errors: vec![PolicyError {
                message: "bad syntax".into(),
                line: Some(1),
                column: Some(5),
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["valid"], false);
        assert_eq!(json["errors"][0]["message"], "bad syntax");
        assert_eq!(json["errors"][0]["line"], 1);
        assert_eq!(json["errors"][0]["column"], 5);
    }

    #[test]
    fn format_response_serialization() {
        let response = FormatResponse {
            formatted: "policy \"x\" {}".into(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["formatted"], "policy \"x\" {}");
    }

    #[test]
    fn max_policy_size_is_1mib() {
        assert_eq!(MAX_POLICY_SIZE, 1_024 * 1_024);
    }
}
