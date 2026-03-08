use std::io;

/// Errors from kernel loading operations.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// I/O error reading the kernel image.
    #[error("kernel I/O error: {0}")]
    Io(#[from] io::Error),

    /// The bzImage has an invalid or missing setup header magic.
    #[error("invalid bzImage magic: expected 0x53726448, got {0:#x}")]
    InvalidBzImageMagic(u32),

    /// The bzImage version is too old.
    #[error("bzImage boot protocol version {0:#06x} too old (need >= 0x0200)")]
    OldBootProtocol(u16),

    /// Invalid ELF magic.
    #[error("invalid ELF magic")]
    InvalidElfMagic,

    /// ELF is not 64-bit.
    #[error("ELF is not 64-bit (class = {0})")]
    Not64BitElf(u8),

    /// ELF is not executable.
    #[error("ELF is not executable (type = {0})")]
    NotExecutableElf(u16),

    /// No loadable segments in ELF.
    #[error("no loadable segments in ELF")]
    NoLoadableSegments,

    /// Invalid PVH note.
    #[error("invalid PVH start address note")]
    InvalidPvhNote,

    /// Kernel image is too large for guest memory.
    #[error("kernel too large: {size} bytes exceeds available memory at {load_addr:#x}")]
    KernelTooLarge { size: usize, load_addr: u64 },

    /// Initrd too large.
    #[error("initrd too large: {size} bytes")]
    InitrdTooLarge { size: usize },

    /// Command line too long.
    #[error("kernel command line too long: {len} bytes (max {max})")]
    CmdlineTooLong { len: usize, max: usize },

    /// Guest memory error.
    #[error("guest memory error: {0}")]
    Memory(#[from] nova_mem::MemError),
}

pub type Result<T> = std::result::Result<T, BootError>;
