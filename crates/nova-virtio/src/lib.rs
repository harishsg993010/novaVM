//! # nova-virtio
//!
//! Virtio device framework for NovaVM.
//!
//! Provides:
//! - Split virtqueue implementation (descriptor chains, available/used rings)
//! - MMIO transport (virtio-mmio v2 register handling)
//! - Device implementations: console, block, net, vsock, balloon, fs

pub mod balloon;
pub mod blk;
pub mod console;
pub mod error;
pub mod fs;
pub mod mmio;
pub mod net;
pub mod queue;
pub mod tap;
pub mod vsock;

pub use error::{Result, VirtioError};

#[cfg(test)]
mod tests {
    use super::*;
    use console::Console;
    use mmio::*;
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use queue::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Helper: set up a virtqueue in guest memory at the given addresses.
    fn setup_queue_in_memory(
        mem: &GuestMemoryMmap,
        desc_addr: u64,
        avail_addr: u64,
        used_addr: u64,
        queue_size: u16,
    ) -> Queue {
        let mut q = Queue::new(queue_size);
        q.desc_table = GuestAddress::new(desc_addr);
        q.avail_ring = GuestAddress::new(avail_addr);
        q.used_ring = GuestAddress::new(used_addr);
        q.size = queue_size;
        q.ready = true;

        // Initialize avail ring: flags=0, idx=0.
        let zero: u16 = 0;
        mem.write_obj(GuestAddress::new(avail_addr), &zero).unwrap(); // flags
        mem.write_obj(GuestAddress::new(avail_addr + 2), &zero)
            .unwrap(); // idx

        // Initialize used ring: flags=0, idx=0.
        mem.write_obj(GuestAddress::new(used_addr), &zero).unwrap();
        mem.write_obj(GuestAddress::new(used_addr + 2), &zero)
            .unwrap();

        q
    }

