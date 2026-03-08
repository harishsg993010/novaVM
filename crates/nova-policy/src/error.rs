//! Error types for the nova-policy engine.

/// Errors that can occur in the policy engine.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// Failed to load or compile a Wasm policy module.
    #[error("wasm error: {0}")]
    Wasm(String),

    /// Policy evaluation error.
    #[error("evaluation error: {0}")]
    Evaluation(String),

    /// Policy bundle error.
    #[error("bundle error: {0}")]
    Bundle(String),

    /// JSON error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, PolicyError>;
