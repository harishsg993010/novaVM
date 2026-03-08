use crate::boot_params::E820Entry;
use crate::error::{BootError, Result};
use nova_mem::{GuestAddress, GuestMemoryMmap};

/// PVH boot start info structure (follows the Xen PVH boot protocol).
///
/// This enables direct boot of kernels that support PVH entry,
/// bypassing the legacy real-mode boot path entirely.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HvmStartInfo {
    /// Magic: "xEn3" = 0x336EC578.
    pub magic: u32,
    /// Version of the start info structure.
    pub version: u32,
    /// Flags.
    pub flags: u32,
    /// Number of modules (0 for direct boot).
    pub nr_modules: u32,
    /// Physical address of the module list.
    pub modlist_paddr: u64,
    /// Physical address of the command line (null-terminated).
    pub cmdline_paddr: u64,
    /// Physical address of the RSDP (ACPI).
    pub rsdp_paddr: u64,
    /// Physical address of the memory map.
    pub memmap_paddr: u64,
    /// Number of memory map entries.
    pub memmap_entries: u32,
    /// Reserved.
    pub _pad: u32,
}

/// PVH memory map entry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HvmMemmapEntry {
    pub addr: u64,
    pub size: u64,
    pub type_: u32,
    pub reserved: u32,
}

/// PVH start info magic: "xEn3".
pub const XEN_HVM_START_MAGIC: u32 = 0x336E_C578;

/// Default address where we place the HVM start info.
pub const PVH_START_INFO_ADDR: u64 = 0x6000;

/// Default address for the PVH memory map.
pub const PVH_MEMMAP_ADDR: u64 = 0x6100;

/// Set up PVH boot structures in guest memory.
///
/// Returns the entry point register value (RBX should point to start_info).
pub fn setup_pvh_boot(
    mem: &GuestMemoryMmap,
    cmdline_addr: u64,
    e820_entries: &[E820Entry],
) -> Result<u64> {
    if e820_entries.len() > 128 {
        return Err(BootError::InvalidPvhNote);
    }

    // Build memory map entries.
    let memmap: Vec<HvmMemmapEntry> = e820_entries
        .iter()
        .map(|e| HvmMemmapEntry {
            addr: e.addr,
            size: e.size,
            type_: e.type_,
            reserved: 0,
        })
        .collect();

    // Write memory map to guest memory.
    let memmap_bytes: Vec<u8> = memmap
        .iter()
        .flat_map(|entry| {
            let ptr = entry as *const HvmMemmapEntry as *const u8;
            // SAFETY: HvmMemmapEntry is repr(C) and we read its full size.
            unsafe { std::slice::from_raw_parts(ptr, std::mem::size_of::<HvmMemmapEntry>()) }
        })
        .copied()
        .collect();

    mem.write_slice(GuestAddress::new(PVH_MEMMAP_ADDR), &memmap_bytes)?;

    // Build start info.
    let start_info = HvmStartInfo {
        magic: XEN_HVM_START_MAGIC,
        version: 1,
        flags: 0,
        nr_modules: 0,
        modlist_paddr: 0,
        cmdline_paddr: cmdline_addr,
        rsdp_paddr: 0,
        memmap_paddr: PVH_MEMMAP_ADDR,
        memmap_entries: e820_entries.len() as u32,
        _pad: 0,
    };

    // Write start info.
    let info_bytes = unsafe {
        std::slice::from_raw_parts(
            &start_info as *const HvmStartInfo as *const u8,
            std::mem::size_of::<HvmStartInfo>(),
        )
    };
    mem.write_slice(GuestAddress::new(PVH_START_INFO_ADDR), info_bytes)?;

    tracing::info!(
        start_info_addr = PVH_START_INFO_ADDR,
        memmap_entries = e820_entries.len(),
        "PVH boot structures written"
    );

    Ok(PVH_START_INFO_ADDR)
}
