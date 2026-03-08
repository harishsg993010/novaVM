//! Virtio block device (type 2).
//!
//! Provides a virtual block device backed by a file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

use nova_mem::GuestMemoryMmap;

use crate::mmio::VirtioDevice;
use crate::queue::Queue;

/// Virtio block device type.
pub const VIRTIO_BLK_DEVICE_TYPE: u32 = 2;

/// Virtio block request types.
pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub const VIRTIO_BLK_T_GET_ID: u32 = 8;

/// Feature bits.
pub const VIRTIO_BLK_F_SIZE_MAX: u64 = 1 << 1;
pub const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
pub const VIRTIO_BLK_F_RO: u64 = 1 << 5;
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

/// Block device config space.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtioBlkConfig {
    pub capacity: u64, // in 512-byte sectors
    pub size_max: u32,
    pub seg_max: u32,
    pub blk_size: u32,
}

/// Virtio block device.
pub struct Block {
    /// Path to the backing file.
    path: PathBuf,
    /// The backing file.
    file: Option<File>,
    /// Read-only flag.
    read_only: bool,
    /// Disk size in 512-byte sectors.
    capacity: u64,
    /// Acknowledged features.
    acked_features: u64,
}

impl Block {
    /// Create a new block device backed by the file at `path`.
    pub fn new(path: PathBuf, read_only: bool) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(&path)?;
        let metadata = file.metadata()?;
        let capacity = metadata.len() / 512;

        Ok(Self {
            path,
            file: Some(file),
            read_only,
            capacity,
            acked_features: 0,
        })
    }

    /// Read sectors from the backing file.
    pub fn read_sectors(&mut self, sector: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(ref mut file) = self.file {
            file.seek(SeekFrom::Start(sector * 512))?;
            file.read(buf)
        } else {
            Ok(0)
        }
    }

    /// Write sectors to the backing file.
    pub fn write_sectors(&mut self, sector: u64, buf: &[u8]) -> std::io::Result<usize> {
        if self.read_only {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "block device is read-only",
            ));
        }
        if let Some(ref mut file) = self.file {
            file.seek(SeekFrom::Start(sector * 512))?;
            file.write(buf)
        } else {
            Ok(0)
        }
    }

    /// Flush the backing file.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if let Some(ref mut file) = self.file {
            file.sync_all()
        } else {
            Ok(())
        }
    }

    /// Returns the disk path.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl VirtioDevice for Block {
    fn device_type(&self) -> u32 {
        VIRTIO_BLK_DEVICE_TYPE
    }

    fn num_queues(&self) -> usize {
        1
    }

    fn device_features(&self, page: u32) -> u32 {
        let mut features = VIRTIO_BLK_F_FLUSH | VIRTIO_BLK_F_BLK_SIZE;
        if self.read_only {
            features |= VIRTIO_BLK_F_RO;
        }
        match page {
            0 => features as u32,
            _ => 0,
        }
    }

    fn ack_features(&mut self, page: u32, value: u32) {
        self.acked_features |= (value as u64) << (page * 32);
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config = VirtioBlkConfig {
            capacity: self.capacity,
            size_max: 1 << 20, // 1 MiB
            seg_max: 128,
            blk_size: 512,
        };
        let config_bytes = unsafe {
            std::slice::from_raw_parts(
                &config as *const VirtioBlkConfig as *const u8,
                std::mem::size_of::<VirtioBlkConfig>(),
            )
        };
        let offset = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config_bytes.get(offset + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Block config is read-only.
    }

    fn activate(&mut self, _queues: Vec<Queue>, _mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()> {
        tracing::info!(path = %self.path.display(), capacity = self.capacity, "virtio-blk activated");
        Ok(())
    }

    fn queue_notify(&mut self, _queue_index: u16, _mem: &GuestMemoryMmap) {
        tracing::debug!("blk queue notified");
    }

    fn reset(&mut self) {
        self.acked_features = 0;
    }
}
