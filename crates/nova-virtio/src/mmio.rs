//! Virtio MMIO transport (virtio-mmio v2).
//!
//! Register layout per virtio spec section 4.2.2.

use std::sync::Arc;

use nova_mem::GuestMemoryMmap;

use crate::queue::Queue;

/// Virtio MMIO magic value: "virt".
pub const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;

/// Virtio MMIO version 2.
pub const VIRTIO_MMIO_VERSION: u32 = 2;

/// VIRTIO_F_VERSION_1 (bit 32) — mandatory for MMIO v2 devices.
/// Sits on feature page 1, bit 0.
const VIRTIO_F_VERSION_1_PAGE1: u32 = 1;

/// Virtio MMIO register offsets.
pub const MMIO_MAGIC_VALUE: u64 = 0x000;
pub const MMIO_VERSION: u64 = 0x004;
pub const MMIO_DEVICE_ID: u64 = 0x008;
pub const MMIO_VENDOR_ID: u64 = 0x00C;
pub const MMIO_DEVICE_FEATURES: u64 = 0x010;
pub const MMIO_DEVICE_FEATURES_SEL: u64 = 0x014;
pub const MMIO_DRIVER_FEATURES: u64 = 0x020;
pub const MMIO_DRIVER_FEATURES_SEL: u64 = 0x024;
pub const MMIO_QUEUE_SEL: u64 = 0x030;
pub const MMIO_QUEUE_NUM_MAX: u64 = 0x034;
pub const MMIO_QUEUE_NUM: u64 = 0x038;
pub const MMIO_QUEUE_READY: u64 = 0x044;
pub const MMIO_QUEUE_NOTIFY: u64 = 0x050;
pub const MMIO_INTERRUPT_STATUS: u64 = 0x060;
pub const MMIO_INTERRUPT_ACK: u64 = 0x064;
pub const MMIO_STATUS: u64 = 0x070;
pub const MMIO_QUEUE_DESC_LOW: u64 = 0x080;
pub const MMIO_QUEUE_DESC_HIGH: u64 = 0x084;
pub const MMIO_QUEUE_AVAIL_LOW: u64 = 0x090;
pub const MMIO_QUEUE_AVAIL_HIGH: u64 = 0x094;
pub const MMIO_QUEUE_USED_LOW: u64 = 0x0A0;
pub const MMIO_QUEUE_USED_HIGH: u64 = 0x0A4;
pub const MMIO_CONFIG_GENERATION: u64 = 0x0FC;
pub const MMIO_CONFIG_SPACE: u64 = 0x100;

/// Device status bits.
pub const STATUS_ACKNOWLEDGE: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_FEATURES_OK: u32 = 8;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FAILED: u32 = 128;

/// Trait for virtio devices behind MMIO transport.
pub trait VirtioDevice: Send {
    /// Device type ID (1=net, 2=blk, 3=console, etc.)
    fn device_type(&self) -> u32;

    /// Number of queues this device uses.
    fn num_queues(&self) -> usize;

    /// Device feature bits (page 0 and 1).
    fn device_features(&self, page: u32) -> u32;

    /// Called when the driver accepts features.
    fn ack_features(&mut self, page: u32, value: u32);

    /// Read from the device-specific config space.
    fn read_config(&self, offset: u64, data: &mut [u8]);

    /// Write to the device-specific config space.
    fn write_config(&mut self, offset: u64, data: &[u8]);

