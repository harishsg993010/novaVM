//! Boot pipeline integration tests.
//!
//! Tests the full boot flow: memory allocation -> kernel loading ->
//! command line setup -> boot parameter construction.

use nova_boot::{BootParams, BzImage, CmdlineBuilder, E820Entry};
use nova_mem::{GuestAddress, GuestMemoryMmap};

/// Test that we can allocate guest memory, build a command line, construct
/// boot parameters with E820, and write them to guest memory.
#[test]
fn test_full_boot_pipeline() {
    // 1. Allocate 128 MiB of guest memory.
    let mem_size = 128 * 1024 * 1024;
    let memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false).unwrap();

    // Verify memory is usable.
    let test_val: u64 = 0xDEAD_BEEF_CAFE_BABE;
    memory
        .write_obj(GuestAddress::new(0x1000), &test_val)
        .unwrap();
    let read_back: u64 = memory.read_obj(GuestAddress::new(0x1000)).unwrap();
    assert_eq!(read_back, test_val);

    // 2. Build a kernel command line.
    let cmdline = CmdlineBuilder::new()
        .arg("console", "ttyS0")
        .arg("reboot", "k")
        .arg("panic", "1")
        .flag("quiet");

    let cmdline_str = cmdline.build();
    assert!(cmdline_str.contains("console=ttyS0"));
    assert!(cmdline_str.contains("reboot=k"));
    assert!(cmdline_str.contains("panic=1"));
    assert!(cmdline_str.contains("quiet"));

    // 3. Write command line to guest memory.
    let mut boot_params = BootParams::new();
    cmdline.write_to_memory(&memory, &mut boot_params).unwrap();

    // Verify the command line was written at CMDLINE_ADDR.
    let mut buf = vec![0u8; cmdline_str.len()];
    memory
        .read_slice(GuestAddress::new(nova_boot::layout::CMDLINE_ADDR), &mut buf)
        .unwrap();
    assert_eq!(&buf, cmdline_str.as_bytes());

    // Verify boot_params has the cmdline pointer set.
    let cmd_ptr = { boot_params.setup_header().cmd_line_ptr };
    assert_eq!(cmd_ptr, nova_boot::layout::CMDLINE_ADDR as u32);

    // 4. Construct E820 memory map.
    boot_params.set_e820_entry(
        0,
        E820Entry {
            addr: 0,
            size: mem_size as u64,
            type_: nova_boot::layout::E820_RAM,
        },
    );
    boot_params.set_e820_entry(
        1,
        E820Entry {
            addr: 0xFEC0_0000,
            size: 0x1000,
            type_: nova_boot::layout::E820_RESERVED,
        },
    );
    boot_params.set_e820_count(2);
    assert_eq!(boot_params.e820_count(), 2);

    // 5. Write boot params to guest memory (zero page).
    let zero_page_addr = GuestAddress::new(nova_boot::layout::ZERO_PAGE_ADDR);
    let bp_bytes = boot_params.as_bytes();
    memory.write_slice(zero_page_addr, bp_bytes).unwrap();

    // Verify the zero page was written.
    let mut readback = vec![0u8; bp_bytes.len()];
    memory.read_slice(zero_page_addr, &mut readback).unwrap();
    assert_eq!(readback, bp_bytes);
}

/// Test bzImage header parsing combined with memory operations.
#[test]
fn test_bzimage_parse_and_memory_load() {
    // Construct a minimal fake bzImage.
    let setup_sects: u8 = 1;
    let total_setup = (1 + setup_sects as usize) * 512; // 1024 bytes
    let kernel_code = b"FAKE_KERNEL_CODE";
    let total_size = total_setup + kernel_code.len();
    let mut image = vec![0u8; total_size];

    // setup_sects at offset 0x1F1
    image[0x1F1] = setup_sects;
    // "HdrS" magic at offset 0x202
    image[0x202] = b'H';
    image[0x203] = b'd';
    image[0x204] = b'r';
    image[0x205] = b'S';
    // boot protocol version at 0x206 (2.16 = 0x0210 LE)
    image[0x206] = 0x10;
    image[0x207] = 0x02;
    // Boot flag at 0x1FE (0xAA55)
    image[0x1FE] = 0x55;
    image[0x1FF] = 0xAA;
    // Kernel code after setup
    image[total_setup..].copy_from_slice(kernel_code);

    let bz = BzImage::parse(std::io::Cursor::new(&image)).unwrap();
    assert_eq!(bz.protocol_version, 0x0210);
    assert_eq!(bz.setup_sects, 1);
    assert_eq!(&bz.kernel_code, kernel_code);
}

/// Test that guest memory supports multi-region layout (low + high).
#[test]
fn test_multi_region_guest_memory() {
    let memory = GuestMemoryMmap::new(
        &[
            (GuestAddress::new(0), 640 * 1024),                // 640 KiB low
            (GuestAddress::new(0x10_0000), 127 * 1024 * 1024), // 127 MiB high
        ],
        false,
    )
    .unwrap();

    assert_eq!(memory.num_regions(), 2);

    // Write to low memory.
    let low_val: u32 = 0x1234;
    memory
        .write_obj(GuestAddress::new(0x500), &low_val)
        .unwrap();
    let read: u32 = memory.read_obj(GuestAddress::new(0x500)).unwrap();
    assert_eq!(read, low_val);

    // Write to high memory.
    let high_val: u64 = 0xCAFE_BABE;
    memory
        .write_obj(GuestAddress::new(0x10_0000), &high_val)
        .unwrap();
    let read: u64 = memory.read_obj(GuestAddress::new(0x10_0000)).unwrap();
    assert_eq!(read, high_val);

    // Gap between regions should be unmapped.
    let mut buf = [0u8; 1];
    assert!(memory
        .read_slice(GuestAddress::new(0x9_FFFF + 1), &mut buf)
        .is_err());
}

/// Test boot params E820 entry bounds checking.
#[test]
fn test_boot_params_e820_bounds() {
    let mut bp = BootParams::new();

    // Valid entries should succeed.
    assert!(bp.set_e820_entry(
        0,
        E820Entry {
            addr: 0,
            size: 0x1000,
            type_: nova_boot::layout::E820_RAM,
        }
    ));
    assert!(bp.set_e820_entry(
        127,
        E820Entry {
            addr: 0,
            size: 0x1000,
            type_: nova_boot::layout::E820_RAM,
        }
    ));

    // Index 128 (out of bounds) should fail.
    assert!(!bp.set_e820_entry(
        128,
        E820Entry {
            addr: 0,
            size: 0x1000,
            type_: nova_boot::layout::E820_RAM,
        }
    ));
}
