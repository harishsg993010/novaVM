//! # nova-kvm
//!
//! Safe Rust wrapper around the Linux KVM API for NovaVM.
//!
//! Provides the ability to:
//! - Open `/dev/kvm` and query capabilities
//! - Create VMs and vCPUs
//! - Set memory regions, registers, and run the vCPU

pub mod cap;
pub mod error;
pub mod kvm_bindings;
pub mod vcpu;
pub mod vm;

use std::os::unix::io::{AsRawFd, RawFd};

use cap::KvmCap;
use error::{KvmError, Result};
use kvm_bindings::*;
use vm::VmFd;

/// Handle to the `/dev/kvm` device.
pub struct Kvm {
    fd: RawFd,
    run_size: usize,
}

impl Kvm {
    /// Open `/dev/kvm` and verify API version compatibility.
    pub fn open() -> Result<Self> {
        // SAFETY: Opening /dev/kvm is safe; it returns a file descriptor.
        let fd = unsafe { libc::open(c"/dev/kvm".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(KvmError::DeviceOpen(std::io::Error::last_os_error()));
        }

        // Check API version.
        // SAFETY: KVM_GET_API_VERSION is a valid ioctl on /dev/kvm.
        let version = unsafe { libc::ioctl(fd, KVM_GET_API_VERSION, 0) };
        if version != KVM_API_VERSION {
            unsafe { libc::close(fd) };
            return Err(KvmError::ApiVersion(version));
        }

        // Get the mmap size for vCPU run regions.
        // SAFETY: KVM_GET_VCPU_MMAP_SIZE is a valid ioctl on /dev/kvm.
        let run_size = unsafe { libc::ioctl(fd, KVM_GET_VCPU_MMAP_SIZE, 0) };
        if run_size < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(KvmError::Ioctl {
                name: "KVM_GET_VCPU_MMAP_SIZE",
                source: err,
            });
        }

        tracing::debug!(api_version = version, run_size, "KVM device opened");

        Ok(Self {
            fd,
            run_size: run_size as usize,
        })
    }

    /// Check if a KVM capability is supported. Returns the capability value
    /// (0 means unsupported, >0 means supported with that value).
    pub fn check_capability(&self, cap: KvmCap) -> i32 {
        // SAFETY: KVM_CHECK_EXTENSION is a valid ioctl on /dev/kvm.
        unsafe { libc::ioctl(self.fd, KVM_CHECK_EXTENSION, cap.as_raw()) }
    }

    /// Check that a required capability is present, returning an error if not.
    pub fn require_capability(&self, cap: KvmCap) -> Result<i32> {
        let val = self.check_capability(cap);
        if val <= 0 {
            return Err(KvmError::MissingCapability(cap.name().to_string()));
        }
        Ok(val)
    }

    /// Create a new VM and return a VmFd handle.
    pub fn create_vm(&self) -> Result<VmFd> {
        // SAFETY: KVM_CREATE_VM is a valid ioctl on /dev/kvm. The argument 0
        // means default machine type.
        let vm_fd = unsafe { libc::ioctl(self.fd, KVM_CREATE_VM, 0) };
        if vm_fd < 0 {
            return Err(KvmError::VmCreate(std::io::Error::last_os_error()));
        }
        tracing::debug!(vm_fd, "created KVM VM");
        Ok(VmFd::new(vm_fd, self.run_size))
    }

    /// Returns the maximum number of vCPUs supported.
    pub fn max_vcpus(&self) -> i32 {
        let max = self.check_capability(KvmCap::MaxVcpus);
        if max <= 0 {
            // Fallback: older KVM versions may not report this.
            let nr = self.check_capability(KvmCap::NrVcpus);
            if nr > 0 {
                nr
            } else {
                1
            }
        } else {
            max
        }
    }

    /// Returns the mmap size for vCPU run regions.
    pub fn vcpu_mmap_size(&self) -> usize {
        self.run_size
    }

