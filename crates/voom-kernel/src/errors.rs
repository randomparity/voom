//! Typed error types for the VOOM kernel.

/// Errors that can occur when loading a WASM plugin.
#[derive(Debug, thiserror::Error)]
pub enum WasmLoadError {
    /// Failed to read the `.wasm` file from disk.
    #[error("failed to read WASM file '{path}': {source}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The WASM module exceeds the configured size limit.
    #[error("WASM module '{path}' exceeds size limit ({size} bytes, max {max})")]
    FileTooLarge {
        path: String,
        size: usize,
        max: usize,
    },

    /// Failed to compile the WASM component.
    #[error("failed to compile WASM component '{path}': {message}")]
    ComponentCompilation { path: String, message: String },

    /// Failed to configure the wasmtime engine.
    #[error("failed to create WASM engine: {0}")]
    EngineCreation(String),

    /// Failed to set up the component linker or register host functions.
    #[error("linker error: {0}")]
    Linker(String),

    /// Failed to instantiate the WASM component.
    #[error("failed to instantiate WASM component: {0}")]
    Instantiation(String),

    /// An error occurred while calling into a WASM component.
    #[error("WASM component call failed: {0}")]
    ComponentCall(String),

    /// The WASM component returned an unexpected value type.
    #[error("unexpected WASM value: {0}")]
    UnexpectedValue(String),

    /// Failed to read the manifest `.toml` file.
    #[error("failed to read manifest '{path}': {source}")]
    ManifestRead {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse the manifest TOML.
    #[error("failed to parse manifest '{path}': {message}")]
    ManifestParse { path: String, message: String },

    /// The manifest failed semantic validation.
    #[error("invalid manifest '{path}': {message}")]
    ManifestInvalid { path: String, message: String },

    /// The manifest file is world-writable and was rejected for security.
    #[error("WASM plugin manifest '{path}' is world-writable (mode {mode:o}), refusing to load")]
    ManifestWorldWritable { path: String, mode: u32 },
}
