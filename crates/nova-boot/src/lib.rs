//! # nova-boot
//!
//! Linux kernel loading for NovaVM.
//!
//! Supports:
//! - **bzImage**: Standard compressed kernel image (setup header parsing)
//! - **ELF**: vmlinux direct ELF loading
//! - **PVH**: Xen PVH direct boot protocol
//! - **Initrd**: Initramfs loading with optional agent injection
//! - **Cmdline**: Kernel command line builder

pub mod boot_params;
pub mod bzimage;
pub mod cmdline;
pub mod cpu_setup;
pub mod elf;
pub mod error;
pub mod initrd;
pub mod layout;
pub mod pvh;

pub use boot_params::{BootParams, E820Entry, SetupHeader};
pub use bzimage::BzImage;
pub use cmdline::CmdlineBuilder;
pub use elf::ElfKernel;
pub use error::{BootError, Result};

#[cfg(test)]
mod tests {
    use super::*;
    use nova_mem::{GuestAddress, GuestMemoryMmap};

    /// Create a minimal fake bzImage for testing.
    /// Layout: 1 boot sector (512 bytes) + setup header + kernel code.
    fn make_fake_bzimage() -> Vec<u8> {
        let setup_sects: u8 = 1; // 1 setup sector
        let total_setup = (1 + setup_sects as usize) * 512; // 1024 bytes
        let kernel_code = b"FAKE_KERNEL_CODE_HERE_1234567890";
        let total_size = total_setup + kernel_code.len();
        let mut image = vec![0u8; total_size];

        // setup_sects at offset 0x1F1.
        image[0x1F1] = setup_sects;

        // "HdrS" magic at offset 0x202.
        image[0x202] = 0x48; // H
        image[0x203] = 0x64; // d
        image[0x204] = 0x72; // r
        image[0x205] = 0x53; // S

        // Boot protocol version at 0x206 (2.15 = 0x020F).
        image[0x206] = 0x0F;
        image[0x207] = 0x02;

        // Boot flag at 0x1FE (0xAA55).
        image[0x1FE] = 0x55;
        image[0x1FF] = 0xAA;

        // Write kernel code after setup.
        image[total_setup..].copy_from_slice(kernel_code);

        image
    }

    #[test]
    fn test_bzimage_parse_header() {
        let image = make_fake_bzimage();
        let bz = BzImage::parse(std::io::Cursor::new(&image)).expect("parse failed");

        assert_eq!(bz.protocol_version, 0x020F);
        assert_eq!(bz.setup_sects, 1);
        assert_eq!(bz.kernel_code, b"FAKE_KERNEL_CODE_HERE_1234567890");
    }

    #[test]
    fn test_bzimage_invalid_magic() {
        let mut image = make_fake_bzimage();
        // Corrupt the magic.
        image[0x202] = 0x00;
        let result = BzImage::parse(std::io::Cursor::new(&image));
        assert!(result.is_err());
    }

    #[test]
    fn test_boot_params_e820() {
        let mut bp = BootParams::new();

        let entries = [
            E820Entry {
                addr: 0,
                size: 0x9FC00,
                type_: layout::E820_RAM,
            },
            E820Entry {
                addr: 0x100000,
                size: 0x7EF00000,
                type_: layout::E820_RAM,
            },
            E820Entry {
                addr: 0xF0000000,
                size: 0x10000000,
                type_: layout::E820_RESERVED,
            },
        ];

        for (i, entry) in entries.iter().enumerate() {
            assert!(bp.set_e820_entry(i, *entry));
        }
        bp.set_e820_count(entries.len() as u8);

        assert_eq!(bp.e820_count(), 3);
    }

    #[test]
    fn test_bzimage_load_into_memory() {
        let image = make_fake_bzimage();
        let mut bz = BzImage::parse(std::io::Cursor::new(&image)).expect("parse failed");

        // Create guest memory large enough for kernel at 1MiB.
        let mem = GuestMemoryMmap::new(
            &[
                (GuestAddress::new(0), 1024 * 1024),            // 0-1MiB
                (GuestAddress::new(0x100000), 4 * 1024 * 1024), // 1MiB-5MiB
            ],
            false,
        )
        .expect("failed to create guest memory");

        bz.load_into_memory(&mem).expect("load failed");

        // Verify kernel code was written at 1MiB.
        let mut buf = vec![0u8; bz.kernel_code.len()];
        mem.read_slice(GuestAddress::new(layout::KERNEL_LOAD_ADDR), &mut buf)
            .expect("read failed");
        assert_eq!(buf, b"FAKE_KERNEL_CODE_HERE_1234567890");

        // Verify boot_params were written at zero page.
        let mut zero_page = [0u8; 4096];
        mem.read_slice(GuestAddress::new(layout::ZERO_PAGE_ADDR), &mut zero_page)
            .expect("read zero page failed");
        // Check type_of_loader was set.
        assert_eq!(zero_page[0x210], 0xFF);
    }

    #[test]
    fn test_cmdline_builder() {
        let cmdline = CmdlineBuilder::new()
            .arg("console", "ttyS0")
            .arg("root", "/dev/vda")
            .flag("ro")
            .flag("quiet")
            .build();

        assert_eq!(cmdline, "console=ttyS0 root=/dev/vda ro quiet");
    }

    #[test]
    fn test_cmdline_write_to_memory() {
        let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 256 * 1024)], false)
            .expect("failed to create guest memory");

        let mut bp = BootParams::new();
        CmdlineBuilder::new()
            .arg("console", "ttyS0")
            .write_to_memory(&mem, &mut bp)
            .expect("write cmdline failed");

        // Verify cmdline was written.
        let mut buf = [0u8; 32];
        mem.read_slice(GuestAddress::new(layout::CMDLINE_ADDR), &mut buf)
            .expect("read failed");
        let cmdline = std::str::from_utf8(&buf).unwrap().trim_end_matches('\0');
        assert_eq!(cmdline, "console=ttyS0");

        // Verify boot_params pointer (copy from packed struct to avoid unaligned ref).
        let cmd_ptr = { bp.setup_header().cmd_line_ptr };
        assert_eq!(cmd_ptr, layout::CMDLINE_ADDR as u32);
    }

    #[test]
    fn test_elf_invalid_magic() {
        let data = vec![0u8; 128];
        let result = ElfKernel::parse(std::io::Cursor::new(&data));
        assert!(result.is_err());
    }

    #[test]
    fn test_initrd_agent_injection() {
        let mut initrd = Vec::new();
        let agent = b"#!/bin/sh\necho hello\n";
        initrd::inject_agent(&mut initrd, agent);

        // Verify the cpio header magic "070701" is present.
        let header_str = std::str::from_utf8(&initrd[..6]).unwrap();
        assert_eq!(header_str, "070701");

        // Verify the filename is embedded.
        let initrd_str = String::from_utf8_lossy(&initrd);
        assert!(initrd_str.contains("sbin/nova-agent"));
        assert!(initrd_str.contains("TRAILER!!!"));
    }
}
