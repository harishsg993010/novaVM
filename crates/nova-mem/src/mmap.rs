use std::os::unix::io::RawFd;

use crate::error::{MemError, Result};

/// An RAII wrapper around a `mmap`-ed memory region (anonymous or file-backed).
pub struct MmapRegion {
    addr: *mut u8,
    size: usize,
    hugetlb: bool,
}

// SAFETY: The mmap'd region is owned exclusively by this struct.
// No aliasing pointers exist outside of controlled borrows via as_ptr/as_mut_ptr.
unsafe impl Send for MmapRegion {}
unsafe impl Sync for MmapRegion {}

impl MmapRegion {
    /// Allocate an anonymous private memory region of `size` bytes.
    ///
    /// If `hugetlb` is true, attempts to use huge pages (MAP_HUGETLB).
    /// Falls back to normal pages if huge pages are unavailable.
    pub fn new(size: usize, hugetlb: bool) -> Result<Self> {
        let mut flags = libc::MAP_ANONYMOUS | libc::MAP_PRIVATE;
        if hugetlb {
            flags |= libc::MAP_HUGETLB;
        }

        // SAFETY: We request anonymous private memory with no fd.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                -1,
                0,
            )
        };

        if addr == libc::MAP_FAILED {
            if hugetlb {
                // Fallback: retry without huge pages.
                tracing::warn!("huge pages unavailable, falling back to normal pages");
                return Self::new(size, false);
            }
            return Err(MemError::Mmap(std::io::Error::last_os_error()));
        }

        Ok(Self {
            addr: addr.cast::<u8>(),
            size,
            hugetlb,
        })
    }

    /// Returns a raw pointer to the start of the mapped region.
    pub fn as_ptr(&self) -> *const u8 {
        self.addr
    }

    /// Returns a mutable raw pointer to the start of the mapped region.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.addr
    }

    /// Returns the size of the mapped region in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns whether this region uses huge pages.
    pub fn is_hugetlb(&self) -> bool {
        self.hugetlb
    }

    /// Returns the host virtual address as a u64 (for KVM userspace_addr).
    pub fn as_userspace_addr(&self) -> u64 {
        self.addr as u64
    }

    /// Create a file-backed private (copy-on-write) memory region.
    ///
    /// Pages are demand-paged from the file — only pages the guest actually
    /// touches trigger disk I/O. Writes go to private copies (MAP_PRIVATE),
    /// so the backing file is never modified. The fd can be closed after this
    /// call; the kernel keeps its own reference to the file.
    pub fn from_file(fd: RawFd, size: usize, offset: i64) -> Result<Self> {
        // SAFETY: MAP_PRIVATE gives us copy-on-write semantics over the file.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                fd,
                offset,
            )
        };

        if addr == libc::MAP_FAILED {
            return Err(MemError::Mmap(std::io::Error::last_os_error()));
        }

        // Hint: random access pattern (VM memory), disable readahead.
        unsafe {
            libc::madvise(addr, size, libc::MADV_RANDOM);
        }

        Ok(Self {
            addr: addr.cast::<u8>(),
            size,
            hugetlb: false,
        })
    }
}

impl Drop for MmapRegion {
    fn drop(&mut self) {
        // SAFETY: We own this mapping and addr/size are valid from mmap.
        let ret = unsafe { libc::munmap(self.addr.cast(), self.size) };
        if ret != 0 {
            tracing::error!(
                error = %std::io::Error::last_os_error(),
                "munmap failed during MmapRegion drop"
            );
        }
    }
}
