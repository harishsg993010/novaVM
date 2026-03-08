//! Virtio filesystem device (type 26, virtio-fs).
//!
//! Exposes host directories to the guest via FUSE-over-virtio.

use std::path::PathBuf;
use std::sync::Arc;

use nova_mem::GuestMemoryMmap;

use crate::mmio::VirtioDevice;
use crate::queue::Queue;

/// Virtio fs device type.
pub const VIRTIO_FS_DEVICE_TYPE: u32 = 26;

/// Virtio fs config space.
#[repr(C)]
#[derive(Clone)]
pub struct VirtioFsConfig {
    /// Filesystem tag (up to 36 bytes, null-padded).
    pub tag: [u8; 36],
    /// Number of request queues.
    pub num_request_queues: u32,
}

/// Virtio fs device.
pub struct Fs {
    /// Mount tag visible to the guest.
    tag: String,
    /// Shared directory path on the host.
    shared_dir: PathBuf,
    /// Number of request queues.
    num_request_queues: u32,
    /// Acknowledged features.
    acked_features: u64,
}

impl Fs {
    /// Create a new virtio-fs device.
    pub fn new(tag: String, shared_dir: PathBuf, num_request_queues: u32) -> Self {
        Self {
            tag,
            shared_dir,
            num_request_queues: num_request_queues.max(1),
            acked_features: 0,
        }
    }

    /// Returns the mount tag.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the shared directory path.
    pub fn shared_dir(&self) -> &PathBuf {
        &self.shared_dir
    }
}

impl VirtioDevice for Fs {
    fn device_type(&self) -> u32 {
        VIRTIO_FS_DEVICE_TYPE
    }

    fn num_queues(&self) -> usize {
        // 1 hiprio queue + N request queues
        1 + self.num_request_queues as usize
    }

    fn device_features(&self, _page: u32) -> u32 {
        0
    }

    fn ack_features(&mut self, page: u32, value: u32) {
        self.acked_features |= (value as u64) << (page * 32);
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut config = VirtioFsConfig {
            tag: [0u8; 36],
            num_request_queues: self.num_request_queues,
        };
        let tag_bytes = self.tag.as_bytes();
        let copy_len = tag_bytes.len().min(36);
        config.tag[..copy_len].copy_from_slice(&tag_bytes[..copy_len]);

        let config_bytes = unsafe {
            std::slice::from_raw_parts(
                &config as *const VirtioFsConfig as *const u8,
                std::mem::size_of::<VirtioFsConfig>(),
            )
        };
        let offset = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config_bytes.get(offset + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(&mut self, _queues: Vec<Queue>, _mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()> {
        tracing::info!(
            tag = %self.tag,
            shared_dir = %self.shared_dir.display(),
            "virtio-fs activated"
        );
        Ok(())
    }

    fn queue_notify(&mut self, _queue_index: u16, _mem: &GuestMemoryMmap) {
        tracing::debug!("fs queue notified");
    }

    fn reset(&mut self) {
        self.acked_features = 0;
    }
}
