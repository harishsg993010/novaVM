use std::io::Read;

use crate::error::{BootError, Result};
use nova_mem::{GuestAddress, GuestMemoryMmap};

/// ELF magic bytes.
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELF class: 64-bit.
const ELFCLASS64: u8 = 2;

/// ELF type: executable.
const ET_EXEC: u16 = 2;

/// ELF program header type: loadable segment.
const PT_LOAD: u32 = 1;

/// ELF note type for PVH entry point.
const XEN_ELFNOTE_PHYS_ENTRY: u32 = 18;

/// A loadable segment parsed from an ELF file.
#[derive(Debug)]
pub struct LoadSegment {
    /// Guest physical address to load at.
    pub guest_addr: u64,
    /// File offset of the segment data.
    pub file_offset: usize,
    /// Size of the segment in the file.
    pub file_size: usize,
    /// Size of the segment in memory (may be larger than file_size for BSS).
    pub mem_size: usize,
}

/// Parsed ELF kernel image.
pub struct ElfKernel {
    /// Raw image bytes.
    pub image: Vec<u8>,
    /// Entry point address.
    pub entry_point: u64,
    /// Loadable segments.
    pub segments: Vec<LoadSegment>,
    /// Optional PVH entry point from ELF notes.
    pub pvh_entry: Option<u64>,
}

impl ElfKernel {
    /// Parse a vmlinux ELF kernel image.
    pub fn parse<R: Read>(mut reader: R) -> Result<Self> {
        let mut image = Vec::new();
        reader.read_to_end(&mut image)?;

        if image.len() < 64 {
            return Err(BootError::InvalidElfMagic);
        }

        // Check ELF magic.
        if image[0..4] != ELF_MAGIC {
            return Err(BootError::InvalidElfMagic);
        }

        // Check 64-bit.
        if image[4] != ELFCLASS64 {
            return Err(BootError::Not64BitElf(image[4]));
        }

        // Parse ELF64 header fields.
        let e_type = u16::from_le_bytes([image[16], image[17]]);
        if e_type != ET_EXEC {
            return Err(BootError::NotExecutableElf(e_type));
        }

        let entry_point = u64::from_le_bytes(image[24..32].try_into().unwrap());
        let ph_offset = u64::from_le_bytes(image[32..40].try_into().unwrap()) as usize;
        let ph_entry_size = u16::from_le_bytes([image[54], image[55]]) as usize;
        let ph_count = u16::from_le_bytes([image[56], image[57]]) as usize;

        // Parse program headers for PT_LOAD segments.
        let mut segments = Vec::new();
        for i in 0..ph_count {
            let offset = ph_offset + i * ph_entry_size;
            if offset + ph_entry_size > image.len() {
                break;
            }
            let p_type = u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap());
            if p_type != PT_LOAD {
                continue;
            }
            let p_offset =
                u64::from_le_bytes(image[offset + 8..offset + 16].try_into().unwrap()) as usize;
            let p_vaddr = u64::from_le_bytes(image[offset + 16..offset + 24].try_into().unwrap());
            let p_paddr = u64::from_le_bytes(image[offset + 24..offset + 32].try_into().unwrap());
            let p_filesz =
                u64::from_le_bytes(image[offset + 32..offset + 40].try_into().unwrap()) as usize;
            let p_memsz =
                u64::from_le_bytes(image[offset + 40..offset + 48].try_into().unwrap()) as usize;

            // Use physical address if non-zero, otherwise virtual.
            let guest_addr = if p_paddr != 0 { p_paddr } else { p_vaddr };

            segments.push(LoadSegment {
                guest_addr,
                file_offset: p_offset,
                file_size: p_filesz,
                mem_size: p_memsz,
            });
        }

        if segments.is_empty() {
            return Err(BootError::NoLoadableSegments);
        }

        // Scan section headers for PVH note.
        let pvh_entry = Self::find_pvh_entry(&image);

        tracing::info!(
            entry_point = format!("{entry_point:#x}"),
            segments = segments.len(),
            pvh = pvh_entry.is_some(),
            "parsed ELF kernel"
        );

        Ok(ElfKernel {
            image,
            entry_point,
            segments,
            pvh_entry,
        })
    }

    /// Load the ELF segments into guest memory.
    pub fn load_into_memory(&self, mem: &GuestMemoryMmap) -> Result<()> {
        for seg in &self.segments {
            // Write file data.
            if seg.file_size > 0 {
                let end = seg.file_offset + seg.file_size;
                if end > self.image.len() {
                    return Err(BootError::KernelTooLarge {
                        size: end,
                        load_addr: seg.guest_addr,
                    });
                }
                mem.write_slice(
                    GuestAddress::new(seg.guest_addr),
                    &self.image[seg.file_offset..end],
                )?;
            }
            // Zero-fill BSS (mem_size > file_size).
            if seg.mem_size > seg.file_size {
                let bss_size = seg.mem_size - seg.file_size;
                let bss_addr = seg.guest_addr + seg.file_size as u64;
                let zeros = vec![0u8; bss_size];
                mem.write_slice(GuestAddress::new(bss_addr), &zeros)?;
            }
        }
        Ok(())
    }

    /// Scan ELF notes for a PVH entry point (XEN_ELFNOTE_PHYS_ENTRY).
    fn find_pvh_entry(image: &[u8]) -> Option<u64> {
        if image.len() < 64 {
            return None;
        }

        // Parse section headers to find NOTE sections.
        let sh_offset = u64::from_le_bytes(image[40..48].try_into().ok()?) as usize;
        let sh_entry_size = u16::from_le_bytes([image[58], image[59]]) as usize;
        let sh_count = u16::from_le_bytes([image[60], image[61]]) as usize;

        // SHT_NOTE = 7
        for i in 0..sh_count {
            let offset = sh_offset + i * sh_entry_size;
            if offset + sh_entry_size > image.len() {
                break;
            }
            let sh_type = u32::from_le_bytes(image[offset + 4..offset + 8].try_into().ok()?);
            if sh_type != 7 {
                continue;
            }
            let note_offset =
                u64::from_le_bytes(image[offset + 24..offset + 32].try_into().ok()?) as usize;
            let note_size =
                u64::from_le_bytes(image[offset + 32..offset + 40].try_into().ok()?) as usize;

            // Parse notes in this section.
            let mut pos = note_offset;
            while pos + 12 <= note_offset + note_size && pos + 12 <= image.len() {
                let namesz = u32::from_le_bytes(image[pos..pos + 4].try_into().ok()?) as usize;
                let descsz = u32::from_le_bytes(image[pos + 4..pos + 8].try_into().ok()?) as usize;
                let note_type = u32::from_le_bytes(image[pos + 8..pos + 12].try_into().ok()?);

                let name_start = pos + 12;
                let name_end = name_start + ((namesz + 3) & !3); // align to 4
                let desc_start = name_end;
                let desc_end = desc_start + ((descsz + 3) & !3);

                if note_type == XEN_ELFNOTE_PHYS_ENTRY
                    && descsz >= 8
                    && desc_start + 8 <= image.len()
                {
                    let entry =
                        u64::from_le_bytes(image[desc_start..desc_start + 8].try_into().ok()?);
                    return Some(entry);
                }

                pos = desc_end;
            }
        }

        None
    }
}
