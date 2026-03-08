//! x86_64 Linux boot memory layout constants.

/// Where the real-mode setup data (boot_params / zero page) is loaded.
pub const ZERO_PAGE_ADDR: u64 = 0x7000;

/// Default address for the protected-mode kernel code (bzImage).
pub const KERNEL_LOAD_ADDR: u64 = 0x100000; // 1 MiB

/// Command line address (just above zero page).
pub const CMDLINE_ADDR: u64 = 0x20000;

/// Maximum command line length.
pub const CMDLINE_MAX_SIZE: usize = 4096;

/// Default initrd load address (16 MiB — above kernel).
pub const INITRD_ADDR: u64 = 0x1000000; // 16 MiB

/// E820 memory map entry types.
pub const E820_RAM: u32 = 1;
pub const E820_RESERVED: u32 = 2;
pub const E820_ACPI: u32 = 3;

/// Maximum number of E820 entries in boot_params.
pub const E820_MAX_ENTRIES: usize = 128;

/// Offset of the setup header within a bzImage.
/// The magic "HdrS" is at offset 0x202 in the boot sector.
pub const SETUP_HEADER_OFFSET: usize = 0x1F1;

/// The magic value "HdrS" (0x53726448) at offset 0x202.
pub const HDRS_MAGIC: u32 = 0x5372_6448;

// ---------------------------------------------------------------------------
// 64-bit long mode boot layout
// ---------------------------------------------------------------------------

/// PML4 page table address (identity-mapped).
pub const PAGE_TABLE_ADDR: u64 = 0x9000;

/// Page Directory Pointer Table address.
pub const PDPT_ADDR: u64 = 0xA000;

/// Page Directory address (2 MiB huge pages).
pub const PD_ADDR: u64 = 0xB000;

/// Global Descriptor Table address.
pub const GDT_ADDR: u64 = 0x500;

/// Initial RSP value for 64-bit boot.
pub const BOOT_STACK_ADDR: u64 = 0x8000;
