//! Error types for the nova-runtime.

use std::io;

/// Errors that can occur in the runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Sandbox not found.
    #[error("sandbox not found: {0}")]
    SandboxNotFound(String),

    /// Sandbox already exists.
    #[error("sandbox already exists: {0}")]
    SandboxExists(String),

    /// Invalid sandbox state transition.
    #[error("invalid state transition for '{id}': {from} -> {to}")]
    InvalidState {
        id: String,
        from: String,
        to: String,
    },

    /// Image pull or conversion error.
    #[error("image error: {0}")]
    Image(String),

    /// Network configuration error.
    #[error("network error: {0}")]
    Network(String),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Configuration error.
    #[error("config error: {0}")]
    Config(String),

    /// Wasm execution error.
    #[error("wasm error: {0}")]
    Wasm(String),

    /// Cache subsystem error.
    #[error("cache error: {0}")]
    Cache(String),

    /// Snapshot save/restore error.
    #[error("snapshot error: {0}")]
    Snapshot(String),

    /// VM pool error.
    #[error("pool error: {0}")]
    Pool(String),
}

pub type Result<T> = std::result::Result<T, RuntimeError>;