    #[test]
    fn test_queue_empty_pop() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 65536)], false).unwrap();
        let mut q = setup_queue_in_memory(&mem, 0x1000, 0x2000, 0x3000, 16);

        // Queue is empty — pop should return None.
        let chain = q.pop(&mem).unwrap();
        assert!(chain.is_none());
    }

    #[test]
    fn test_queue_pop_single_descriptor() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 65536)], false).unwrap();
        let mut q = setup_queue_in_memory(&mem, 0x1000, 0x2000, 0x3000, 16);

        // Write a single descriptor at index 0.
        let desc = VirtqDesc {
            addr: 0x5000,
            len: 256,
            flags: 0, // readable, no next
            next: 0,
        };
        mem.write_obj(GuestAddress::new(0x1000), &desc).unwrap();

        // Update available ring: ring[0] = 0, idx = 1.
        let desc_idx: u16 = 0;
        mem.write_obj(GuestAddress::new(0x2000 + 4), &desc_idx)
            .unwrap(); // ring[0]
        let avail_idx: u16 = 1;
        mem.write_obj(GuestAddress::new(0x2000 + 2), &avail_idx)
            .unwrap(); // idx

        // Pop should return the descriptor chain.
        let chain = q.pop(&mem).unwrap().expect("expected a descriptor chain");
        assert_eq!(chain.head_index, 0);
        assert_eq!(chain.descriptors.len(), 1);
        assert_eq!(chain.descriptors[0].addr, 0x5000);
        assert_eq!(chain.descriptors[0].len, 256);
    }

    #[test]
    fn test_queue_pop_chained_descriptors() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 65536)], false).unwrap();
        let mut q = setup_queue_in_memory(&mem, 0x1000, 0x2000, 0x3000, 16);

        // Descriptor 0: readable, next=1.
        let desc0 = VirtqDesc {
            addr: 0x5000,
            len: 128,
            flags: VIRTQ_DESC_F_NEXT,
            next: 1,
        };
        mem.write_obj(GuestAddress::new(0x1000), &desc0).unwrap();

        // Descriptor 1: writable, no next.
        let desc1 = VirtqDesc {
            addr: 0x6000,
            len: 512,
            flags: VIRTQ_DESC_F_WRITE,
            next: 0,
        };
        mem.write_obj(GuestAddress::new(0x1000 + 16), &desc1)
            .unwrap();

        // Available ring: ring[0] = 0, idx = 1.
        let desc_idx: u16 = 0;
        mem.write_obj(GuestAddress::new(0x2000 + 4), &desc_idx)
            .unwrap();
        let avail_idx: u16 = 1;
        mem.write_obj(GuestAddress::new(0x2000 + 2), &avail_idx)
            .unwrap();

        let chain = q.pop(&mem).unwrap().expect("expected chain");
        assert_eq!(chain.descriptors.len(), 2);

        // First descriptor is readable.
        let readable: Vec<_> = chain.readable().collect();
        assert_eq!(readable.len(), 1);
        assert_eq!(readable[0].addr, 0x5000);

        // Second descriptor is writable.
        let writable: Vec<_> = chain.writable().collect();
        assert_eq!(writable.len(), 1);
        assert_eq!(writable[0].addr, 0x6000);
    }

    #[test]
    fn test_queue_add_used() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 65536)], false).unwrap();
        let mut q = setup_queue_in_memory(&mem, 0x1000, 0x2000, 0x3000, 16);

        q.add_used(&mem, 0, 256).unwrap();

        // Read back the used ring.
        let used_idx: u16 = mem.read_obj(GuestAddress::new(0x3000 + 2)).unwrap();
        assert_eq!(used_idx, 1);

        let used_elem: VirtqUsedElem = mem.read_obj(GuestAddress::new(0x3000 + 4)).unwrap();
        assert_eq!(used_elem.id, 0);
        assert_eq!(used_elem.len, 256);
    }

    #[test]
    fn test_mmio_transport_register_reads() {
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let console = Console::new(output);
        let transport = MmioTransport::new(Box::new(console));

        // Magic.
        assert_eq!(transport.read(MMIO_MAGIC_VALUE), VIRTIO_MMIO_MAGIC);

        // Version.
        assert_eq!(transport.read(MMIO_VERSION), VIRTIO_MMIO_VERSION);

        // Device type (console = 3).
        assert_eq!(transport.read(MMIO_DEVICE_ID), 3);

        // Vendor ID = "Nova".
        assert_eq!(transport.read(MMIO_VENDOR_ID), 0x4E6F_7661);
    }

    #[test]
    fn test_mmio_transport_queue_setup() {
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let console = Console::new(output);
        let mut transport = MmioTransport::new(Box::new(console));

        // Select queue 0.
        transport.write(MMIO_QUEUE_SEL, 0);

        // Check max queue size.
        let max = transport.read(MMIO_QUEUE_NUM_MAX);
        assert!(max > 0);

        // Set queue size.
        transport.write(MMIO_QUEUE_NUM, 16);

        // Set descriptor table address.
        transport.write(MMIO_QUEUE_DESC_LOW, 0x1000);
        transport.write(MMIO_QUEUE_DESC_HIGH, 0);

        // Set available ring address.
        transport.write(MMIO_QUEUE_AVAIL_LOW, 0x2000);
        transport.write(MMIO_QUEUE_AVAIL_HIGH, 0);

        // Set used ring address.
        transport.write(MMIO_QUEUE_USED_LOW, 0x3000);
        transport.write(MMIO_QUEUE_USED_HIGH, 0);

        // Enable the queue.
        transport.write(MMIO_QUEUE_READY, 1);
        assert_eq!(transport.read(MMIO_QUEUE_READY), 1);
    }

    #[test]
    fn test_mmio_transport_status_sequence() {
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let console = Console::new(output);
        let mut transport = MmioTransport::new(Box::new(console));

        // Standard virtio initialization sequence.
        transport.write(MMIO_STATUS, 0); // reset
        assert_eq!(transport.read(MMIO_STATUS), 0);

        transport.write(MMIO_STATUS, STATUS_ACKNOWLEDGE);
        assert_eq!(transport.read(MMIO_STATUS), STATUS_ACKNOWLEDGE);

        transport.write(MMIO_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        assert_eq!(
            transport.read(MMIO_STATUS),
            STATUS_ACKNOWLEDGE | STATUS_DRIVER
        );
    }

    #[test]
    fn test_balloon_device() {
        let balloon = balloon::Balloon::new();
        balloon.set_target_pages(100);
        assert_eq!(balloon.actual_pages(), 0);
    }

    #[test]
    fn test_descriptor_chain_iterators() {
        let chain = DescriptorChain {
            head_index: 0,
            descriptors: vec![
                VirtqDesc {
                    addr: 0x1000,
                    len: 64,
                    flags: 0,
                    next: 0,
                },
                VirtqDesc {
                    addr: 0x2000,
                    len: 128,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
                VirtqDesc {
                    addr: 0x3000,
                    len: 256,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            ],
        };

        assert_eq!(chain.readable().count(), 1);
        assert_eq!(chain.writable().count(), 2);
    }
}
