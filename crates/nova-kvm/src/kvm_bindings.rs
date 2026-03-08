// Raw KVM ioctl numbers and data structures.
// These are defined in linux/kvm.h — we reproduce them here to avoid
// depending on bindgen at build time.

#![allow(non_camel_case_types, dead_code)]

use std::mem;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ioctl direction/type encoding helpers (Linux ABI)
// ---------------------------------------------------------------------------
const _IOC_NRBITS: u32 = 8;
const _IOC_TYPEBITS: u32 = 8;
const _IOC_SIZEBITS: u32 = 14;

const _IOC_NRSHIFT: u32 = 0;
const _IOC_TYPESHIFT: u32 = _IOC_NRSHIFT + _IOC_NRBITS;
const _IOC_SIZESHIFT: u32 = _IOC_TYPESHIFT + _IOC_TYPEBITS;
const _IOC_DIRSHIFT: u32 = _IOC_SIZESHIFT + _IOC_SIZEBITS;

const _IOC_NONE: u32 = 0;
const _IOC_WRITE: u32 = 1;
const _IOC_READ: u32 = 2;

const fn _ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u64 {
    ((dir << _IOC_DIRSHIFT)
        | (ty << _IOC_TYPESHIFT)
        | (nr << _IOC_NRSHIFT)
        | (size << _IOC_SIZESHIFT)) as u64
}

const fn _io(ty: u32, nr: u32) -> u64 {
    _ioc(_IOC_NONE, ty, nr, 0)
}

const fn _iow<T>(ty: u32, nr: u32) -> u64 {
    _ioc(_IOC_WRITE, ty, nr, mem::size_of::<T>() as u32)
}

const fn _ior<T>(ty: u32, nr: u32) -> u64 {
    _ioc(_IOC_READ, ty, nr, mem::size_of::<T>() as u32)
}

const fn _iowr<T>(ty: u32, nr: u32) -> u64 {
    _ioc(_IOC_READ | _IOC_WRITE, ty, nr, mem::size_of::<T>() as u32)
}

const KVMIO: u32 = 0xAE;

// ---------------------------------------------------------------------------
// System ioctls (on /dev/kvm fd)
// ---------------------------------------------------------------------------
pub const KVM_GET_API_VERSION: u64 = _io(KVMIO, 0x00);
pub const KVM_CREATE_VM: u64 = _io(KVMIO, 0x01);
pub const KVM_CHECK_EXTENSION: u64 = _io(KVMIO, 0x03);
pub const KVM_GET_VCPU_MMAP_SIZE: u64 = _io(KVMIO, 0x04);

// ---------------------------------------------------------------------------
// VM ioctls (on VM fd)
// ---------------------------------------------------------------------------
pub const KVM_CREATE_VCPU: u64 = _io(KVMIO, 0x41);
pub const KVM_SET_USER_MEMORY_REGION: u64 = _iow::<KvmUserspaceMemoryRegion>(KVMIO, 0x46);
pub const KVM_CREATE_IRQCHIP: u64 = _io(KVMIO, 0x60);
pub const KVM_CREATE_PIT2: u64 = _iow::<KvmPitConfig>(KVMIO, 0x77);
pub const KVM_IRQFD: u64 = _iow::<KvmIrqfd>(KVMIO, 0x76);
pub const KVM_IRQ_LINE: u64 = _iow::<KvmIrqLevel>(KVMIO, 0x61);
pub const KVM_SET_TSS_ADDR: u64 = _io(KVMIO, 0x47);
pub const KVM_GET_DIRTY_LOG: u64 = _iow::<KvmDirtyLog>(KVMIO, 0x42);

// Snapshot-related VM ioctls.
pub const KVM_GET_CLOCK: u64 = _ior::<KvmClockData>(KVMIO, 0x7C);
pub const KVM_SET_CLOCK: u64 = _iow::<KvmClockData>(KVMIO, 0x7B);
pub const KVM_GET_IRQCHIP: u64 = _iowr::<KvmIrqchip>(KVMIO, 0x62);
pub const KVM_SET_IRQCHIP: u64 = _ior::<KvmIrqchip>(KVMIO, 0x63);
pub const KVM_GET_PIT2: u64 = _ior::<KvmPitState2>(KVMIO, 0x9F);
pub const KVM_SET_PIT2: u64 = _iow::<KvmPitState2>(KVMIO, 0xA0);

