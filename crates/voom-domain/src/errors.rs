use thiserror::Error;

/// Classifies the kind of storage failure without exposing rusqlite internals.
#[derive(Debug, Clone, PartialEq)]
pub enum StorageErrorKind {
    /// A UNIQUE or FOREIGN KEY constraint was violated.
    ConstraintViolation,
    /// The requested record does not exist.
    NotFound,
    /// Could not acquire or open a database connection.
    ConnectionError,
    /// Any other storage error.
    Other,
}

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("plugin error: {plugin}: {message}")]
    Plugin { plugin: String, message: String },

    #[error("wasm error: {0}")]
    Wasm(String),

    #[error("storage error: {message}")]
    Storage {
        kind: StorageErrorKind,
        message: String,
    },

    #[error("tool not found: {tool}")]
    ToolNotFound { tool: String },

    #[error("tool execution error: {tool}: {message}")]
    ToolExecution { tool: String, message: String },

    #[error("validation error: {0}")]
    Validation(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, VoomError>;
