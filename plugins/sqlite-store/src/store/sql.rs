use chrono::{DateTime, Utc};
use uuid::Uuid;

use voom_domain::errors::{Result, StorageErrorKind, VoomError};

/// Classify a rusqlite error into a [`StorageErrorKind`].
fn classify_rusqlite(e: &rusqlite::Error) -> StorageErrorKind {
    match e {
        rusqlite::Error::SqliteFailure(ffi_err, _) => {
            use rusqlite::ffi::ErrorCode;
            match ffi_err.code {
                ErrorCode::ConstraintViolation => StorageErrorKind::ConstraintViolation,
                _ => StorageErrorKind::Other,
            }
        }
        rusqlite::Error::QueryReturnedNoRows => StorageErrorKind::NotFound,
        _ => StorageErrorKind::Other,
    }
}

/// Create a `.map_err` closure for `rusqlite::Error` that classifies the error kind.
pub(crate) fn storage_err(msg: &str) -> impl FnOnce(rusqlite::Error) -> VoomError + '_ {
    move |e| VoomError::Storage {
        kind: classify_rusqlite(&e),
        message: format!("{msg}: {e}"),
    }
}

/// Wrap any displayable error as a generic storage error with [`StorageErrorKind::Other`].
pub(crate) fn other_storage_err<E: std::fmt::Display>(
    msg: &str,
) -> impl FnOnce(E) -> VoomError + '_ {
    move |e| VoomError::Storage {
        kind: StorageErrorKind::Other,
        message: format!("{msg}: {e}"),
    }
}

/// Lightweight builder for dynamic SQL queries with positional parameters.
pub(crate) struct SqlQuery {
    pub(crate) sql: String,
    pub(crate) params: Vec<String>,
}

impl SqlQuery {
    pub(crate) fn new(base: &str) -> Self {
        Self {
            sql: base.to_string(),
            params: Vec::new(),
        }
    }

    /// Append a parameterized SQL fragment. Returns `&mut Self` for chaining.
    pub(crate) fn parameterized_clause(&mut self, clause: &str, value: String) -> &mut Self {
        self.params.push(value);
        self.sql
            .push_str(&clause.replace("{}", &format!("?{}", self.params.len())));
        self
    }

    /// Append LIMIT and OFFSET clauses with clamped values.
    pub(crate) fn paginate(&mut self, limit: Option<u32>, offset: Option<u32>) {
        if let Some(limit) = limit {
            self.parameterized_clause(" LIMIT {}", limit.min(10_000).to_string());
        }
        if let Some(offset) = offset {
            self.parameterized_clause(" OFFSET {}", offset.min(1_000_000).to_string());
        }
    }

    /// Build the parameter references for rusqlite.
    pub(crate) fn param_refs(&self) -> Vec<&dyn rusqlite::types::ToSql> {
        self.params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect()
    }
}

pub(crate) fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(other_storage_err(&format!("invalid UUID '{s}'")))
}

/// Escape LIKE wildcard characters so user-supplied strings match literally.
pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .map_err(other_storage_err(&format!("invalid datetime '{s}'")))
}

pub(crate) fn format_datetime(dt: &DateTime<Utc>) -> String {
    voom_domain::utils::format::format_iso(dt)
}

/// Extension trait for `rusqlite::Result<T>` to convert to `Option<T>`.
pub(crate) trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
