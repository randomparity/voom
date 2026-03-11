use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("plugin error: {plugin}: {message}")]
    Plugin { plugin: String, message: String },

    #[error("event dispatch error: {0}")]
    EventDispatch(String),

    #[error("capability not found: {0}")]
    CapabilityNotFound(String),

    #[error("plugin not found: {0}")]
    PluginNotFound(String),

    #[error("manifest error: {0}")]
    Manifest(String),

    #[error("wasm error: {0}")]
    Wasm(String),

    #[error("DSL parse error at {location}: {message}")]
    DslParse { location: String, message: String },

    #[error("DSL validation error: {0}")]
    DslValidation(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("invalid codec: {0}")]
    InvalidCodec(String),

    #[error("invalid language code: {0}")]
    InvalidLanguage(String),

    #[error("plan execution error: {phase}: {message}")]
    PlanExecution { phase: String, message: String },

    #[error("tool not found: {tool}")]
    ToolNotFound { tool: String },

    #[error("tool execution error: {tool}: {message}")]
    ToolExecution { tool: String, message: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, VoomError>;
