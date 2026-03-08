use std::io;

/// Errors from guest memory operations.
#[derive(Debug, thiserror::Error)]
pub enum MemError {
    /// Failed to mmap memory.
    #[error("mmap failed: {0}")]
    Mmap(io::Error),

    /// Failed to munmap memory.
    #[error("munmap failed: {0}")]
    Munmap(io::Error),

    /// Attempted access outside the guest memory bounds.
    #[error(
        "out of bounds: offset {offset:#x} + size {size:#x} exceeds region size {region_size:#x}"
    )]
    OutOfBounds {
        offset: u64,
        size: usize,
        region_size: u64,
    },

    /// No memory region found for the given guest address.
    #[error("no memory region found for guest address {0:#x}")]
    NoRegion(u64),

    /// Address arithmetic overflow.
    #[error("address overflow: {0:#x} + {1:#x}")]
    Overflow(u64, u64),

    /// Dirty log ioctl failed.
    #[error("dirty log operation failed: {0}")]
    DirtyLog(#[from] nova_kvm::error::KvmError),
}

pub type Result<T> = std::result::Result<T, MemError>;