// ---------------------------------------------------------------------------
// vCPU ioctls (on vCPU fd)
// ---------------------------------------------------------------------------
pub const KVM_RUN: u64 = _io(KVMIO, 0x80);
pub const KVM_GET_REGS: u64 = _ior::<KvmRegs>(KVMIO, 0x81);
pub const KVM_SET_REGS: u64 = _iow::<KvmRegs>(KVMIO, 0x82);
pub const KVM_GET_SREGS: u64 = _ior::<KvmSregs>(KVMIO, 0x83);
pub const KVM_SET_SREGS: u64 = _iow::<KvmSregs>(KVMIO, 0x84);

// LAPIC ioctls (on vCPU fd).
pub const KVM_GET_LAPIC: u64 = _ior::<KvmLapicState>(KVMIO, 0x8E);
pub const KVM_SET_LAPIC: u64 = _iow::<KvmLapicState>(KVMIO, 0x8F);

// XSAVE/XCRS ioctls (snapshot FPU + extended state).
pub const KVM_GET_XSAVE: u64 = _ior::<KvmXsave>(KVMIO, 0xA4);
pub const KVM_SET_XSAVE: u64 = _iow::<KvmXsave>(KVMIO, 0xA5);
pub const KVM_GET_XCRS: u64 = _ior::<KvmXcrs>(KVMIO, 0xA6);
pub const KVM_SET_XCRS: u64 = _iow::<KvmXcrs>(KVMIO, 0xA7);
pub const KVM_GET_VCPU_EVENTS: u64 = _ior::<KvmVcpuEvents>(KVMIO, 0x9F);
pub const KVM_SET_VCPU_EVENTS: u64 = _iow::<KvmVcpuEvents>(KVMIO, 0xA0);

// ---------------------------------------------------------------------------
// KVM exit reasons
// ---------------------------------------------------------------------------
pub const KVM_EXIT_UNKNOWN: u32 = 0;
pub const KVM_EXIT_IO: u32 = 2;
pub const KVM_EXIT_MMIO: u32 = 6;
pub const KVM_EXIT_HLT: u32 = 5;
pub const KVM_EXIT_SHUTDOWN: u32 = 8;
pub const KVM_EXIT_INTERNAL_ERROR: u32 = 17;

/// IO direction: in
pub const KVM_EXIT_IO_IN: u8 = 0;
/// IO direction: out
pub const KVM_EXIT_IO_OUT: u8 = 1;

// ---------------------------------------------------------------------------
// KVM data structures
// ---------------------------------------------------------------------------

/// Memory region passed to KVM_SET_USER_MEMORY_REGION.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmUserspaceMemoryRegion {
    pub slot: u32,
    pub flags: u32,
    pub guest_phys_addr: u64,
    pub memory_size: u64,
    pub userspace_addr: u64,
}

/// Flags for KvmUserspaceMemoryRegion.
pub const KVM_MEM_LOG_DIRTY_PAGES: u32 = 1;
pub const KVM_MEM_READONLY: u32 = 2;

/// PIT configuration for KVM_CREATE_PIT2.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmPitConfig {
    pub flags: u32,
    pub pad: [u32; 15],
}

/// Flag for KvmPitConfig: emulate speaker port (0x61) in-kernel.
pub const KVM_PIT_SPEAKER_DUMMY: u32 = 1;

/// IRQ level for KVM_IRQ_LINE.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmIrqLevel {
    pub irq: u32,
    pub level: u32,
}

/// IRQ fd registration for KVM_IRQFD.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmIrqfd {
    pub fd: u32,
    pub gsi: u32,
    pub flags: u32,
    pub resamplefd: u32,
    pub pad: [u8; 16],
}

/// General-purpose registers.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

/// Segment register.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmSegment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub type_: u8,
    pub present: u8,
    pub dpl: u8,
    pub db: u8,
    pub s: u8,
    pub l: u8,
    pub g: u8,
    pub avl: u8,
    pub unusable: u8,
    pub padding: u8,
}

/// Descriptor table register.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmDtable {
    pub base: u64,
    pub limit: u16,
    pub padding: [u16; 3],
}

/// Special (system) registers.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmSregs {
    pub cs: KvmSegment,
    pub ds: KvmSegment,
    pub es: KvmSegment,
    pub fs: KvmSegment,
    pub gs: KvmSegment,
    pub ss: KvmSegment,
    pub tr: KvmSegment,
    pub ldt: KvmSegment,
    pub gdt: KvmDtable,
    pub idt: KvmDtable,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
    pub interrupt_bitmap: [u64; 4],
}

