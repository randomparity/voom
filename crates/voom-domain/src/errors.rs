use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("plugin error: {plugin}: {message}")]
    Plugin { plugin: String, message: String },

    #[error("wasm error: {0}")]
    Wasm(String),

    #[error("storage error: {0}")]
    Storage(String),

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
