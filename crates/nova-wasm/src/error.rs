//! Error types for the nova-wasm executor.

/// Errors that can occur in the Wasm executor.
#[derive(Debug, thiserror::Error)]
pub enum WasmError {
    /// Failed to compile a Wasm module.
    #[error("compilation error: {0}")]
    Compilation(String),

    /// Failed to instantiate a Wasm module.
    #[error("instantiation error: {0}")]
    Instantiation(String),

    /// Runtime execution error.
    #[error("execution error: {0}")]
    Execution(String),

    /// WASI configuration error.
    #[error("WASI error: {0}")]
    Wasi(String),

    /// Module cache error.
    #[error("cache error: {0}")]
    Cache(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, WasmError>;
