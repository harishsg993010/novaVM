use std::os::unix::io::{AsRawFd, RawFd};

use crate::error::{KvmError, Result};
use crate::kvm_bindings::*;
use crate::vcpu::VcpuFd;

/// A handle to a KVM virtual machine.
pub struct VmFd {
    fd: RawFd,
    run_size: usize,
}

impl VmFd {
    /// Create a new VmFd wrapper.
    ///
    /// # Arguments
    /// * `fd` — The raw file descriptor returned by KVM_CREATE_VM.
    /// * `run_size` — The mmap size for vCPU run regions.
    pub(crate) fn new(fd: RawFd, run_size: usize) -> Self {
        Self { fd, run_size }
    }

    /// Create a new vCPU with the given ID.
    pub fn create_vcpu(&self, id: u64) -> Result<VcpuFd> {
        // SAFETY: KVM_CREATE_VCPU is a valid ioctl on a VM fd.
        // The id is passed as the ioctl arg. Returns a new fd on success.
        let ret = unsafe { libc::ioctl(self.fd, KVM_CREATE_VCPU, id) };
        if ret < 0 {
            return Err(KvmError::VcpuCreate(std::io::Error::last_os_error()));
        }
        VcpuFd::new(ret, self.run_size)
    }

    /// Set a user memory region for the VM.
    pub fn set_user_memory_region(&self, region: &KvmUserspaceMemoryRegion) -> Result<()> {
        // SAFETY: KVM_SET_USER_MEMORY_REGION is a valid ioctl on a VM fd.
        // The region pointer is valid for the duration of the call.
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_SET_USER_MEMORY_REGION,
                region as *const KvmUserspaceMemoryRegion,
            )
        };
        if ret < 0 {
            return Err(KvmError::MemoryRegion(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Create an in-kernel IRQ chip (PIC + IOAPIC).
    pub fn create_irqchip(&self) -> Result<()> {
        // SAFETY: KVM_CREATE_IRQCHIP is a valid ioctl on a VM fd with no args.
        let ret = unsafe { libc::ioctl(self.fd, KVM_CREATE_IRQCHIP, 0) };
        if ret < 0 {
            return Err(KvmError::CreateIrqChip(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Create an in-kernel PIT (i8254) with speaker port emulation.
    pub fn create_pit2(&self) -> Result<()> {
        let pit_config = KvmPitConfig {
            flags: KVM_PIT_SPEAKER_DUMMY,
            ..Default::default()
        };
        // SAFETY: KVM_CREATE_PIT2 is a valid ioctl on a VM fd.
        let ret =
            unsafe { libc::ioctl(self.fd, KVM_CREATE_PIT2, &pit_config as *const KvmPitConfig) };
        if ret < 0 {
            return Err(KvmError::CreatePit2(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Set the level of an IRQ line (assert or de-assert).
    pub fn set_irq_line(&self, irq: u32, level: u32) -> Result<()> {
        let irq_level = KvmIrqLevel { irq, level };
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_IRQ_LINE,
                &irq_level as *const KvmIrqLevel,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_IRQ_LINE",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Register an eventfd for IRQ injection.
    pub fn register_irqfd(&self, fd: RawFd, gsi: u32) -> Result<()> {
        let irqfd = KvmIrqfd {
            fd: fd as u32,
            gsi,
            ..Default::default()
        };
        // SAFETY: KVM_IRQFD is a valid ioctl on a VM fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_IRQFD, &irqfd as *const KvmIrqfd) };
        if ret < 0 {
            return Err(KvmError::RegisterIrqfd(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Set the address of the three-page TSS region (required for Intel VT-x).
    pub fn set_tss_addr(&self, addr: u64) -> Result<()> {
        // SAFETY: KVM_SET_TSS_ADDR is a valid ioctl on a VM fd.
        let ret = unsafe { libc::ioctl(self.fd, KVM_SET_TSS_ADDR, addr) };
        if ret < 0 {
            return Err(KvmError::SetTssAddr(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Get the dirty page bitmap for a memory slot.
    pub fn get_dirty_log(&self, slot: u32, bitmap: &mut [u64]) -> Result<()> {
        let dirty_log = KvmDirtyLog {
            slot,
            padding: 0,
            dirty_bitmap: bitmap.as_mut_ptr() as u64,
        };
        // SAFETY: KVM_GET_DIRTY_LOG is a valid ioctl; bitmap is valid and large enough.
        let ret =
            unsafe { libc::ioctl(self.fd, KVM_GET_DIRTY_LOG, &dirty_log as *const KvmDirtyLog) };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_DIRTY_LOG",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Snapshot-related ioctls
    // -----------------------------------------------------------------------

    /// Get the VM clock (TSC offset, etc.).
    pub fn get_clock(&self) -> Result<KvmClockData> {
        let mut clock = KvmClockData::default();
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_GET_CLOCK,
                &mut clock as *mut KvmClockData,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_CLOCK",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(clock)
    }

    /// Set the VM clock.
    pub fn set_clock(&self, clock: &KvmClockData) -> Result<()> {
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_SET_CLOCK,
                clock as *const KvmClockData,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_CLOCK",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Get the state of an in-kernel IRQ chip.
    /// `chip_id`: 0 = PIC master, 1 = PIC slave, 2 = IOAPIC.
    pub fn get_irqchip(&self, chip_id: u32) -> Result<KvmIrqchip> {
        let mut irqchip = KvmIrqchip {
            chip_id,
            ..Default::default()
        };
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_GET_IRQCHIP,
                &mut irqchip as *mut KvmIrqchip,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_IRQCHIP",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(irqchip)
    }

    /// Set the state of an in-kernel IRQ chip.
    pub fn set_irqchip(&self, irqchip: &KvmIrqchip) -> Result<()> {
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_SET_IRQCHIP,
                irqchip as *const KvmIrqchip,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_IRQCHIP",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Get the PIT2 state.
    pub fn get_pit2(&self) -> Result<KvmPitState2> {
        let mut pit = KvmPitState2::default();
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_GET_PIT2,
                &mut pit as *mut KvmPitState2,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_GET_PIT2",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(pit)
    }

    /// Set the PIT2 state.
    pub fn set_pit2(&self, pit: &KvmPitState2) -> Result<()> {
        let ret = unsafe {
            libc::ioctl(
                self.fd,
                KVM_SET_PIT2,
                pit as *const KvmPitState2,
            )
        };
        if ret < 0 {
            return Err(KvmError::Ioctl {
                name: "KVM_SET_PIT2",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Returns the raw file descriptor for this VM.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl AsRawFd for VmFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for VmFd {
    fn drop(&mut self) {
        // SAFETY: We own this fd and it was returned by KVM_CREATE_VM.
        unsafe {
            libc::close(self.fd);
        }
    }
}
