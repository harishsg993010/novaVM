/// Errors from virtio operations.
#[derive(Debug, thiserror::Error)]
pub enum VirtioError {
    /// Invalid descriptor chain.
    #[error("invalid descriptor chain: {0}")]
    InvalidChain(String),

    /// Queue not ready.
    #[error("queue not ready")]
    QueueNotReady,

    /// Guest memory error.
    #[error("guest memory error: {0}")]
    Memory(#[from] nova_mem::MemError),

    /// I/O error from a device backend.
    #[error("device I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Device activation error.
    #[error("device activation error: {0}")]
    ActivationError(String),

    /// Invalid MMIO register access.
    #[error("invalid MMIO access: offset {offset:#x}, size {size}")]
    InvalidMmioAccess { offset: u64, size: u32 },
}

pub type Result<T> = std::result::Result<T, VirtioError>;
