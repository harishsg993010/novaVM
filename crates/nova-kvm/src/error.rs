use std::io;

/// Errors that can occur when interacting with KVM.
#[derive(Debug, thiserror::Error)]
pub enum KvmError {
    /// Failed to open /dev/kvm device.
    #[error("failed to open /dev/kvm: {0}")]
    DeviceOpen(io::Error),

    /// KVM API version mismatch (expected 12).
    #[error("KVM API version mismatch: expected 12, got {0}")]
    ApiVersion(i32),

    /// A required KVM capability is missing.
    #[error("missing required KVM capability: {0}")]
    MissingCapability(String),

    /// Failed to create a VM.
    #[error("failed to create VM: {0}")]
    VmCreate(io::Error),

    /// Failed to create a vCPU.
    #[error("failed to create vCPU: {0}")]
    VcpuCreate(io::Error),

    /// Failed to set user memory region.
    #[error("failed to set user memory region: {0}")]
    MemoryRegion(io::Error),

    /// A generic ioctl failed.
    #[error("KVM ioctl {name} failed: {source}")]
    Ioctl {
        name: &'static str,
        source: io::Error,
    },

    /// Failed to mmap the vCPU run area.
    #[error("failed to mmap vCPU run area: {0}")]
    VcpuMmap(io::Error),

    /// Failed to set registers.
    #[error("failed to set registers: {0}")]
    SetRegs(io::Error),

    /// Failed to get registers.
    #[error("failed to get registers: {0}")]
    GetRegs(io::Error),

    /// Failed to set special registers.
    #[error("failed to set special registers: {0}")]
    SetSregs(io::Error),

    /// Failed to get special registers.
    #[error("failed to get special registers: {0}")]
    GetSregs(io::Error),

    /// Failed to run vCPU.
    #[error("failed to run vCPU: {0}")]
    VcpuRun(io::Error),

    /// Failed to create IRQ chip.
    #[error("failed to create IRQ chip: {0}")]
    CreateIrqChip(io::Error),

    /// Failed to create PIT2.
    #[error("failed to create PIT2: {0}")]
    CreatePit2(io::Error),

    /// Failed to register irqfd.
    #[error("failed to register irqfd: {0}")]
    RegisterIrqfd(io::Error),

    /// Failed to set TSS address.
    #[error("failed to set TSS address: {0}")]
    SetTssAddr(io::Error),

    /// Failed to get supported CPUID.
    #[error("failed to get supported CPUID: {0}")]
    GetSupportedCpuid(io::Error),

    /// Failed to set CPUID on vCPU.
    #[error("failed to set CPUID: {0}")]
    SetCpuid(io::Error),
}

pub type Result<T> = std::result::Result<T, KvmError>;
