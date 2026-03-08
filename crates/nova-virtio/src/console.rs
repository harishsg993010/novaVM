//! Virtio console device (type 3).
//!
//! Provides a simple serial-like console to the guest via virtio.
//! The guest writes to the transmit queue; we read and forward to a callback.
//! The receive queue allows injecting data into the guest.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use nova_mem::GuestMemoryMmap;

use crate::mmio::VirtioDevice;
use crate::queue::Queue;

/// Virtio console device type.
pub const VIRTIO_CONSOLE_DEVICE_TYPE: u32 = 3;

/// Feature bits for virtio-console.
pub const VIRTIO_CONSOLE_F_SIZE: u64 = 1 << 0;
pub const VIRTIO_CONSOLE_F_MULTIPORT: u64 = 1 << 1;

/// Output sink for console data written by the guest.
pub type ConsoleOutput = Arc<Mutex<VecDeque<u8>>>;

/// Virtio console device.
pub struct Console {
    /// Acknowledged features.
    acked_features: u64,
    /// Buffer for output from the guest.
    output: ConsoleOutput,
    /// Console dimensions (cols, rows).
    cols: u16,
    rows: u16,
}

impl Console {
    /// Create a new console device.
    pub fn new(output: ConsoleOutput) -> Self {
        Self {
            acked_features: 0,
            output,
            cols: 80,
            rows: 25,
        }
    }

    /// Get a handle to the output buffer.
    pub fn output_handle(&self) -> ConsoleOutput {
        Arc::clone(&self.output)
    }

    /// Set console dimensions.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }
}

impl VirtioDevice for Console {
    fn device_type(&self) -> u32 {
        VIRTIO_CONSOLE_DEVICE_TYPE
    }

    fn num_queues(&self) -> usize {
        2 // receiveq (0) and transmitq (1)
    }

    fn device_features(&self, page: u32) -> u32 {
        match page {
            0 => VIRTIO_CONSOLE_F_SIZE as u32,
            _ => 0,
        }
    }

    fn ack_features(&mut self, page: u32, value: u32) {
        self.acked_features |= (value as u64) << (page * 32);
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // Config space: cols (u16 at 0), rows (u16 at 2), max_nr_ports (u32 at 4).
        let config = [
            self.cols.to_le_bytes()[0],
            self.cols.to_le_bytes()[1],
            self.rows.to_le_bytes()[0],
            self.rows.to_le_bytes()[1],
            1,
            0,
            0,
            0, // max_nr_ports = 1
        ];
        let offset = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config.get(offset + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        // Console config is mostly read-only; ignore writes.
        tracing::debug!(offset, len = data.len(), "console config write (ignored)");
    }

    fn activate(&mut self, _queues: Vec<Queue>, _mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()> {
        tracing::info!("virtio-console activated");
        Ok(())
    }

    fn queue_notify(&mut self, queue_index: u16, _mem: &GuestMemoryMmap) {
        if queue_index == 1 {
            // Transmit queue — in a full implementation we'd read descriptors
            // from guest memory. Here we just note the notification.
            tracing::debug!("console transmit queue notified");
        }
    }

    fn reset(&mut self) {
        self.acked_features = 0;
        if let Ok(mut out) = self.output.lock() {
            out.clear();
        }
    }
}
