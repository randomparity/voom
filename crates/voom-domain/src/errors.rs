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

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, VoomError>;
