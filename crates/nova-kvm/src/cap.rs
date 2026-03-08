/// KVM capability identifiers used with KVM_CHECK_EXTENSION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum KvmCap {
    /// KVM_CAP_IRQCHIP — in-kernel interrupt controller.
    Irqchip = 0,
    /// KVM_CAP_HLT — HLT instruction interception.
    Hlt = 1,
    /// KVM_CAP_USER_MEMORY — user memory regions.
    UserMemory = 3,
    /// KVM_CAP_SET_TSS_ADDR — set TSS address.
    SetTssAddr = 4,
    /// KVM_CAP_EXT_CPUID — extended CPUID.
    ExtCpuid = 7,
    /// KVM_CAP_NR_VCPUS — recommended vCPU count.
    NrVcpus = 9,
    /// KVM_CAP_MAX_VCPUS — maximum vCPU count.
    MaxVcpus = 66,
    /// KVM_CAP_MAX_VCPU_ID — maximum vCPU ID.
    MaxVcpuId = 128,
    /// KVM_CAP_PIT2 — in-kernel PIT.
    Pit2 = 33,
    /// KVM_CAP_IRQFD — irqfd support.
    Irqfd = 32,
    /// KVM_CAP_IOEVENTFD — ioeventfd support.
    IoEventfd = 36,
    /// KVM_CAP_IMMEDIATE_EXIT — immediate exit from KVM_RUN.
    ImmediateExit = 136,
    /// KVM_CAP_MP_STATE — MP state get/set.
    MpState = 14,
    /// KVM_CAP_DIRTY_LOG_RING — dirty log ring buffer.
    DirtyLogRing = 192,
    /// KVM_CAP_SPLIT_IRQCHIP — split IRQ chip.
    SplitIrqchip = 121,
}

impl KvmCap {
    /// Returns the numeric KVM capability constant value.
    pub fn as_raw(self) -> u32 {
        self as u32
    }

    /// Returns a human-readable name for this capability.
    pub fn name(self) -> &'static str {
        match self {
            Self::Irqchip => "KVM_CAP_IRQCHIP",
            Self::Hlt => "KVM_CAP_HLT",
            Self::UserMemory => "KVM_CAP_USER_MEMORY",
            Self::SetTssAddr => "KVM_CAP_SET_TSS_ADDR",
            Self::ExtCpuid => "KVM_CAP_EXT_CPUID",
            Self::NrVcpus => "KVM_CAP_NR_VCPUS",
            Self::MaxVcpus => "KVM_CAP_MAX_VCPUS",
            Self::MaxVcpuId => "KVM_CAP_MAX_VCPU_ID",
            Self::Pit2 => "KVM_CAP_PIT2",
            Self::Irqfd => "KVM_CAP_IRQFD",
            Self::IoEventfd => "KVM_CAP_IOEVENTFD",
            Self::ImmediateExit => "KVM_CAP_IMMEDIATE_EXIT",
            Self::MpState => "KVM_CAP_MP_STATE",
            Self::DirtyLogRing => "KVM_CAP_DIRTY_LOG_RING",
            Self::SplitIrqchip => "KVM_CAP_SPLIT_IRQCHIP",
        }
    }
}
