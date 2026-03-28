use thiserror::Error;

/// Classifies the kind of storage failure without exposing rusqlite internals.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StorageErrorKind {
    /// A UNIQUE or FOREIGN KEY constraint was violated.
    ConstraintViolation,
    /// The requested record does not exist.
    NotFound,
    /// Any other storage error.
    Other,
}

#[derive(Debug, Error)]
#[non_exhaustive]
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

impl VoomError {
    /// Create a plugin error with the given plugin name and message.
    pub fn plugin(plugin: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Plugin {
            plugin: plugin.into(),
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, VoomError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voom_error_display_plugin() {
        let err = VoomError::Plugin {
            plugin: "ffprobe".into(),
            message: "not installed".into(),
        };
        assert_eq!(err.to_string(), "plugin error: ffprobe: not installed");
    }

    #[test]
    fn voom_error_display_storage_with_kind() {
        let err = VoomError::Storage {
            kind: StorageErrorKind::NotFound,
            message: "file id 42 missing".into(),
        };
        assert_eq!(err.to_string(), "storage error: file id 42 missing");
        // Verify the kind is accessible and comparable
        if let VoomError::Storage { kind, .. } = &err {
            assert_eq!(*kind, StorageErrorKind::NotFound);
        }
    }

    #[test]
    fn voom_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: VoomError = io_err.into();
        assert!(err.to_string().contains("I/O error"));
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn storage_error_kind_clone_and_eq() {
        let a = StorageErrorKind::ConstraintViolation;
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, StorageErrorKind::Other);
    }
}
