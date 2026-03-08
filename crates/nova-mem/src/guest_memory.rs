use std::os::unix::io::RawFd;

use crate::error::{MemError, Result};
use crate::guest_address::GuestAddress;
use crate::mmap::MmapRegion;

/// A single guest memory region backed by an mmap.
struct GuestRegion {
    /// Guest physical base address.
    guest_base: GuestAddress,
    /// The underlying mmap'd host memory.
    mmap: MmapRegion,
}

/// Guest physical memory composed of one or more non-overlapping mmap regions.
pub struct GuestMemoryMmap {
    regions: Vec<GuestRegion>,
}

impl GuestMemoryMmap {
    /// Create a new guest memory layout from a list of (guest_base, size) pairs.
    ///
    /// Each region is allocated as anonymous private memory.
    /// Set `hugetlb` to attempt huge page backing for all regions.
    pub fn new(regions: &[(GuestAddress, usize)], hugetlb: bool) -> Result<Self> {
        let mut guest_regions = Vec::with_capacity(regions.len());
        for &(base, size) in regions {
            let mmap = MmapRegion::new(size, hugetlb)?;
            guest_regions.push(GuestRegion {
                guest_base: base,
                mmap,
            });
        }
        // Sort by guest address for binary search.
        guest_regions.sort_by_key(|r| r.guest_base);
        Ok(Self {
            regions: guest_regions,
        })
    }

    /// Create guest memory backed by a snapshot file (demand-paged, MAP_PRIVATE).
    ///
    /// Each region is mapped from the file at the corresponding offset.
    /// Pages fault in lazily as the guest accesses them — no upfront I/O.
    /// `regions` is a slice of (guest_addr, size, file_offset) tuples.
    pub fn from_file(fd: RawFd, regions: &[(GuestAddress, usize, i64)]) -> Result<Self> {
        let mut guest_regions = Vec::with_capacity(regions.len());
        for &(base, size, file_offset) in regions {
            let mmap = MmapRegion::from_file(fd, size, file_offset)?;
            guest_regions.push(GuestRegion {
                guest_base: base,
                mmap,
            });
        }
        guest_regions.sort_by_key(|r| r.guest_base);
        Ok(Self {
            regions: guest_regions,
        })
    }

    /// Find the region containing `addr` and return (region_index, offset_within_region).
    fn find_region(&self, addr: GuestAddress) -> Result<(usize, usize)> {
        for (i, region) in self.regions.iter().enumerate() {
            if addr >= region.guest_base {
                let offset = addr.raw() - region.guest_base.raw();
                if (offset as usize) < region.mmap.size() {
                    return Ok((i, offset as usize));
                }
            }
        }
        Err(MemError::NoRegion(addr.raw()))
    }

    /// Write a `T: Copy` object at the given guest address.
    pub fn write_obj<T: Copy>(&self, addr: GuestAddress, val: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let (idx, offset) = self.find_region(addr)?;
        let region = &self.regions[idx];

        if offset + size > region.mmap.size() {
            return Err(MemError::OutOfBounds {
                offset: offset as u64,
                size,
                region_size: region.mmap.size() as u64,
            });
        }

        // SAFETY: We verified bounds above. The source is a valid T reference.
        unsafe {
            let dst = region.mmap.as_mut_ptr().add(offset);
            std::ptr::copy_nonoverlapping(val as *const T as *const u8, dst, size);
        }
        Ok(())
    }

    /// Read a `T: Copy` object from the given guest address.
    pub fn read_obj<T: Copy + Default>(&self, addr: GuestAddress) -> Result<T> {
        let size = std::mem::size_of::<T>();
        let (idx, offset) = self.find_region(addr)?;
        let region = &self.regions[idx];

        if offset + size > region.mmap.size() {
            return Err(MemError::OutOfBounds {
                offset: offset as u64,
                size,
                region_size: region.mmap.size() as u64,
            });
        }

        let mut val = T::default();
        // SAFETY: We verified bounds. The destination is a valid T.
        unsafe {
            let src = region.mmap.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(src, &mut val as *mut T as *mut u8, size);
        }
        Ok(val)
    }

    /// Write a byte slice to guest memory at `addr`.
    pub fn write_slice(&self, addr: GuestAddress, data: &[u8]) -> Result<()> {
        let (idx, offset) = self.find_region(addr)?;
        let region = &self.regions[idx];

        if offset + data.len() > region.mmap.size() {
            return Err(MemError::OutOfBounds {
                offset: offset as u64,
                size: data.len(),
                region_size: region.mmap.size() as u64,
            });
        }

        // SAFETY: Bounds checked above.
        unsafe {
            let dst = region.mmap.as_mut_ptr().add(offset);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        Ok(())
    }

    /// Read bytes from guest memory at `addr` into `buf`.
    pub fn read_slice(&self, addr: GuestAddress, buf: &mut [u8]) -> Result<()> {
        let (idx, offset) = self.find_region(addr)?;
        let region = &self.regions[idx];

        if offset + buf.len() > region.mmap.size() {
            return Err(MemError::OutOfBounds {
                offset: offset as u64,
                size: buf.len(),
                region_size: region.mmap.size() as u64,
            });
        }

        // SAFETY: Bounds checked above.
        unsafe {
            let src = region.mmap.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len());
        }
        Ok(())
    }

    /// Returns a raw host pointer for a guest address. Useful for KVM memory mapping.
    ///
    /// # Safety
    /// The caller must ensure the pointer is not used after the GuestMemoryMmap is dropped.
    pub unsafe fn as_ptr(&self, addr: GuestAddress) -> Result<*const u8> {
        let (idx, offset) = self.find_region(addr)?;
        let region = &self.regions[idx];
        Ok(region.mmap.as_ptr().add(offset))
    }

    /// Returns the number of regions.
    pub fn num_regions(&self) -> usize {
        self.regions.len()
    }

    /// Iterate over all regions, yielding (guest_base, size, host_ptr).
    pub fn iter_regions(&self) -> impl Iterator<Item = (GuestAddress, usize, *const u8)> + '_ {
        self.regions
            .iter()
            .map(|r| (r.guest_base, r.mmap.size(), r.mmap.as_ptr()))
    }

    /// Returns the total size of all memory regions.
    pub fn total_size(&self) -> usize {
        self.regions.iter().map(|r| r.mmap.size()).sum()
    }

    /// Returns the host userspace address for a region (for KVM_SET_USER_MEMORY_REGION).
    pub fn region_host_addr(&self, index: usize) -> Option<u64> {
        self.regions.get(index).map(|r| r.mmap.as_userspace_addr())
    }

    /// Returns the guest base and size for a region.
    pub fn region_info(&self, index: usize) -> Option<(GuestAddress, usize)> {
        self.regions
            .get(index)
            .map(|r| (r.guest_base, r.mmap.size()))
    }
}
