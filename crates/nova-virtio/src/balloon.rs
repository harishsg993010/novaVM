//! Virtio balloon device (type 5).
//!
//! Allows the host to reclaim memory from the guest.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use nova_mem::GuestMemoryMmap;

use crate::mmio::VirtioDevice;
use crate::queue::Queue;

/// Virtio balloon device type.
pub const VIRTIO_BALLOON_DEVICE_TYPE: u32 = 5;

/// Balloon config space.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtioBalloonConfig {
    /// Number of pages the host wants the guest to give up.
    pub num_pages: u32,
    /// Number of pages the guest has actually given up.
    pub actual: u32,
}

/// Virtio balloon device.
pub struct Balloon {
    /// Target balloon size in pages.
    target_pages: AtomicU32,
    /// Actual inflated pages (reported by guest).
    actual_pages: AtomicU32,
    /// Acknowledged features.
    acked_features: u64,
}

impl Balloon {
    /// Create a new balloon device.
    pub fn new() -> Self {
        Self {
            target_pages: AtomicU32::new(0),
            actual_pages: AtomicU32::new(0),
            acked_features: 0,
        }
    }

    /// Set the target balloon size in 4KiB pages.
    pub fn set_target_pages(&self, pages: u32) {
        self.target_pages.store(pages, Ordering::Release);
    }

    /// Get the actual number of reclaimed pages.
    pub fn actual_pages(&self) -> u32 {
        self.actual_pages.load(Ordering::Acquire)
    }
}

impl Default for Balloon {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioDevice for Balloon {
    fn device_type(&self) -> u32 {
        VIRTIO_BALLOON_DEVICE_TYPE
    }

    fn num_queues(&self) -> usize {
        2 // inflate (0) and deflate (1)
    }

    fn device_features(&self, _page: u32) -> u32 {
        0
    }

    fn ack_features(&mut self, page: u32, value: u32) {
        self.acked_features |= (value as u64) << (page * 32);
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config = VirtioBalloonConfig {
            num_pages: self.target_pages.load(Ordering::Acquire),
            actual: self.actual_pages.load(Ordering::Acquire),
        };
        let config_bytes = unsafe {
            std::slice::from_raw_parts(
                &config as *const VirtioBalloonConfig as *const u8,
                std::mem::size_of::<VirtioBalloonConfig>(),
            )
        };
        let offset = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config_bytes.get(offset + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        // Guest writes actual pages at offset 4.
        if offset == 4 && data.len() >= 4 {
            let actual = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            self.actual_pages.store(actual, Ordering::Release);
        }
    }

    fn activate(&mut self, _queues: Vec<Queue>, _mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()> {
        tracing::info!("virtio-balloon activated");
        Ok(())
    }

    fn queue_notify(&mut self, _queue_index: u16, _mem: &GuestMemoryMmap) {
        tracing::debug!("balloon queue notified");
    }

    fn reset(&mut self) {
        self.acked_features = 0;
        self.target_pages.store(0, Ordering::Release);
        self.actual_pages.store(0, Ordering::Release);
    }
}
