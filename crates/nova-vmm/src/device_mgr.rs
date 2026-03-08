//! MMIO bus for dispatching device accesses.

use nova_kvm::kvm_bindings::{KvmIrqLevel, KVM_IRQ_LINE};
use nova_virtio::mmio::MmioTransport;

/// A device registered on the MMIO bus.
struct MmioDevice {
    /// Base address of the MMIO region.
    base: u64,
    /// Size of the MMIO region.
    size: u64,
    /// The MMIO transport for this device.
    transport: MmioTransport,
    /// IRQ number for interrupt injection (if any).
    irq: Option<u32>,
}

/// MMIO bus that dispatches reads/writes to the correct device.
pub struct MmioBus {
    devices: Vec<MmioDevice>,
    /// Raw VM fd for KVM_IRQ_LINE injection.
    vm_fd: Option<i32>,
}

impl MmioBus {
    /// Create a new empty MMIO bus.
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            vm_fd: None,
        }
    }

    /// Set the VM file descriptor for IRQ injection.
    pub fn set_vm_fd(&mut self, fd: i32) {
        self.vm_fd = Some(fd);
    }

    /// Register a device at the given base address, region size, and optional IRQ.
    pub fn register(&mut self, base: u64, size: u64, transport: MmioTransport, irq: Option<u32>) {
        tracing::info!(
            base = format!("{base:#x}"),
            size,
            device_type = transport.device_type(),
            irq = ?irq,
            "registered MMIO device"
        );
        self.devices.push(MmioDevice {
            base,
            size,
            transport,
            irq,
        });
    }

    /// Handle an MMIO read at the given absolute address.
    pub fn read(&self, addr: u64, size: u32) -> Option<u32> {
        for dev in &self.devices {
            if addr >= dev.base && addr < dev.base + dev.size {
                let offset = addr - dev.base;
                match size {
                    4 => return Some(dev.transport.read(offset)),
                    // Sub-word reads: read the aligned u32, extract the byte/halfword.
                    1 => {
                        let aligned = offset & !0x3;
                        let byte_idx = (offset & 0x3) as usize;
                        let word = dev.transport.read(aligned);
                        return Some((word >> (byte_idx * 8)) & 0xFF);
                    }
                    2 => {
                        let aligned = offset & !0x3;
                        let hw_idx = ((offset & 0x3) >> 1) as usize;
                        let word = dev.transport.read(aligned);
                        return Some((word >> (hw_idx * 16)) & 0xFFFF);
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Handle an MMIO write at the given absolute address.
    pub fn write(&mut self, addr: u64, size: u32, value: u32) -> bool {
        for dev in &mut self.devices {
            if addr >= dev.base && addr < dev.base + dev.size {
                let offset = addr - dev.base;
                match size {
                    4 => {
                        dev.transport.write(offset, value);
                        return true;
                    }
                    // Sub-word writes: the transport only takes u32 writes,
                    // so we pass the value directly (transport handles it).
                    1 | 2 => {
                        dev.transport.write(offset, value);
                        return true;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// Returns the number of registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Returns the raw VM fd (for IRQ injection).
    pub fn vm_fd(&self) -> Option<i32> {
        self.vm_fd
    }

    /// Poll all devices for async I/O (e.g., TAP RX packets).
    /// If a device signals, inject a KVM IRQ via KVM_IRQ_LINE.
    pub fn poll_devices(&mut self) -> bool {
        let vm_fd = match self.vm_fd {
            Some(fd) => fd,
            None => return false,
        };
        let mut any = false;
        for dev in &mut self.devices {
            if dev.transport.poll() {
                any = true;
                // Inject edge-triggered IRQ into the guest via KVM.
                if let Some(irq) = dev.irq {
                    let level_high = KvmIrqLevel { irq, level: 1 };
                    let level_low = KvmIrqLevel { irq, level: 0 };
                    let ret_hi;
                    let ret_lo;
                    unsafe {
                        ret_hi = libc::ioctl(vm_fd, KVM_IRQ_LINE, &level_high as *const KvmIrqLevel);
                        ret_lo = libc::ioctl(vm_fd, KVM_IRQ_LINE, &level_low as *const KvmIrqLevel);
                    }
                    tracing::info!(irq, ret_hi, ret_lo, "injected IRQ via KVM_IRQ_LINE");
                }
            }
        }
        any
    }

    /// Iterate over device (base, size) info for kernel command line construction.
    pub fn device_info(&self) -> Vec<(u64, u64, u32)> {
        self.devices
            .iter()
            .map(|d| (d.base, d.size, d.transport.device_type()))
            .collect()
    }

    /// Open a TAP device and set its fd on the net device (device_type == 1).
    /// Returns true if the TAP was successfully opened and assigned.
    pub fn open_tap_for_net(&mut self, tap_name: &str) -> bool {
        match nova_virtio::tap::Tap::open(tap_name) {
            Ok(tap) => {
                if let Err(e) = tap.set_nonblocking() {
                    tracing::warn!(error = %e, "failed to set TAP non-blocking");
                }
                let fd = tap.fd();
                // Find the net device (device_type == 1) and set the TAP fd.
                for dev in &mut self.devices {
                    if dev.transport.device_type() == 1 {
                        dev.transport.set_tap_fd(fd);
                        tracing::info!(tap = %tap_name, fd, "TAP fd assigned to net device");
                        std::mem::forget(tap);
                        return true;
                    }
                }
                tracing::warn!("no net device found on MMIO bus to assign TAP fd");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, tap = %tap_name, "failed to open TAP device");
                false
            }
        }
    }

    /// Get virtio device queue states for snapshot save.
    /// Returns (device_type, Vec<(max_size, size, ready, desc, avail, used, next_avail, next_used)>).
    pub fn snapshot_virtio_queues(&self) -> Vec<(u32, Vec<(u16, u16, bool, u64, u64, u64, u16, u16)>)> {
        let mut result = Vec::new();
        for dev in &self.devices {
            let device_type = dev.transport.device_type();
            if let Some(queues) = dev.transport.get_activated_queue_states() {
                let queue_states: Vec<_> = queues.iter().map(|q| {
                    (
                        q.max_size,
                        q.size,
                        q.ready,
                        q.desc_table.raw(),
                        q.avail_ring.raw(),
                        q.used_ring.raw(),
                        q.next_avail(),
                        q.next_used(),
                    )
                }).collect();
                result.push((device_type, queue_states));
            }
        }
        result
    }

    /// Force-activate a virtio device with saved queue state (snapshot restore).
    pub fn force_activate_device(&mut self, device_type: u32, queues: Vec<nova_virtio::queue::Queue>) {
        for dev in &mut self.devices {
            if dev.transport.device_type() == device_type {
                dev.transport.force_activate(queues);
                return;
            }
        }
        tracing::warn!(device_type, "force_activate_device: device not found");
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}
