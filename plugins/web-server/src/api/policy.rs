//! Policy-related API endpoints (validate, format).

use axum::Json;
use serde::{Deserialize, Serialize};

use voom_dsl::service;

use crate::errors::WebError;

/// Maximum policy source size (1 MiB).
const MAX_POLICY_SIZE: usize = 1_024 * 1_024;

#[non_exhaustive]
#[derive(Debug, Deserialize)]
pub struct PolicyInput {
    pub source: String,
}

#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub valid: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<service::ErrorInfo>,
}

#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct FormatResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatted: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<service::ErrorInfo>,
}

/// POST /api/policy/validate -- validate DSL source
#[tracing::instrument(skip(input))]
pub async fn validate_policy(
    Json(input): Json<PolicyInput>,
) -> Result<Json<ValidateResponse>, WebError> {
    if input.source.len() > MAX_POLICY_SIZE {
        return Err(WebError::BadRequest(format!(
            "Policy source exceeds maximum size of {MAX_POLICY_SIZE} bytes"
        )));
    }

    let result = service::validate_source(&input.source);
    Ok(Json(ValidateResponse {
        valid: result.valid,
        errors: result.errors,
    }))
}

/// POST /api/policy/format -- format DSL source
#[tracing::instrument(skip(input))]
pub async fn format_policy(
    Json(input): Json<PolicyInput>,
) -> Result<Json<FormatResponse>, WebError> {
    if input.source.len() > MAX_POLICY_SIZE {
        return Err(WebError::BadRequest(format!(
            "Policy source exceeds maximum size of {MAX_POLICY_SIZE} bytes"
        )));
    }

    let result = service::format_source(&input.source);
    Ok(Json(FormatResponse {
        formatted: result.formatted,
        errors: result.errors,
    }))
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
    async fn test_validate_valid_policy_returns_valid_true() {
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
    async fn test_validate_invalid_policy_returns_errors() {
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
    async fn test_validate_oversized_policy_returns_bad_request() {
        let input = PolicyInput {
            source: "x".repeat(MAX_POLICY_SIZE + 1),
        };
        let result = validate_policy(Json(input)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, WebError::BadRequest(_)));
    }

    #[tokio::test]
    async fn test_format_valid_policy_returns_formatted() {
        let input = PolicyInput {
            source: VALID_POLICY.to_string(),
        };
        let result = format_policy(Json(input)).await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        let formatted = response.formatted.expect("formatted should be Some");
        assert!(!formatted.is_empty());
        assert!(formatted.contains("policy"));
    }

    #[tokio::test]
    async fn test_format_invalid_policy_returns_errors_in_body() {
        let input = PolicyInput {
            source: "not valid".to_string(),
        };
        let result = format_policy(Json(input)).await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        assert!(response.formatted.is_none());
        assert!(!response.errors.is_empty());
    }

    #[tokio::test]
    async fn test_format_oversized_policy_returns_bad_request() {
        let input = PolicyInput {
            source: "x".repeat(MAX_POLICY_SIZE + 1),
        };
        let result = format_policy(Json(input)).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_policy_input_deserialize() {
        let input: PolicyInput = serde_json::from_str(r#"{"source":"policy \"x\" {}"}"#).unwrap();
        assert_eq!(input.source, r#"policy "x" {}"#);
    }

    #[test]
    fn test_validate_response_serialization_valid() {
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
    fn test_validate_response_serialization_with_errors() {
        let response = ValidateResponse {
            valid: false,
            errors: vec![service::ErrorInfo::new("bad syntax", Some(1), Some(5))],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["valid"], false);
        assert_eq!(json["errors"][0]["message"], "bad syntax");
        assert_eq!(json["errors"][0]["line"], 1);
        assert_eq!(json["errors"][0]["column"], 5);
    }

    #[test]
    fn test_format_response_serialization() {
        let response = FormatResponse {
            formatted: Some("policy \"x\" {}".into()),
            errors: vec![],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["formatted"], "policy \"x\" {}");
    }

    #[test]
    fn test_max_policy_size_is_1mib() {
        assert_eq!(MAX_POLICY_SIZE, 1_024 * 1_024);
    }

    #[test]
    fn test_validate_response_serialization_with_suggestion() {
        let response = ValidateResponse {
            valid: false,
            errors: vec![service::ErrorInfo::with_suggestion(
                "unknown codec \"h265\"",
                Some(3),
                Some(10),
                "did you mean \"hevc\"?",
            )],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["errors"][0]["suggestion"], "did you mean \"hevc\"?");
        assert!(!json["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("suggestion"));
    }

    #[test]
    fn test_validate_response_serialization_omits_none_suggestion() {
        let response = ValidateResponse {
            valid: false,
            errors: vec![service::ErrorInfo::new("bad syntax", Some(1), Some(5))],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert!(json["errors"][0].get("suggestion").is_none());
    }
}
