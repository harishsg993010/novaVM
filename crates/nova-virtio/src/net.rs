//! Virtio network device (type 1).
//!
//! Provides a virtual NIC backed by a TAP device with real TX/RX packet I/O.

use std::os::unix::io::RawFd;
use std::sync::Arc;

use nova_mem::{GuestAddress, GuestMemoryMmap};

use crate::mmio::VirtioDevice;
use crate::queue::Queue;

/// Virtio net device type.
pub const VIRTIO_NET_DEVICE_TYPE: u32 = 1;

/// Feature bits.
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
pub const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;

/// Maximum ethernet frame size (MTU 1500 + headers).
const MAX_FRAME_SIZE: usize = 1514;

/// Size of the virtio-net header for virtio 1.0 (VIRTIO_F_VERSION_1).
///
/// The v1 header has an additional `num_buffers` field compared to the legacy
/// 10-byte header. Since we mandate VIRTIO_F_VERSION_1 (MMIO v2), the guest
/// driver always uses this 12-byte format.
const VNET_HDR_SIZE: usize = 12;

/// Virtio net header v1 (with num_buffers for VIRTIO_F_VERSION_1).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct VirtioNetHdr {
    flags: u8,
    gso_type: u8,
    hdr_len: u16,
    gso_size: u16,
    csum_start: u16,
    csum_offset: u16,
    num_buffers: u16,
}

/// Virtio net config space.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioNetConfig {
    pub mac: [u8; 6],
    pub status: u16,
}

impl Default for VirtioNetConfig {
    fn default() -> Self {
        Self {
            // Default MAC: locally administered, unicast.
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            status: 1, // VIRTIO_NET_S_LINK_UP
        }
    }
}

/// Virtio network device.
pub struct Net {
    /// TAP file descriptor.
    tap_fd: Option<RawFd>,
    /// TAP device name.
    tap_name: String,
    /// Network config.
    config: VirtioNetConfig,
    /// Acknowledged features.
    acked_features: u64,
    /// Queues stored after activation (rx=0, tx=1).
    queues: Option<Vec<Queue>>,
    /// Guest memory reference stored after activation.
    mem: Option<Arc<GuestMemoryMmap>>,
    /// Eventfd for IRQ injection (registered with KVM_IRQFD).
    irq_fd: Option<RawFd>,
}

impl Net {
    /// Create a new net device with the given TAP name and MAC address.
    pub fn new(tap_name: String, mac: [u8; 6]) -> Self {
        Self {
            tap_fd: None,
            tap_name,
            config: VirtioNetConfig {
                mac,
                ..Default::default()
            },
            acked_features: 0,
            queues: None,
            mem: None,
            irq_fd: None,
        }
    }

    /// Set the TAP file descriptor (after opening).
    pub fn set_tap_fd(&mut self, fd: RawFd) {
        self.tap_fd = Some(fd);
    }

    /// Returns the TAP device name.
    pub fn tap_name(&self) -> &str {
        &self.tap_name
    }

    /// Returns the MAC address.
    pub fn mac(&self) -> &[u8; 6] {
        &self.config.mac
    }

    /// Set the eventfd for IRQ injection (registered with KVM_IRQFD).
    pub fn set_irq_fd(&mut self, fd: RawFd) {
        self.irq_fd = Some(fd);
    }

    /// Signal the IRQ eventfd to inject an interrupt into the guest.
    fn signal_irq(&self) {
        if let Some(fd) = self.irq_fd {
            let val: u64 = 1;
            unsafe {
                libc::write(fd, &val as *const u64 as *const libc::c_void, 8);
            }
        }
    }

    /// Process the TX queue: read packets from guest, write to TAP.
    ///
    /// Returns the number of packets transmitted.
    pub fn process_tx(&mut self) -> usize {
        let tap_fd = match self.tap_fd {
            Some(fd) => fd,
            None => {
                tracing::warn!("process_tx: no tap_fd");
                return 0;
            }
        };
        let (queues, mem) = match (self.queues.as_mut(), self.mem.as_ref()) {
            (Some(q), Some(m)) => (q, m),
            _ => {
                tracing::warn!("process_tx: no queues or mem");
                return 0;
            }
        };

        let tx_queue = match queues.get_mut(1) {
            Some(q) => q,
            None => {
                tracing::warn!("process_tx: no TX queue");
                return 0;
            }
        };

        let mut tx_count = 0;

        loop {
            let chain = match tx_queue.pop(mem) {
                Ok(Some(c)) => {
                    tracing::info!(head = c.head_index, "TX: popped descriptor chain");
                    c
                }
                Ok(None) => {
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "TX: queue pop error");
                    break;
                }
            };

            // Collect the frame from readable descriptors, skipping the vnet header.
            let mut frame = Vec::with_capacity(MAX_FRAME_SIZE);
            let mut skip = VNET_HDR_SIZE;

            for desc in chain.readable() {
                let len = desc.len as usize;
                if skip >= len {
                    skip -= len;
                    continue;
                }

                let start = desc.addr + skip as u64;
                let read_len = len - skip;
                let mut buf = vec![0u8; read_len];
                if mem.read_slice(GuestAddress::new(start), &mut buf).is_ok() {
                    frame.extend_from_slice(&buf);
                }
                skip = 0;
            }

            // Write frame to TAP.
            if !frame.is_empty() {
                // Log first frame's hex for debugging.
                static TX_LOG_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let c = TX_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if c < 5 {
                    let hex: String = frame.iter().take(64).map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
                    tracing::info!(frame_len = frame.len(), frame_num = c, hex = %hex, "TX: frame hex dump");
                }

                let nwritten = unsafe {
                    libc::write(tap_fd, frame.as_ptr() as *const libc::c_void, frame.len())
                };
                if nwritten < 0 {
                    let errno = unsafe { *libc::__errno_location() };
                    tracing::warn!(frame_len = frame.len(), errno, "TX: TAP write failed");
                } else {
                    tracing::info!(frame_len = frame.len(), nwritten, tap_fd, "TX: wrote frame to TAP");
                }
            } else {
                tracing::warn!("TX: empty frame after stripping vnet header");
            }

            // Return descriptor to used ring.
            let _ = tx_queue.add_used(mem, chain.head_index, 0);
            tx_count += 1;
        }

