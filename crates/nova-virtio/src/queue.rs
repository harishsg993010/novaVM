//! Virtio split virtqueue implementation.
//!
//! A virtqueue consists of three areas in guest memory:
//! - Descriptor table: array of VirtqDesc
//! - Available ring: guest-published available descriptors
//! - Used ring: device-published used descriptors

use nova_mem::{GuestAddress, GuestMemoryMmap};

use crate::error::{Result, VirtioError};

/// Maximum queue size.
pub const MAX_QUEUE_SIZE: u16 = 1024;

/// A virtqueue descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqDesc {
    /// Guest physical address of the buffer.
    pub addr: u64,
    /// Length of the buffer.
    pub len: u32,
    /// Descriptor flags.
    pub flags: u16,
    /// Next descriptor index (if VIRTQ_DESC_F_NEXT is set).
    pub next: u16,
}

/// Descriptor flags.
pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// Available ring header.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    // ring[queue_size] follows
}

/// Used ring header.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    // ring[queue_size] follows, each element is VirtqUsedElem
}

/// Used ring element.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

/// A chain of descriptors from the available ring.
#[derive(Debug)]
pub struct DescriptorChain {
    /// The head descriptor index.
    pub head_index: u16,
    /// The descriptors in order.
    pub descriptors: Vec<VirtqDesc>,
}

impl DescriptorChain {
    /// Returns an iterator over readable (device-reads, guest-written) descriptors.
    pub fn readable(&self) -> impl Iterator<Item = &VirtqDesc> {
        self.descriptors
            .iter()
            .filter(|d| d.flags & VIRTQ_DESC_F_WRITE == 0)
    }

    /// Returns an iterator over writable (device-writes, guest-reads) descriptors.
    pub fn writable(&self) -> impl Iterator<Item = &VirtqDesc> {
        self.descriptors
            .iter()
            .filter(|d| d.flags & VIRTQ_DESC_F_WRITE != 0)
    }
}

/// A split virtqueue.
pub struct Queue {
    /// Maximum size of the queue.
    pub max_size: u16,
    /// Actual size (must be power of 2, <= max_size).
    pub size: u16,
    /// Whether the queue is ready/enabled.
    pub ready: bool,
    /// Guest physical address of the descriptor table.
    pub desc_table: GuestAddress,
    /// Guest physical address of the available ring.
    pub avail_ring: GuestAddress,
    /// Guest physical address of the used ring.
    pub used_ring: GuestAddress,
    /// Our cached copy of the last seen available index.
    next_avail: u16,
    /// Our cached copy of the next used index.
    next_used: u16,
}

impl Queue {
    /// Create a new queue with the given maximum size.
    pub fn new(max_size: u16) -> Self {
        Self {
            max_size,
            size: max_size,
            ready: false,
            desc_table: GuestAddress::new(0),
            avail_ring: GuestAddress::new(0),
            used_ring: GuestAddress::new(0),
            next_avail: 0,
            next_used: 0,
        }
    }

    /// Create a queue from saved state (snapshot restore).
    pub fn from_saved(
        max_size: u16,
        size: u16,
        ready: bool,
        desc_table: GuestAddress,
        avail_ring: GuestAddress,
        used_ring: GuestAddress,
        next_avail: u16,
        next_used: u16,
    ) -> Self {
        Self {
            max_size,
            size,
            ready,
            desc_table,
            avail_ring,
            used_ring,
            next_avail,
            next_used,
        }
    }

    /// Get the cached next available index.
    pub fn next_avail(&self) -> u16 {
        self.next_avail
    }

    /// Get the cached next used index.
    pub fn next_used(&self) -> u16 {
        self.next_used
    }

    /// Pop the next available descriptor chain from the queue.
    ///
    /// Returns `None` if the queue is empty.
    pub fn pop(&mut self, mem: &GuestMemoryMmap) -> Result<Option<DescriptorChain>> {
        if !self.ready {
            return Err(VirtioError::QueueNotReady);
        }

        // Read the available ring index.
        let avail_idx: u16 = mem.read_obj(self.avail_ring + std::mem::size_of::<u16>() as u64)?;

        if self.next_avail == avail_idx {
            return Ok(None); // Queue is empty.
        }

        // Read the descriptor index from the available ring.
        let ring_offset = 4 + (self.next_avail % self.size) as u64 * 2;
        let desc_idx: u16 = mem.read_obj(self.avail_ring + ring_offset)?;

        // Walk the descriptor chain.
        let mut descriptors = Vec::new();
        let mut idx = desc_idx;
        let mut seen = 0u32;

        loop {
            if seen >= self.size as u32 {
                return Err(VirtioError::InvalidChain(
                    "descriptor chain too long".to_string(),
                ));
            }

            let desc_addr = self.desc_table + (idx as u64 * 16);
            let desc: VirtqDesc = mem.read_obj(desc_addr)?;
            let has_next = desc.flags & VIRTQ_DESC_F_NEXT != 0;
            let next = desc.next;
            descriptors.push(desc);
            seen += 1;

            if !has_next {
                break;
            }
            idx = next;
        }

        self.next_avail = self.next_avail.wrapping_add(1);

        Ok(Some(DescriptorChain {
            head_index: desc_idx,
            descriptors,
        }))
    }

    /// Add a used descriptor to the used ring.
    pub fn add_used(&mut self, mem: &GuestMemoryMmap, head_index: u16, len: u32) -> Result<()> {
        let used_elem = VirtqUsedElem {
            id: head_index as u32,
            len,
        };

        // Write the used element.
        let ring_offset = 4 + (self.next_used % self.size) as u64 * 8;
        mem.write_obj(self.used_ring + ring_offset, &used_elem)?;

        // Update the used index.
        self.next_used = self.next_used.wrapping_add(1);
        let idx_offset = std::mem::size_of::<u16>() as u64; // flags is 2 bytes, idx follows
        mem.write_obj(self.used_ring + idx_offset, &self.next_used)?;

        Ok(())
    }

    /// Check if the guest wants notifications (used ring events).
    pub fn needs_notification(&self, mem: &GuestMemoryMmap) -> Result<bool> {
        // Read avail flags — if VIRTQ_AVAIL_F_NO_INTERRUPT is set, skip notification.
        let avail_flags: u16 = mem.read_obj(self.avail_ring)?;
        Ok(avail_flags & 1 == 0)
    }

    /// Returns whether there are pending available descriptors.
    pub fn has_pending(&self, mem: &GuestMemoryMmap) -> Result<bool> {
        let avail_idx: u16 = mem.read_obj(self.avail_ring + std::mem::size_of::<u16>() as u64)?;
        Ok(self.next_avail != avail_idx)
    }
}
