use std::os::unix::io::{AsRawFd, RawFd};

use crate::error::{KvmError, Result};
use crate::kvm_bindings::*;

/// Represents a VM exit from KVM_RUN.
#[derive(Debug)]
pub enum VmExit {
    /// MMIO read: guest read from `addr`, expects `len` bytes of data.
    MmioRead { addr: u64, len: u32 },
    /// MMIO write: guest wrote `data` (first `len` bytes valid) to `addr`.
    MmioWrite { addr: u64, data: [u8; 8], len: u32 },
    /// Port I/O in: guest performed `in` from `port`, size bytes.
    IoIn { port: u16, size: u8 },
    /// Port I/O out: guest performed `out` to `port` with `data`.
    IoOut { port: u16, size: u8, data: u32 },
    /// Guest executed HLT.
    Hlt,
    /// Guest triggered shutdown (triple fault, etc.).
    Shutdown,
    /// The KVM_RUN ioctl was interrupted by a signal (EINTR).
    Interrupted,
    /// KVM internal error.
    InternalError { suberror: u32 },
    /// Unknown or unhandled exit reason.
    Unknown(u32),
}

/// A handle to a KVM vCPU.
pub struct VcpuFd {
    fd: RawFd,
    kvm_run: *mut KvmRun,
    mmap_size: usize,
}

// SAFETY: VcpuFd is not Clone and the kvm_run pointer is only accessed
// through &self / &mut self methods. The underlying fd is thread-safe
// as long as only one thread calls KVM_RUN at a time (which is the KVM contract).
unsafe impl Send for VcpuFd {}

impl VcpuFd {
    /// Create a VcpuFd by mmap-ing the kvm_run region from the vCPU fd.
    pub(crate) fn new(fd: RawFd, run_size: usize) -> Result<Self> {
        // SAFETY: We mmap the kvm_run shared region from the vCPU fd.
        // The fd is valid (just returned by KVM_CREATE_VCPU) and run_size
        // was obtained from KVM_GET_VCPU_MMAP_SIZE.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                run_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            // Close the fd we own before returning error.
            unsafe { libc::close(fd) };
            return Err(KvmError::VcpuMmap(std::io::Error::last_os_error()));
        }

