//! Domain types for media file integrity verification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Verification mode — three classes of integrity check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerificationMode {
    Quick,
    Thorough,
    Hash,
}

impl VerificationMode {
    /// Canonical lowercase string used in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationMode::Quick => "quick",
            VerificationMode::Thorough => "thorough",
            VerificationMode::Hash => "hash",
        }
    }

    /// Parse from the canonical database string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "quick" => Some(VerificationMode::Quick),
            "thorough" => Some(VerificationMode::Thorough),
            "hash" => Some(VerificationMode::Hash),
            _ => None,
        }
    }
}

/// Outcome of a single verification run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerificationOutcome {
    Ok,
    Warning,
    Error,
}

impl VerificationOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationOutcome::Ok => "ok",
            VerificationOutcome::Warning => "warning",
            VerificationOutcome::Error => "error",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(VerificationOutcome::Ok),
            "warning" => Some(VerificationOutcome::Warning),
            "error" => Some(VerificationOutcome::Error),
            _ => None,
        }
    }
}

/// A single persisted verification result.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationRecord {
    pub id: Uuid,
    pub file_id: String,
    pub verified_at: DateTime<Utc>,
    pub mode: VerificationMode,
    pub outcome: VerificationOutcome,
    pub error_count: u32,
    pub warning_count: u32,
    /// Some only when `mode == Hash`.
    pub content_hash: Option<String>,
    /// Free-form JSON details: tool stderr summary, hash mismatch context, etc.
    pub details: Option<String>,
}

/// Filters for querying verifications.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct VerificationFilters {
    pub file_id: Option<String>,
    pub mode: Option<VerificationMode>,
    pub outcome: Option<VerificationOutcome>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
}

/// Aggregate integrity summary across the library.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegritySummary {
    pub total_files: u64,
    pub never_verified: u64,
    pub stale: u64,
    pub with_errors: u64,
    pub with_warnings: u64,
    pub hash_mismatches: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_roundtrip() {
        for m in [
            VerificationMode::Quick,
            VerificationMode::Thorough,
            VerificationMode::Hash,
        ] {
            assert_eq!(VerificationMode::parse(m.as_str()), Some(m));
        }
        assert!(VerificationMode::parse("nonsense").is_none());
    }

    #[test]
    fn outcome_roundtrip() {
        for o in [
            VerificationOutcome::Ok,
            VerificationOutcome::Warning,
            VerificationOutcome::Error,
        ] {
            assert_eq!(VerificationOutcome::parse(o.as_str()), Some(o));
        }
        assert!(VerificationOutcome::parse("nonsense").is_none());
    }

    #[test]
    fn record_serde_roundtrip() {
        let rec = VerificationRecord {
            id: Uuid::nil(),
            file_id: "file-id".into(),
            verified_at: chrono::Utc::now(),
            mode: VerificationMode::Hash,
            outcome: VerificationOutcome::Ok,
            error_count: 0,
            warning_count: 0,
            content_hash: Some("abc".into()),
            details: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: VerificationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }
}
