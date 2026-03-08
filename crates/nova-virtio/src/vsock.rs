//! Virtio vsock device (type 19).
//!
//! Provides host-guest communication over AF_VSOCK.

use std::sync::Arc;

use nova_mem::GuestMemoryMmap;

use crate::mmio::VirtioDevice;
use crate::queue::Queue;

/// Virtio vsock device type.
pub const VIRTIO_VSOCK_DEVICE_TYPE: u32 = 19;

/// Vsock config space.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtioVsockConfig {
    /// Guest CID.
    pub guest_cid: u64,
}

/// Virtio vsock device.
pub struct Vsock {
    /// Guest CID (context identifier).
    guest_cid: u64,
    /// Acknowledged features.
    acked_features: u64,
    /// vhost-vsock fd (when using vhost backend).
    vhost_fd: Option<std::os::unix::io::RawFd>,
}

impl Vsock {
    /// Create a new vsock device with the given guest CID.
    pub fn new(guest_cid: u64) -> Self {
        Self {
            guest_cid,
            acked_features: 0,
            vhost_fd: None,
        }
    }

    /// Set the vhost-vsock backend fd.
    pub fn set_vhost_fd(&mut self, fd: std::os::unix::io::RawFd) {
        self.vhost_fd = Some(fd);
    }

    /// Returns the guest CID.
    pub fn guest_cid(&self) -> u64 {
        self.guest_cid
    }
}

impl VirtioDevice for Vsock {
    fn device_type(&self) -> u32 {
        VIRTIO_VSOCK_DEVICE_TYPE
    }

    fn num_queues(&self) -> usize {
        3 // rx, tx, event
    }

    fn device_features(&self, _page: u32) -> u32 {
        0
    }

    fn ack_features(&mut self, page: u32, value: u32) {
        self.acked_features |= (value as u64) << (page * 32);
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config = VirtioVsockConfig {
            guest_cid: self.guest_cid,
        };
        let config_bytes = unsafe {
            std::slice::from_raw_parts(
                &config as *const VirtioVsockConfig as *const u8,
                std::mem::size_of::<VirtioVsockConfig>(),
            )
        };
        let offset = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config_bytes.get(offset + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(&mut self, _queues: Vec<Queue>, _mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()> {
        tracing::info!(guest_cid = self.guest_cid, "virtio-vsock activated");
        Ok(())
    }

    fn queue_notify(&mut self, _queue_index: u16, _mem: &GuestMemoryMmap) {
        tracing::debug!("vsock queue notified");
    }

    fn reset(&mut self) {
        self.acked_features = 0;
    }
}