        Ok(Self {
            fd,
            kvm_run: addr.cast::<KvmRun>(),
            mmap_size: run_size,
        })
    }

    /// Get the current general-purpose registers.
    pub fn get_regs(&self) -> Result<KvmRegs> {
        let mut regs = KvmRegs::default();
        // SAFETY: KVM_GET_REGS is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_GET_REGS, &mut regs as *mut KvmRegs) };
        if ret < 0 {
            return Err(KvmError::GetRegs(std::io::Error::last_os_error()));
        }
        Ok(regs)
    }

    /// Set the general-purpose registers.
    pub fn set_regs(&self, regs: &KvmRegs) -> Result<()> {
        // SAFETY: KVM_SET_REGS is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_REGS, regs as *const KvmRegs) };
        if ret < 0 {
            return Err(KvmError::SetRegs(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Get the special (system) registers.
    pub fn get_sregs(&self) -> Result<KvmSregs> {
        let mut sregs: KvmSregs = unsafe { std::mem::zeroed() };
        // SAFETY: KVM_GET_SREGS is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_GET_SREGS, &mut sregs as *mut KvmSregs) };
        if ret < 0 {
            return Err(KvmError::GetSregs(std::io::Error::last_os_error()));
        }
        Ok(sregs)
    }

    /// Set the special (system) registers.
    pub fn set_sregs(&self, sregs: &KvmSregs) -> Result<()> {
        // SAFETY: KVM_SET_SREGS is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_SREGS, sregs as *const KvmSregs) };
        if ret < 0 {
            return Err(KvmError::SetSregs(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Run the vCPU until it exits. Returns the exit reason.
    ///
    /// # Safety contract
    /// This must be called from a single thread at a time per vCPU (KVM requirement).
    /// No allocations, no logging, no mutexes in this hot path.
    pub fn run(&self) -> Result<VmExit> {
        // SAFETY: KVM_RUN is a valid ioctl on a vCPU fd with no arguments.
        let ret = unsafe { libc::ioctl(self.fd, KVM_RUN, 0) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                // Signal interrupted the ioctl — clear immediate_exit so the
                // next call doesn't return immediately, then report Interrupted.
                unsafe { (*self.kvm_run).immediate_exit = 0; }
                return Ok(VmExit::Interrupted);
            }
            return Err(KvmError::VcpuRun(err));
        }

        // SAFETY: kvm_run is valid and mapped for the lifetime of VcpuFd.
        let run = unsafe { &*self.kvm_run };
        let exit = match run.exit_reason {
            KVM_EXIT_IO => {
                // SAFETY: When exit_reason == KVM_EXIT_IO, the exit_data
                // union contains a KvmRunExitIo at offset 0.
                let io = unsafe { &*(run.exit_data.as_ptr().cast::<KvmRunExitIo>()) };
                if io.direction == KVM_EXIT_IO_OUT {
                    // Read the data from the kvm_run page at data_offset.
                    let data_ptr =
                        (self.kvm_run as *const u8).wrapping_add(io.data_offset as usize);
                    let mut data_val: u32 = 0;
                    // SAFETY: data_offset points within the mmap'd kvm_run region.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            data_ptr,
                            &mut data_val as *mut u32 as *mut u8,
                            io.size as usize,
                        );
                    }
                    VmExit::IoOut {
                        port: io.port,
                        size: io.size,
                        data: data_val,
                    }
                } else {
                    VmExit::IoIn {
                        port: io.port,
                        size: io.size,
                    }
                }
            }
            KVM_EXIT_MMIO => {
                // SAFETY: When exit_reason == KVM_EXIT_MMIO, the exit_data
                // union contains KvmRunExitMmio.
                let mmio = unsafe { &*(run.exit_data.as_ptr().cast::<KvmRunExitMmio>()) };
                if mmio.is_write != 0 {
                    VmExit::MmioWrite {
                        addr: mmio.phys_addr,
                        data: mmio.data,
                        len: mmio.len,
                    }
                } else {
                    VmExit::MmioRead {
                        addr: mmio.phys_addr,
                        len: mmio.len,
                    }
                }
            }
            KVM_EXIT_HLT => VmExit::Hlt,
            KVM_EXIT_SHUTDOWN => VmExit::Shutdown,
            KVM_EXIT_INTERNAL_ERROR => {
                // The suberror is the first u32 in the exit data for internal errors.
                let suberror = unsafe { *(run.exit_data.as_ptr().cast::<u32>()) };
                VmExit::InternalError { suberror }
            }
            other => VmExit::Unknown(other),
        };

        Ok(exit)
    }

    /// Set the immediate_exit flag so the next KVM_RUN returns immediately.
    pub fn set_immediate_exit(&self, val: bool) {
        // SAFETY: kvm_run is mapped and valid.
        unsafe {
            (*self.kvm_run).immediate_exit = u8::from(val);
        }
    }

    /// Returns the raw file descriptor.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Set CPUID entries for this vCPU.
    ///
    /// Must be called before the first `run()`. Entries should come from
    /// `Kvm::get_supported_cpuid()`.
    pub fn set_cpuid2(&self, entries: &[KvmCpuidEntry2]) -> Result<()> {
        let entry_size = std::mem::size_of::<KvmCpuidEntry2>();
        let header_size = 8usize; // nent (u32) + padding (u32)
        let total_size = header_size + entries.len() * entry_size;

        let mut buf = vec![0u8; total_size];

        // Write header.
        unsafe {
            let nent_ptr = buf.as_mut_ptr().cast::<u32>();
            *nent_ptr = entries.len() as u32;
        }

        // Write entries.
        let entries_dst =
            unsafe { buf.as_mut_ptr().add(header_size).cast::<KvmCpuidEntry2>() };
        for (i, entry) in entries.iter().enumerate() {
            unsafe {
                *entries_dst.add(i) = *entry;
            }
        }

        // SAFETY: KVM_SET_CPUID2 is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_CPUID2, buf.as_ptr()) };
        if ret < 0 {
            return Err(KvmError::SetCpuid(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Get the XSAVE state (FPU + SSE + AVX registers).
    pub fn get_xsave(&self) -> Result<KvmXsave> {
        let mut xsave = KvmXsave::default();
        let ret = unsafe { libc::ioctl(self.fd, KVM_GET_XSAVE, &mut xsave as *mut KvmXsave) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_XSAVE",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(xsave)
    }

    /// Set the XSAVE state.
    pub fn set_xsave(&self, xsave: &KvmXsave) -> Result<()> {
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_XSAVE, xsave as *const KvmXsave) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_XSAVE",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Get the extended control registers (XCR0, etc.)
    pub fn get_xcrs(&self) -> Result<KvmXcrs> {
        let mut xcrs = KvmXcrs::default();
        let ret = unsafe { libc::ioctl(self.fd, KVM_GET_XCRS, &mut xcrs as *mut KvmXcrs) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_XCRS",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(xcrs)
    }

    /// Set the extended control registers.
    pub fn set_xcrs(&self, xcrs: &KvmXcrs) -> Result<()> {
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_XCRS, xcrs as *const KvmXcrs) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_XCRS",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Get vCPU events (pending interrupts, exceptions, NMIs).
    pub fn get_vcpu_events(&self) -> Result<KvmVcpuEvents> {
        let mut events = KvmVcpuEvents::default();
        let ret = unsafe { libc::ioctl(self.fd, KVM_GET_VCPU_EVENTS, &mut events as *mut KvmVcpuEvents) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_VCPU_EVENTS",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(events)
    }

    /// Set vCPU events.
    pub fn set_vcpu_events(&self, events: &KvmVcpuEvents) -> Result<()> {
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_VCPU_EVENTS, events as *const KvmVcpuEvents) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_VCPU_EVENTS",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Get MSR values from the vCPU. Pass the MSR indices to read.
    /// Returns a Vec of (index, data) pairs for successfully read MSRs.
    pub fn get_msrs(&self, indices: &[u32]) -> Result<Vec<KvmMsrEntry>> {
        let entry_size = std::mem::size_of::<KvmMsrEntry>();
        let header_size = 8usize; // nmsrs (u32) + pad (u32)
        let total_size = header_size + indices.len() * entry_size;

        let mut buf = vec![0u8; total_size];

        // Write header.
        unsafe {
            let nmsrs_ptr = buf.as_mut_ptr().cast::<u32>();
            *nmsrs_ptr = indices.len() as u32;
        }

        // Write entry indices.
        let entries_dst = unsafe { buf.as_mut_ptr().add(header_size).cast::<KvmMsrEntry>() };
        for (i, &index) in indices.iter().enumerate() {
            unsafe {
                (*entries_dst.add(i)).index = index;
            }
        }

        // SAFETY: KVM_GET_MSRS is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_GET_MSRS, buf.as_mut_ptr()) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_MSRS",
                source: std::io::Error::last_os_error(),
            });
        }

        // ret = number of MSRs successfully read.
        let count = ret as usize;
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let entry = unsafe { *entries_dst.add(i) };
            result.push(entry);
        }
        Ok(result)
    }

    /// Set MSR values on the vCPU.
    pub fn set_msrs(&self, entries: &[KvmMsrEntry]) -> Result<()> {
        let entry_size = std::mem::size_of::<KvmMsrEntry>();
        let header_size = 8usize;
        let total_size = header_size + entries.len() * entry_size;

        let mut buf = vec![0u8; total_size];

        // Write header.
        unsafe {
            let nmsrs_ptr = buf.as_mut_ptr().cast::<u32>();
            *nmsrs_ptr = entries.len() as u32;
        }

        // Write entries.
        let entries_dst = unsafe { buf.as_mut_ptr().add(header_size).cast::<KvmMsrEntry>() };
        for (i, entry) in entries.iter().enumerate() {
            unsafe {
                *entries_dst.add(i) = *entry;
            }
        }

        // SAFETY: KVM_SET_MSRS is a valid ioctl on a vCPU fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_MSRS, buf.as_ptr()) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_MSRS",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Get the local APIC state.
    pub fn get_lapic(&self) -> Result<KvmLapicState> {
        let mut lapic = KvmLapicState::default();
        let ret =
            unsafe { libc::ioctl(self.fd, KVM_GET_LAPIC, &mut lapic as *mut KvmLapicState) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_LAPIC",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(lapic)
    }

    /// Set the local APIC state.
    pub fn set_lapic(&self, lapic: &KvmLapicState) -> Result<()> {
        let ret =
            unsafe { libc::ioctl(self.fd, KVM_SET_LAPIC, lapic as *const KvmLapicState) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_LAPIC",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Configure LAPIC LINT0 for ExtINT (PIC interrupt delivery) and
    /// LINT1 for NMI. Required for kernels 5.x+ to receive timer interrupts.
    pub fn set_lint(&self) -> Result<()> {
        const APIC_LVT0: usize = 0x350;
        const APIC_LVT1: usize = 0x360;
        const APIC_MODE_NMI: u32 = 0x4;
        const APIC_MODE_EXTINT: u32 = 0x7;

        fn get_reg(lapic: &KvmLapicState, offset: usize) -> u32 {
            let bytes = &lapic.regs[offset..offset + 4];
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }

        fn set_reg(lapic: &mut KvmLapicState, offset: usize, val: u32) {
            let bytes = val.to_le_bytes();
            lapic.regs[offset..offset + 4].copy_from_slice(&bytes);
        }

        fn set_delivery_mode(reg: u32, mode: u32) -> u32 {
            (reg & !0x700) | (mode << 8)
        }

        let mut lapic = self.get_lapic()?;

        let lvt0 = get_reg(&lapic, APIC_LVT0);
        set_reg(&mut lapic, APIC_LVT0, set_delivery_mode(lvt0, APIC_MODE_EXTINT));

        let lvt1 = get_reg(&lapic, APIC_LVT1);
        set_reg(&mut lapic, APIC_LVT1, set_delivery_mode(lvt1, APIC_MODE_NMI));

        self.set_lapic(&lapic)
    }

    /// Returns a pointer to the kvm_run region. Useful for advanced users
    /// who need to write data back (e.g., for IO in).
    ///
    /// # Safety
    /// Caller must ensure no concurrent mutation.
    pub unsafe fn kvm_run_ptr(&self) -> *mut KvmRun {
        self.kvm_run
    }
}

impl AsRawFd for VcpuFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for VcpuFd {
    fn drop(&mut self) {
        // SAFETY: We own the mmap region and the fd.
        unsafe {
            libc::munmap(self.kvm_run.cast(), self.mmap_size);
            libc::close(self.fd);
        }
    }
}