    /// Get the CPUID entries supported by this host.
    ///
    /// These should be passed to `VcpuFd::set_cpuid2()` for each vCPU
    /// so the guest sees proper CPU feature flags.
    pub fn get_supported_cpuid(&self, max_entries: usize) -> Result<Vec<KvmCpuidEntry2>> {
        let entry_size = std::mem::size_of::<KvmCpuidEntry2>();
        // Header: nent (u32) + padding (u32) = 8 bytes
        let header_size = 8usize;
        let total_size = header_size + max_entries * entry_size;

        let mut buf = vec![0u8; total_size];

        // Write nent to the header.
        // SAFETY: buf is large enough and properly aligned for a u32 write.
        unsafe {
            let nent_ptr = buf.as_mut_ptr().cast::<u32>();
            *nent_ptr = max_entries as u32;
        }

        // SAFETY: KVM_GET_SUPPORTED_CPUID is a valid ioctl on /dev/kvm.
        let ret =
            unsafe { libc::ioctl(self.fd, KVM_GET_SUPPORTED_CPUID, buf.as_mut_ptr()) };
        if ret < 0 {
            return Err(KvmError::GetSupportedCpuid(
                std::io::Error::last_os_error(),
            ));
        }

        // Read back nent (the kernel may have written fewer entries).
        let nent = unsafe { *(buf.as_ptr().cast::<u32>()) } as usize;
        let entries_ptr = unsafe { buf.as_ptr().add(header_size).cast::<KvmCpuidEntry2>() };
        let entries: Vec<KvmCpuidEntry2> =
            (0..nent).map(|i| unsafe { *entries_ptr.add(i) }).collect();

        tracing::debug!(nent, "got supported CPUID entries");
        Ok(entries)
    }
}

impl AsRawFd for Kvm {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for Kvm {
    fn drop(&mut self) {
        // SAFETY: We own this fd (opened in Kvm::open).
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::vcpu::VmExit;

    #[test]
    fn test_kvm_open_and_capabilities() {
        let kvm = Kvm::open().expect("failed to open /dev/kvm");
        let max = kvm.max_vcpus();
        assert!(max > 0, "max_vcpus should be > 0, got {max}");

        // User memory should always be supported.
        let user_mem = kvm.check_capability(KvmCap::UserMemory);
        assert!(user_mem > 0, "KVM_CAP_USER_MEMORY not supported");
    }

    #[test]
    fn test_create_vm_and_vcpu() {
        let kvm = Kvm::open().expect("failed to open /dev/kvm");
        let vm = kvm.create_vm().expect("failed to create VM");

        let _vcpu = vm.create_vcpu(0).expect("failed to create vCPU");
    }

    /// Full pipeline test: open KVM, create VM, create vCPU, load a tiny x86 program,
    /// run it, and verify the exit reasons.
    ///
    /// The program:
    ///   mov al, 0x42   ; B0 42
    ///   out 0x10, al   ; E6 10
    ///   hlt            ; F4
    #[test]
    fn test_run_tiny_guest() {
        let kvm = Kvm::open().expect("failed to open /dev/kvm");
        let vm = kvm.create_vm().expect("failed to create VM");

        // Allocate 4KB of guest memory.
        let mem_size: usize = 4096;
        // SAFETY: We allocate anonymous private memory with mmap.
        let mem_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mem_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        assert_ne!(mem_ptr, libc::MAP_FAILED);

        // Write the guest code at address 0 (which maps to the start of our region).
        let code: &[u8] = &[
            0xB0, 0x42, // mov al, 0x42
            0xE6, 0x10, // out 0x10, al
            0xF4, // hlt
        ];
        // SAFETY: mem_ptr is valid and large enough.
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem_ptr.cast::<u8>(), code.len());
        }

        // Map guest physical address 0 → our mmap'd memory.
        let region = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem_size as u64,
            userspace_addr: mem_ptr as u64,
        };
        vm.set_user_memory_region(&region)
            .expect("failed to set memory region");

        // Create vCPU.
        let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

        // Set up special registers: set CS base to 0 so code runs from phys 0.
        let mut sregs = vcpu.get_sregs().expect("failed to get sregs");
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        vcpu.set_sregs(&sregs).expect("failed to set sregs");

        // Set RIP to 0 (start of our code).
        let mut regs = vcpu.get_regs().expect("failed to get regs");
        regs.rip = 0;
        regs.rflags = 0x2; // Required: bit 1 must be set.
        vcpu.set_regs(&regs).expect("failed to set regs");

        // First run should exit with IoOut (port 0x10, data 0x42).
        let exit1 = vcpu.run().expect("vCPU run failed");
        match exit1 {
            VmExit::IoOut { port, data, size } => {
                assert_eq!(port, 0x10, "expected port 0x10");
                assert_eq!(data, 0x42, "expected data 0x42");
                assert_eq!(size, 1, "expected size 1");
            }
            other => panic!("expected IoOut, got {other:?}"),
        }

        // Second run should exit with Hlt.
        let exit2 = vcpu.run().expect("vCPU run failed");
        match exit2 {
            VmExit::Hlt => {}
            other => panic!("expected Hlt, got {other:?}"),
        }

        // Cleanup.
        // SAFETY: We own the mmap'd region.
        unsafe {
            libc::munmap(mem_ptr, mem_size);
        }
    }
}