/// Dirty log query for KVM_GET_DIRTY_LOG.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KvmDirtyLog {
    pub slot: u32,
    pub padding: u32,
    pub dirty_bitmap: u64, // actually a pointer
}

/// The `kvm_run` shared region mapped from the vCPU fd.
/// We only model the fields we need — the full structure is 2048+ bytes.
#[repr(C)]
pub struct KvmRun {
    // Request/immediate_exit fields
    pub request_interrupt_window: u8,
    pub immediate_exit: u8,
    pub padding1: [u8; 6],

    // Exit reason
    pub exit_reason: u32,
    pub ready_for_interrupt_injection: u8,
    pub if_flag: u8,
    pub flags: u16,

    // CR8
    pub cr8: u64,
    pub apic_base: u64,

    // Exit data union — we treat this as raw bytes and interpret per exit_reason.
    // Offset 32 from the start of kvm_run.
    pub exit_data: [u8; 256],
}

/// IO exit data extracted from kvm_run.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KvmRunExitIo {
    pub direction: u8,
    pub size: u8,
    pub port: u16,
    pub count: u32,
    pub data_offset: u64,
}

/// MMIO exit data extracted from kvm_run.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KvmRunExitMmio {
    pub phys_addr: u64,
    pub data: [u8; 8],
    pub len: u32,
    pub is_write: u8,
}

// ---------------------------------------------------------------------------
// Snapshot-related KVM structures
// ---------------------------------------------------------------------------

/// Clock data for KVM_GET_CLOCK / KVM_SET_CLOCK.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmClockData {
    pub clock: u64,
    pub flags: u32,
    pub pad: [u32; 9],
}

/// IRQ chip state for KVM_GET_IRQCHIP / KVM_SET_IRQCHIP.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KvmIrqchip {
    pub chip_id: u32,
    pub pad: u32,
    pub chip: [u8; 512],
}

impl Default for KvmIrqchip {
    fn default() -> Self {
        Self {
            chip_id: 0,
            pad: 0,
            chip: [0u8; 512],
        }
    }
}

/// PIT channel state (part of KvmPitState2).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmPitChannelState {
    pub count: u32,
    pub latched_count: u16,
    pub count_latched: u8,
    pub status_latched: u8,
    pub status: u8,
    pub read_state: u8,
    pub write_state: u8,
    pub write_latch: u8,
    pub rw_mode: u8,
    pub mode: u8,
    pub bcd: u8,
    pub gate: u8,
    pub count_load_time: i64,
}

/// PIT state for KVM_GET_PIT2 / KVM_SET_PIT2.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmPitState2 {
    pub channels: [KvmPitChannelState; 3],
    pub flags: u32,
    pub padding: [u32; 9],
}

// ---------------------------------------------------------------------------
// MSR ioctls (on vCPU fd)
// ---------------------------------------------------------------------------

/// Header for the variable-length kvm_msrs structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KvmMsrsHdr {
    pub nmsrs: u32,
    pub pad: u32,
}

/// KVM_GET_MSRS — read MSR values from a vCPU.
pub const KVM_GET_MSRS: u64 = _iowr::<KvmMsrsHdr>(KVMIO, 0x88);

/// KVM_SET_MSRS — write MSR values to a vCPU.
pub const KVM_SET_MSRS: u64 = _iow::<KvmMsrsHdr>(KVMIO, 0x89);

/// A single MSR entry for KVM_GET_MSRS / KVM_SET_MSRS.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmMsrEntry {
    pub index: u32,
    pub reserved: u32,
    pub data: u64,
}

/// Well-known x86 MSR indices needed for snapshot restore.
pub const MSR_IA32_TSC: u32 = 0x10;
pub const MSR_IA32_SYSENTER_CS: u32 = 0x174;
pub const MSR_IA32_SYSENTER_ESP: u32 = 0x175;
pub const MSR_IA32_SYSENTER_EIP: u32 = 0x176;
pub const MSR_IA32_MISC_ENABLE: u32 = 0x1A0;
pub const MSR_STAR: u32 = 0xC000_0081;
pub const MSR_LSTAR: u32 = 0xC000_0082;
pub const MSR_CSTAR: u32 = 0xC000_0083;
pub const MSR_SYSCALL_MASK: u32 = 0xC000_0084;
pub const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;
pub const MSR_IA32_TSC_ADJUST: u32 = 0x3B;

