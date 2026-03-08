use std::io::Read;

use crate::boot_params::BootParams;
use crate::error::{BootError, Result};
use crate::layout::*;
use nova_mem::{GuestAddress, GuestMemoryMmap};

/// Parsed bzImage information.
pub struct BzImage {
    /// The boot_params (zero page) populated from the image.
    pub boot_params: BootParams,
    /// The protected-mode kernel code.
    pub kernel_code: Vec<u8>,
    /// Boot protocol version.
    pub protocol_version: u16,
    /// Number of setup sectors.
    pub setup_sects: u8,
}

impl BzImage {
    /// Parse a bzImage from a reader.
    ///
    /// Reads the setup header, validates the magic, and separates
    /// the setup data from the protected-mode kernel code.
    pub fn parse<R: Read>(mut reader: R) -> Result<Self> {
        // Read the entire image into memory.
        let mut image = Vec::new();
        reader.read_to_end(&mut image)?;

        if image.len() < 0x268 {
            return Err(BootError::InvalidBzImageMagic(0));
        }

        // Check "HdrS" magic at offset 0x202.
        let magic = u32::from_le_bytes([image[0x202], image[0x203], image[0x204], image[0x205]]);
        if magic != HDRS_MAGIC {
            return Err(BootError::InvalidBzImageMagic(magic));
        }

        // Read boot protocol version at offset 0x206.
        let version = u16::from_le_bytes([image[0x206], image[0x207]]);
        if version < 0x0200 {
            return Err(BootError::OldBootProtocol(version));
        }

        // Number of setup sectors at offset 0x1F1 (0 means 4).
        let setup_sects = if image[0x1F1] == 0 { 4 } else { image[0x1F1] };

        // Setup size = (1 + setup_sects) * 512 (includes boot sector).
        let setup_size = (1 + setup_sects as usize) * 512;

        if image.len() < setup_size {
            return Err(BootError::KernelTooLarge {
                size: image.len(),
                load_addr: 0,
            });
        }

        // Build boot_params from the setup header.
        let mut boot_params = BootParams::new();
        let header_bytes = &image[SETUP_HEADER_OFFSET..setup_size.min(image.len())];
        boot_params.copy_setup_header_from(header_bytes);

        // The protected-mode kernel code starts after the setup sectors.
        let kernel_code = image[setup_size..].to_vec();

        tracing::info!(
            protocol_version = format!("{version:#06x}"),
            setup_sects,
            kernel_size = kernel_code.len(),
            "parsed bzImage"
        );

        Ok(BzImage {
            boot_params,
            kernel_code,
            protocol_version: version,
            setup_sects,
        })
    }

    /// Load the parsed bzImage into guest memory.
    ///
    /// - Writes boot_params to ZERO_PAGE_ADDR
    /// - Writes the protected-mode kernel to KERNEL_LOAD_ADDR
    /// - Sets up the command line pointer and loader type
    pub fn load_into_memory(&mut self, mem: &GuestMemoryMmap) -> Result<()> {
        // Set loader type (0xFF = undefined, but marks that there is a boot loader).
        let header = self.boot_params.setup_header_mut();
        header.type_of_loader = 0xFF;

        // Set the loaded-high flag (bit 0 of loadflags) so kernel loads at 1MiB.
        header.loadflags |= 0x01;

        // Write the protected-mode kernel code to 1 MiB.
        mem.write_slice(GuestAddress::new(KERNEL_LOAD_ADDR), &self.kernel_code)
            .map_err(|e| {
                tracing::error!(error = %e, "failed to write kernel code to guest memory");
                BootError::KernelTooLarge {
                    size: self.kernel_code.len(),
                    load_addr: KERNEL_LOAD_ADDR,
                }
            })?;

        // Write boot_params (zero page) to 0x7000.
        mem.write_slice(
            GuestAddress::new(ZERO_PAGE_ADDR),
            self.boot_params.as_bytes(),
        )?;

        tracing::info!(
            kernel_addr = KERNEL_LOAD_ADDR,
            zero_page_addr = ZERO_PAGE_ADDR,
            "loaded bzImage into guest memory"
        );

        Ok(())
    }
}