        tx_count
    }

    /// Process the RX queue: read packets from TAP, write to guest.
    ///
    /// Returns true if an interrupt should be raised (packets were received).
    pub fn process_rx(&mut self) -> bool {
        let tap_fd = match self.tap_fd {
            Some(fd) => fd,
            None => return false,
        };
        let (queues, mem) = match (self.queues.as_mut(), self.mem.as_ref()) {
            (Some(q), Some(m)) => (q, m),
            _ => {
                // Log periodically when queues/mem not set
                static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if c % 10000 == 0 {
                    tracing::warn!(count = c, "process_rx: queues or mem not available");
                }
                return false;
            }
        };

        let rx_queue = match queues.get_mut(0) {
            Some(q) => q,
            None => return false,
        };

        let mut received = false;

        loop {
            // Read a frame from TAP (non-blocking).
            let mut frame_buf = vec![0u8; MAX_FRAME_SIZE];
            let nread = unsafe {
                libc::read(
                    tap_fd,
                    frame_buf.as_mut_ptr() as *mut libc::c_void,
                    frame_buf.len(),
                )
            };

            if nread > 0 {
                tracing::info!(nread, "TAP read {} bytes", nread);
            }

            if nread <= 0 {
                break; // No more packets or error (EAGAIN for non-blocking).
            }
            let frame_len = nread as usize;

            // Get a writable descriptor chain from the RX queue.
            let chain = match rx_queue.pop(mem) {
                Ok(Some(c)) => c,
                Ok(None) => {
                    tracing::warn!("RX queue empty — no descriptors available, dropping packet ({frame_len} bytes)");
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "RX queue pop error");
                    break;
                }
            };

            // Build the full payload: zeroed vnet header + frame.
            let vnet_hdr = VirtioNetHdr::default();
            let hdr_bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    &vnet_hdr as *const VirtioNetHdr as *const u8,
                    VNET_HDR_SIZE,
                )
            };

            let total_len = VNET_HDR_SIZE + frame_len;
            let mut payload = Vec::with_capacity(total_len);
            payload.extend_from_slice(hdr_bytes);
            payload.extend_from_slice(&frame_buf[..frame_len]);

            // Write payload into writable descriptors.
            let mut written = 0usize;
            for desc in chain.writable() {
                if written >= payload.len() {
                    break;
                }
                let write_len = (desc.len as usize).min(payload.len() - written);
                let _ = mem.write_slice(
                    GuestAddress::new(desc.addr),
                    &payload[written..written + write_len],
                );
                written += write_len;
            }

            let _ = rx_queue.add_used(mem, chain.head_index, written as u32);
            tracing::info!(written, frame_len, "RX: delivered packet to guest");
            received = true;
        }

        if received {
            self.signal_irq();
        }
        received
    }

    /// Returns the TAP file descriptor, if set.
    pub fn tap_fd(&self) -> Option<RawFd> {
        self.tap_fd
    }
}

impl VirtioDevice for Net {
    fn device_type(&self) -> u32 {
        VIRTIO_NET_DEVICE_TYPE
    }

    fn num_queues(&self) -> usize {
        2 // rx (0) and tx (1)
    }

    fn device_features(&self, page: u32) -> u32 {
        let features = VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS;
        match page {
            0 => features as u32,
            _ => 0,
        }
    }

    fn ack_features(&mut self, page: u32, value: u32) {
        self.acked_features |= (value as u64) << (page * 32);
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config_bytes = unsafe {
            std::slice::from_raw_parts(
                &self.config as *const VirtioNetConfig as *const u8,
                std::mem::size_of::<VirtioNetConfig>(),
            )
        };
        let offset = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config_bytes.get(offset + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(&mut self, queues: Vec<Queue>, mem: Arc<GuestMemoryMmap>) -> crate::error::Result<()> {
        tracing::info!(
            tap = %self.tap_name,
            mac = ?self.config.mac,
            "virtio-net activated"
        );
        self.queues = Some(queues);
        self.mem = Some(mem);
        Ok(())
    }

    fn queue_notify(&mut self, queue_index: u16, _mem: &GuestMemoryMmap) {
        match queue_index {
            1 => {
                // TX queue notification — process outgoing packets.
                tracing::info!("TX queue kicked by guest");
                let count = self.process_tx();
                tracing::info!(count, "TX: processed packets");
            }
            0 => {
                // RX queue notification — guest made buffers available.
                tracing::info!("RX queue notified (guest added buffers)");
            }
            _ => {
                tracing::warn!(queue_index, "unknown net queue notified");
            }
        }
    }

    fn reset(&mut self) {
        self.acked_features = 0;
        self.queues = None;
        self.mem = None;
    }

    fn set_tap_fd(&mut self, fd: RawFd) {
        self.tap_fd = Some(fd);
    }

    fn activated_queues(&self) -> Option<&[crate::queue::Queue]> {
        self.queues.as_deref()
    }

    fn poll(&mut self) -> bool {
        static POLL_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let c = POLL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if c % 50000 == 0 {
            tracing::info!(poll_count = c, tap_fd = ?self.tap_fd, queues_set = self.queues.is_some(), "net::poll called");
        }
        self.process_rx()
    }
}