/// List of MSRs to save/restore for VM snapshots.
pub const SNAPSHOT_MSRS: &[u32] = &[
    MSR_IA32_TSC,
    MSR_IA32_SYSENTER_CS,
    MSR_IA32_SYSENTER_ESP,
    MSR_IA32_SYSENTER_EIP,
    MSR_IA32_MISC_ENABLE,
    MSR_STAR,
    MSR_LSTAR,
    MSR_CSTAR,
    MSR_SYSCALL_MASK,
    MSR_KERNEL_GS_BASE,
    MSR_IA32_TSC_ADJUST,
];

// ---------------------------------------------------------------------------
// CPUID ioctls
// ---------------------------------------------------------------------------

/// Header for the variable-length kvm_cpuid2 structure (ioctl encoding only).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KvmCpuid2Hdr {
    pub nent: u32,
    pub padding: u32,
}

/// KVM_GET_SUPPORTED_CPUID — get host-supported CPUID entries (system ioctl).
pub const KVM_GET_SUPPORTED_CPUID: u64 = _iowr::<KvmCpuid2Hdr>(KVMIO, 0x05);

/// KVM_SET_CPUID2 — set CPUID entries for a vCPU (vCPU ioctl).
pub const KVM_SET_CPUID2: u64 = _iow::<KvmCpuid2Hdr>(KVMIO, 0x90);

/// A single CPUID entry for KVM_SET_CPUID2 / KVM_GET_SUPPORTED_CPUID.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmCpuidEntry2 {
    pub function: u32,
    pub index: u32,
    pub flags: u32,
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub padding: [u32; 3],
}

// ---------------------------------------------------------------------------
// XSAVE / XCRS / VCPU events structures (for snapshot)
// ---------------------------------------------------------------------------

/// XSAVE state area (FPU + SSE + AVX).
/// 4096 bytes covers the standard xsave area.
#[repr(C)]
#[derive(Clone)]
pub struct KvmXsave {
    pub region: [u32; 1024],
}

impl Default for KvmXsave {
    fn default() -> Self {
        Self { region: [0u32; 1024] }
    }
}

impl std::fmt::Debug for KvmXsave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvmXsave").field("size", &4096).finish()
    }
}

/// Extended control registers (XCR0, etc.)
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct KvmXcr {
    pub xcr: u32,
    pub reserved: u32,
    pub value: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KvmXcrs {
    pub nr_xcrs: u32,
    pub flags: u32,
    pub xcrs: [KvmXcr; 16],
    pub padding: [u64; 16],
}

impl Default for KvmXcrs {
    fn default() -> Self {
        Self {
            nr_xcrs: 0,
            flags: 0,
            xcrs: [KvmXcr::default(); 16],
            padding: [0u64; 16],
        }
    }
}

/// vCPU events (interrupts, NMIs, exceptions in-flight).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmVcpuEvents {
    pub exception: KvmVcpuEventException,
    pub interrupt: KvmVcpuEventInterrupt,
    pub nmi: KvmVcpuEventNmi,
    pub sipi_vector: u32,
    pub flags: u32,
    pub smi: KvmVcpuEventSmi,
    pub reserved: [u8; 27],
    pub exception_has_payload: u8,
    pub exception_payload: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmVcpuEventException {
    pub injected: u8,
    pub nr: u8,
    pub has_error_code: u8,
    pub pending: u8,
    pub error_code: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmVcpuEventInterrupt {
    pub injected: u8,
    pub nr: u8,
    pub soft: u8,
    pub shadow: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmVcpuEventNmi {
    pub injected: u8,
    pub pending: u8,
    pub masked: u8,
    pub pad: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KvmVcpuEventSmi {
    pub smm: u8,
    pub pending: u8,
    pub smm_inside_nmi: u8,
    pub latched_init: u8,
}

// ---------------------------------------------------------------------------
// LAPIC state (for KVM_GET_LAPIC / KVM_SET_LAPIC)
// ---------------------------------------------------------------------------

/// LAPIC register file size (0x400 = 1024 bytes).
pub const KVM_APIC_REG_SIZE: usize = 0x400;

/// Local APIC state for KVM_GET_LAPIC / KVM_SET_LAPIC.
#[repr(C)]
#[derive(Clone)]
pub struct KvmLapicState {
    pub regs: [u8; KVM_APIC_REG_SIZE],
}

impl Default for KvmLapicState {
    fn default() -> Self {
        Self {
            regs: [0u8; KVM_APIC_REG_SIZE],
        }
    }
}

impl std::fmt::Debug for KvmLapicState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvmLapicState")
            .field("size", &KVM_APIC_REG_SIZE)
            .finish()
    }
}

// Expected KVM API version.
pub const KVM_API_VERSION: i32 = 12;
