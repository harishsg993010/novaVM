use std::io;

/// Errors that can occur in the nova-eye eBPF sensor subsystem.
#[derive(Debug, thiserror::Error)]
pub enum EyeError {
    /// Failed to load an eBPF program.
    #[error("failed to load eBPF program '{name}': {reason}")]
    LoadError { name: String, reason: String },

    /// Failed to access or create an eBPF map.
    #[error("eBPF map error for '{map_name}': {reason}")]
    MapError { map_name: String, reason: String },

    /// Error while processing an event from the ring buffer.
    #[error("event processing error: {0}")]
    EventError(String),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, EyeError>;
