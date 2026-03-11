//! Policy-related API endpoints (validate, format).

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::WebError;

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
    let ast = voom_dsl::parse_policy(&input.source)
        .map_err(|e| WebError::BadRequest(format!("Parse error: {e}")))?;

    let formatted = voom_dsl::format_policy(&ast);
    Ok(Json(FormatResponse { formatted }))
}
