//! Error types for the nova-agent.

use std::io;

/// Errors that can occur in the guest agent.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Failed to connect or communicate over vsock.
    #[error("vsock error: {0}")]
    Vsock(String),

    /// Failed to execute a command inside the guest.
    #[error("exec error: {0}")]
    Exec(String),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// System-level error (nix).
    #[error("system error: {0}")]
    #[allow(dead_code)]
    System(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;