    /// Activate the device after DRIVER_OK status.
    fn activate(&mut self, queues: Vec<Queue>, mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()>;

    /// Handle a queue notification (guest kicked a queue).
    fn queue_notify(&mut self, queue_index: u16, mem: &GuestMemoryMmap);

    /// Reset the device to initial state.
    fn reset(&mut self);

    /// Poll for async I/O (e.g., RX packets from TAP). Returns true if interrupt needed.
    fn poll(&mut self) -> bool {
        false
    }

    /// Set the TAP file descriptor for network devices. No-op for non-net devices.
    fn set_tap_fd(&mut self, _fd: std::os::unix::io::RawFd) {}

    /// Get activated queue references for snapshotting. Returns None if not activated.
    fn activated_queues(&self) -> Option<&[crate::queue::Queue]> {
        None
    }
}

/// MMIO transport state for a single virtio device.
pub struct MmioTransport {
    /// The underlying virtio device.
    device: Box<dyn VirtioDevice>,
    /// Queues managed by the transport.
    queues: Vec<Queue>,
    /// Currently selected queue index.
    queue_sel: u32,
    /// Device status register.
    status: u32,
    /// Interrupt status.
    interrupt_status: u32,
    /// Feature selection page.
    device_features_sel: u32,
    /// Driver feature selection page.
    driver_features_sel: u32,
    /// Driver-acknowledged feature bits.
    driver_features: [u32; 2],
    /// Config generation counter.
    config_generation: u32,
    /// Whether the device has been activated.
    activated: bool,
    /// Guest memory reference (set before activation).
    guest_memory: Option<Arc<GuestMemoryMmap>>,
}

impl MmioTransport {
    /// Create a new MMIO transport wrapping the given device.
    pub fn new(device: Box<dyn VirtioDevice>) -> Self {
        let num_queues = device.num_queues();
        let queues = (0..num_queues).map(|_| Queue::new(256)).collect();
        Self {
            device,
            queues,
            queue_sel: 0,
            status: 0,
            interrupt_status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: [0; 2],
            config_generation: 0,
            activated: false,
            guest_memory: None,
        }
    }

    /// Handle a 32-bit MMIO read at `offset`.
    pub fn read(&self, offset: u64) -> u32 {
        match offset {
            MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC,
            MMIO_VERSION => VIRTIO_MMIO_VERSION,
            MMIO_DEVICE_ID => self.device.device_type(),
            MMIO_VENDOR_ID => 0x4E6F_7661, // "Nova"
            MMIO_DEVICE_FEATURES => {
                let mut features = self.device.device_features(self.device_features_sel);
                // MMIO v2 mandates VIRTIO_F_VERSION_1 (bit 32 = page 1, bit 0).
                if self.device_features_sel == 1 {
                    features |= VIRTIO_F_VERSION_1_PAGE1;
                }
                features
            }
            MMIO_QUEUE_NUM_MAX => {
                if let Some(q) = self.queues.get(self.queue_sel as usize) {
                    q.max_size as u32
                } else {
                    0
                }
            }
            MMIO_QUEUE_READY => {
                if let Some(q) = self.queues.get(self.queue_sel as usize) {
                    u32::from(q.ready)
                } else {
                    0
                }
            }
            MMIO_INTERRUPT_STATUS => {
                if self.interrupt_status != 0 {
                    tracing::info!(status = self.interrupt_status, device = self.device.device_type(), "MMIO read InterruptStatus (non-zero)");
                }
                self.interrupt_status
            }
            MMIO_STATUS => self.status,
            MMIO_CONFIG_GENERATION => self.config_generation,
            o if o >= MMIO_CONFIG_SPACE => {
                let mut data = [0u8; 4];
                self.device.read_config(o - MMIO_CONFIG_SPACE, &mut data);
                u32::from_le_bytes(data)
            }
            _ => {
                tracing::warn!(offset, "unhandled MMIO read");
                0
            }
        }
    }

