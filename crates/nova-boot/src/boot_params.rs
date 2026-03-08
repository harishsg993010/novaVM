//! x86 Linux boot protocol structures.
//! Based on linux/arch/x86/include/uapi/asm/bootparam.h

/// An E820 memory map entry.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct E820Entry {
    pub addr: u64,
    pub size: u64,
    pub type_: u32,
}

/// The setup_header structure within boot_params.
/// This is a subset — we include only the fields we need to read/write.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct SetupHeader {
    pub setup_sects: u8,            // 0x1F1
    pub root_flags: u16,            // 0x1F2
    pub syssize: u32,               // 0x1F4
    pub ram_size: u16,              // 0x1F8
    pub vid_mode: u16,              // 0x1FA
    pub root_dev: u16,              // 0x1FC
    pub boot_flag: u16,             // 0x1FE (should be 0xAA55)
    pub jump: u16,                  // 0x200
    pub header: u32,                // 0x202 — "HdrS" magic
    pub version: u16,               // 0x206
    pub realmode_swtch: u32,        // 0x208
    pub start_sys_seg: u16,         // 0x20C
    pub kernel_version: u16,        // 0x20E
    pub type_of_loader: u8,         // 0x210
    pub loadflags: u8,              // 0x211
    pub setup_move_size: u16,       // 0x212
    pub code32_start: u32,          // 0x214
    pub ramdisk_image: u32,         // 0x218
    pub ramdisk_size: u32,          // 0x21C
    pub bootsect_kludge: u32,       // 0x220
    pub heap_end_ptr: u16,          // 0x224
    pub ext_loader_ver: u8,         // 0x226
    pub ext_loader_type: u8,        // 0x227
    pub cmd_line_ptr: u32,          // 0x228
    pub initrd_addr_max: u32,       // 0x22C
    pub kernel_alignment: u32,      // 0x230
    pub relocatable_kernel: u8,     // 0x234
    pub min_alignment: u8,          // 0x235
    pub xloadflags: u16,            // 0x236
    pub cmdline_size: u32,          // 0x238
    pub hardware_subarch: u32,      // 0x23C
    pub hardware_subarch_data: u64, // 0x240
    pub payload_offset: u32,        // 0x248
    pub payload_length: u32,        // 0x24C
    pub setup_data: u64,            // 0x250
    pub pref_address: u64,          // 0x258
    pub init_size: u32,             // 0x260
    pub handover_offset: u32,       // 0x264
}

impl Default for SetupHeader {
    fn default() -> Self {
        // SAFETY: SetupHeader is all-zeros valid.
        unsafe { std::mem::zeroed() }
    }
}

/// The full boot_params structure (the "zero page").
/// We represent it as the 4096-byte page with typed access to known fields.
#[repr(C, align(4096))]
pub struct BootParams {
    /// Raw bytes of the zero page. We overlay specific fields.
    data: [u8; 4096],
}

impl Default for BootParams {
    fn default() -> Self {
        Self { data: [0u8; 4096] }
    }
}

impl BootParams {
    /// Create a new zero-initialized boot_params.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a reference to the setup header (starts at offset 0x1F1).
    pub fn setup_header(&self) -> &SetupHeader {
        // SAFETY: Offset 0x1F1 within the 4096-byte page is valid,
        // and SetupHeader fits within the remaining space.
        unsafe { &*(self.data.as_ptr().add(0x1F1).cast::<SetupHeader>()) }
    }

    /// Get a mutable reference to the setup header.
    pub fn setup_header_mut(&mut self) -> &mut SetupHeader {
        // SAFETY: Same as above, but mutable.
        unsafe { &mut *(self.data.as_mut_ptr().add(0x1F1).cast::<SetupHeader>()) }
    }

    /// Set an E820 memory map entry. Returns false if index >= 128.
    pub fn set_e820_entry(&mut self, index: usize, entry: E820Entry) -> bool {
        if index >= super::layout::E820_MAX_ENTRIES {
            return false;
        }
        // E820 table starts at offset 0x2D0, each entry is 20 bytes.
        let offset = 0x2D0 + index * 20;
        // SAFETY: We checked bounds; offset + 20 <= 4096 for index < 128.
        unsafe {
            let ptr = self.data.as_mut_ptr().add(offset);
            std::ptr::copy_nonoverlapping(&entry as *const E820Entry as *const u8, ptr, 20);
        }
        true
    }

    /// Set the E820 entry count (at offset 0x1E8).
    pub fn set_e820_count(&mut self, count: u8) {
        self.data[0x1E8] = count;
    }

    /// Get the E820 entry count.
    pub fn e820_count(&self) -> u8 {
        self.data[0x1E8]
    }

    /// Returns the raw bytes of the boot_params for writing to guest memory.
    pub fn as_bytes(&self) -> &[u8; 4096] {
        &self.data
    }

    /// Write the setup header from a kernel image's header bytes.
    pub fn copy_setup_header_from(&mut self, header_bytes: &[u8]) {
        let copy_len = header_bytes.len().min(4096 - 0x1F1);
        self.data[0x1F1..0x1F1 + copy_len].copy_from_slice(&header_bytes[..copy_len]);
    }
}
