//! # nova-mem
//!
//! Guest physical memory management for NovaVM.
//!
//! Provides:
//! - `GuestAddress`: newtype for guest physical addresses
//! - `MmapRegion`: RAII anonymous mmap wrapper
//! - `GuestMemoryMmap`: multi-region guest memory with read/write operations
//! - `DirtyLogTracker`: dirty page tracking via KVM ioctls

pub mod dirty;
pub mod error;
pub mod guest_address;
pub mod guest_memory;
pub mod mmap;

pub use error::{MemError, Result};
pub use guest_address::GuestAddress;
pub use guest_memory::GuestMemoryMmap;
pub use mmap::MmapRegion;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guest_address_arithmetic() {
        let a = GuestAddress::new(0x1000);
        let b = GuestAddress::new(0x2000);

        // Add offset.
        assert_eq!(a + 0x500, GuestAddress::new(0x1500));

        // Sub addresses.
        assert_eq!(b - a, 0x1000);

        // Checked add.
        assert_eq!(a.checked_add(0x100), Some(GuestAddress::new(0x1100)));
        assert_eq!(GuestAddress::new(u64::MAX).checked_add(1), None);

        // Checked sub.
        assert_eq!(b.checked_sub(a), Some(0x1000));
        assert_eq!(a.checked_sub(b), None);

        // Display.
        assert_eq!(format!("{a}"), "0x1000");

        // Ord.
        assert!(a < b);
    }

    #[test]
    fn test_mmap_region_alloc_and_access() {
        let size = 4096;
        let region = MmapRegion::new(size, false).expect("mmap failed");

        assert_eq!(region.size(), size);
        assert!(!region.as_ptr().is_null());
        assert!(!region.is_hugetlb());

        // Write and read back.
        // SAFETY: region is valid for `size` bytes.
        unsafe {
            let ptr = region.as_mut_ptr();
            std::ptr::write(ptr, 0xAB);
            std::ptr::write(ptr.add(size - 1), 0xCD);
            assert_eq!(std::ptr::read(ptr), 0xAB);
            assert_eq!(std::ptr::read(ptr.add(size - 1)), 0xCD);
        }
    }

    #[test]
    fn test_mmap_region_hugetlb_fallback() {
        // This should succeed even if huge pages aren't configured —
        // it falls back to normal pages.
        let size = 2 * 1024 * 1024; // 2 MiB
        let region = MmapRegion::new(size, true).expect("mmap with hugetlb fallback failed");
        assert_eq!(region.size(), size);
    }

    #[test]
    fn test_guest_memory_write_read_obj() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 4096)], false)
            .expect("failed to create guest memory");

        // Write a u32 at offset 0.
        let val: u32 = 0xDEAD_BEEF;
        mem.write_obj(GuestAddress::new(0), &val)
            .expect("write_obj failed");

        // Read it back.
        let read_val: u32 = mem.read_obj(GuestAddress::new(0)).expect("read_obj failed");
        assert_eq!(read_val, 0xDEAD_BEEF);
    }

    #[test]
    fn test_guest_memory_write_read_slice() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0x1000), 8192)], false)
            .expect("failed to create guest memory");

        let data = b"Hello, NovaVM guest memory!";
        mem.write_slice(GuestAddress::new(0x1000), data)
            .expect("write_slice failed");

        let mut buf = vec![0u8; data.len()];
        mem.read_slice(GuestAddress::new(0x1000), &mut buf)
            .expect("read_slice failed");
        assert_eq!(&buf, data);
    }

    #[test]
    fn test_guest_memory_out_of_bounds() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 4096)], false)
            .expect("failed to create guest memory");

        // Write past the end — should fail.
        let data = vec![0u8; 4097];
        let err = mem.write_slice(GuestAddress::new(0), &data);
        assert!(err.is_err(), "expected out of bounds error");

        // Read from an unmapped address.
        let mut buf = [0u8; 1];
        let err = mem.read_slice(GuestAddress::new(0x10000), &mut buf);
        assert!(err.is_err(), "expected no-region error");
    }

    #[test]
    fn test_guest_memory_multi_region() {
        let mem = GuestMemoryMmap::new(
            &[
                (GuestAddress::new(0x0000), 4096),
                (GuestAddress::new(0x10000), 8192),
            ],
            false,
        )
        .expect("failed to create multi-region memory");

        assert_eq!(mem.num_regions(), 2);
        assert_eq!(mem.total_size(), 4096 + 8192);

        // Write to first region.
        let val1: u64 = 0x1111_1111;
        mem.write_obj(GuestAddress::new(0), &val1).unwrap();

        // Write to second region.
        let val2: u64 = 0x2222_2222;
        mem.write_obj(GuestAddress::new(0x10000), &val2).unwrap();

        // Read back from both.
        let r1: u64 = mem.read_obj(GuestAddress::new(0)).unwrap();
        let r2: u64 = mem.read_obj(GuestAddress::new(0x10000)).unwrap();
        assert_eq!(r1, 0x1111_1111);
        assert_eq!(r2, 0x2222_2222);

        // Gap between regions should fail.
        let mut buf = [0u8; 1];
        assert!(mem.read_slice(GuestAddress::new(0x5000), &mut buf).is_err());
    }

    #[test]
    fn test_dirty_log_bitmap_parsing() {
        use dirty::DirtyLogTracker;

        // Simulate a bitmap with pages 0, 2, and 65 dirty.
        let bitmap: Vec<u64> = vec![0b101, 1u64 << 1]; // pages 0, 2 in word 0; page 65 in word 1
        let dirty = DirtyLogTracker::dirty_pages(&bitmap);
        assert_eq!(dirty, vec![0, 2, 65]);
    }

    #[test]
    fn test_guest_memory_iter_regions() {
        let mem = GuestMemoryMmap::new(
            &[
                (GuestAddress::new(0x0), 4096),
                (GuestAddress::new(0x100000), 8192),
            ],
            false,
        )
        .unwrap();

        let regions: Vec<_> = mem.iter_regions().collect();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].0, GuestAddress::new(0));
        assert_eq!(regions[0].1, 4096);
        assert_eq!(regions[1].0, GuestAddress::new(0x100000));
        assert_eq!(regions[1].1, 8192);
    }
}