    /// Handle a 32-bit MMIO write at `offset`.
    pub fn write(&mut self, offset: u64, value: u32) {
        match offset {
            MMIO_DEVICE_FEATURES_SEL => self.device_features_sel = value,
            MMIO_DRIVER_FEATURES => {
                let page = self.driver_features_sel as usize;
                if page < 2 {
                    self.driver_features[page] = value;
                    self.device.ack_features(page as u32, value);
                }
            }
            MMIO_DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            MMIO_QUEUE_SEL => self.queue_sel = value,
            MMIO_QUEUE_NUM => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.size = value as u16;
                }
            }
            MMIO_QUEUE_READY => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.ready = value == 1;
                }
            }
            MMIO_QUEUE_NOTIFY => {
                tracing::info!(queue = value, has_mem = self.guest_memory.is_some(), "MMIO QueueNotify write");
                if let Some(ref mem) = self.guest_memory {
                    self.device.queue_notify(value as u16, mem);
                }
            }
            MMIO_INTERRUPT_ACK => {
                tracing::info!(ack = value, device = self.device.device_type(), "MMIO InterruptAck");
                self.interrupt_status &= !value;
            }
            MMIO_STATUS => {
                self.status = value;
                if value == 0 {
                    // Reset.
                    self.device.reset();
                    self.activated = false;
                } else if value & STATUS_DRIVER_OK != 0 && !self.activated {
                    // Activate the device.
                    let queues: Vec<Queue> = self.queues.drain(..).collect();
                    if let Some(mem) = self.guest_memory.clone() {
                        if let Err(e) = self.device.activate(queues, mem) {
                            tracing::error!(error = %e, "device activation failed");
                            self.status |= STATUS_FAILED;
                        } else {
                            self.activated = true;
                        }
                    } else {
                        tracing::error!("guest memory not set, cannot activate device");
                        self.status |= STATUS_FAILED;
                    }
                }
            }
            MMIO_QUEUE_DESC_LOW => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    let addr = q.desc_table.raw();
                    q.desc_table =
                        nova_mem::GuestAddress::new((addr & 0xFFFF_FFFF_0000_0000) | value as u64);
                }
            }
            MMIO_QUEUE_DESC_HIGH => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    let addr = q.desc_table.raw();
                    q.desc_table = nova_mem::GuestAddress::new(
                        (addr & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32),
                    );
                }
            }
            MMIO_QUEUE_AVAIL_LOW => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    let addr = q.avail_ring.raw();
                    q.avail_ring =
                        nova_mem::GuestAddress::new((addr & 0xFFFF_FFFF_0000_0000) | value as u64);
                }
            }
            MMIO_QUEUE_AVAIL_HIGH => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    let addr = q.avail_ring.raw();
                    q.avail_ring = nova_mem::GuestAddress::new(
                        (addr & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32),
                    );
                }
            }
            MMIO_QUEUE_USED_LOW => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    let addr = q.used_ring.raw();
                    q.used_ring =
                        nova_mem::GuestAddress::new((addr & 0xFFFF_FFFF_0000_0000) | value as u64);
                }
            }
            MMIO_QUEUE_USED_HIGH => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    let addr = q.used_ring.raw();
                    q.used_ring = nova_mem::GuestAddress::new(
                        (addr & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32),
                    );
                }
            }
            o if o >= MMIO_CONFIG_SPACE => {
                let data = value.to_le_bytes();
                self.device.write_config(o - MMIO_CONFIG_SPACE, &data);
                self.config_generation = self.config_generation.wrapping_add(1);
            }
            _ => {
                tracing::warn!(offset, value, "unhandled MMIO write");
            }
        }
    }

    /// Raise an interrupt (used buffer notification).
    pub fn raise_interrupt(&mut self) {
        self.interrupt_status |= 1;
    }

    /// Returns whether the device has been activated.
    pub fn is_activated(&self) -> bool {
        self.activated
    }

    /// Returns the device type.
    pub fn device_type(&self) -> u32 {
        self.device.device_type()
    }

    /// Set the TAP file descriptor on the underlying device (for net devices).
    pub fn set_tap_fd(&mut self, fd: std::os::unix::io::RawFd) {
        self.device.set_tap_fd(fd);
    }

    /// Poll the device for async I/O. Sets interrupt status if device signals.
    pub fn poll(&mut self) -> bool {
        if self.activated && self.device.poll() {
            self.interrupt_status |= 1;
            true
        } else {
            false
        }
    }

    /// Set the guest memory reference for this transport.
    /// Must be called before device activation.
    pub fn set_guest_memory(&mut self, mem: Arc<GuestMemoryMmap>) {
        self.guest_memory = Some(mem);
    }

    /// Force-activate the transport with pre-configured queues.
    /// Used for snapshot restore where the guest already negotiated the device.
    pub fn force_activate(&mut self, queues: Vec<Queue>) {
        if let Some(mem) = self.guest_memory.clone() {
            if let Err(e) = self.device.activate(queues, mem) {
                tracing::error!(error = %e, "force-activate failed");
            } else {
                self.status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK;
                self.activated = true;
                tracing::info!(device_type = self.device.device_type(), "force-activated device from snapshot");
            }
        } else {
            tracing::error!("force_activate: no guest memory set");
        }
    }

    /// Get the transport status.
    pub fn transport_status(&self) -> u32 {
        self.status
    }

    /// Get queue snapshots from the activated device (for snapshot save).
    pub fn get_activated_queue_states(&self) -> Option<&[Queue]> {
        self.device.activated_queues()
    }
}
