use crate::error::Result;
use nova_kvm::vm::VmFd;

/// Dirty page tracking for live migration support.
pub struct DirtyLogTracker {
    /// Number of pages per slot.
    pages_per_slot: Vec<usize>,
}

impl DirtyLogTracker {
    /// Create a tracker for the given slot sizes (in bytes).
    pub fn new(slot_sizes: &[usize]) -> Self {
        let page_size = 4096usize;
        let pages_per_slot = slot_sizes
            .iter()
            .map(|&size| size.div_ceil(page_size))
            .collect();
        Self { pages_per_slot }
    }

    /// Get the dirty page bitmap for a slot.
    ///
    /// The bitmap has one bit per page. A set bit means the page was written
    /// since the last call to `get_dirty_log`.
    pub fn get_dirty_log(&self, vm: &VmFd, slot: u32) -> Result<Vec<u64>> {
        let num_pages = self.pages_per_slot[slot as usize];
        // Bitmap: 1 bit per page, packed into u64s.
        let bitmap_len = num_pages.div_ceil(64);
        let mut bitmap = vec![0u64; bitmap_len];
        vm.get_dirty_log(slot, &mut bitmap)?;
        Ok(bitmap)
    }

    /// Returns the list of dirty page indices from a bitmap.
    pub fn dirty_pages(bitmap: &[u64]) -> Vec<usize> {
        let mut pages = Vec::new();
        for (i, &word) in bitmap.iter().enumerate() {
            if word == 0 {
                continue;
            }
            for bit in 0..64 {
                if word & (1u64 << bit) != 0 {
                    pages.push(i * 64 + bit);
                }
            }
        }
        pages
    }
}
