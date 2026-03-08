use std::io::{self, Read};
use std::path::Path;

use crate::boot_params::BootParams;
use crate::error::{BootError, Result};
use crate::layout::INITRD_ADDR;
use nova_mem::{GuestAddress, GuestMemoryMmap};

/// Load an initrd/initramfs into guest memory.
///
/// Updates the boot_params to reflect the initrd location and size.
pub fn load_initrd<R: Read>(
    reader: &mut R,
    mem: &GuestMemoryMmap,
    boot_params: &mut BootParams,
) -> Result<()> {
    load_initrd_at(reader, mem, boot_params, INITRD_ADDR)
}

/// Load an initrd/initramfs at a specific guest address.
pub fn load_initrd_at<R: Read>(
    reader: &mut R,
    mem: &GuestMemoryMmap,
    boot_params: &mut BootParams,
    load_addr: u64,
) -> Result<()> {
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;

    if data.len() > 512 * 1024 * 1024 {
        return Err(BootError::InitrdTooLarge { size: data.len() });
    }

    mem.write_slice(GuestAddress::new(load_addr), &data)?;

    // Update boot_params setup header.
    let header = boot_params.setup_header_mut();
    header.ramdisk_image = load_addr as u32;
    header.ramdisk_size = data.len() as u32;

    tracing::info!(
        addr = load_addr,
        size = data.len(),
        "loaded initrd into guest memory"
    );

    Ok(())
}

/// Inject a guest agent binary into an existing initrd (cpio archive).
///
/// Creates a simple cpio entry appended with the agent binary,
/// which will be extracted to `/sbin/nova-agent` inside the guest.
pub fn inject_agent(initrd_data: &mut Vec<u8>, agent_binary: &[u8]) {
    // Create a cpio "newc" format entry for /sbin/nova-agent.
    let filename = b"sbin/nova-agent\0";
    let header = format!(
        "070701\
         00000000\
         000081ED\
         00000000\
         00000000\
         00000001\
         00000000\
         {:08X}\
         00000000\
         00000000\
         00000000\
         00000000\
         {:08X}\
         00000000",
        agent_binary.len(),
        filename.len()
    );

    initrd_data.extend_from_slice(header.as_bytes());
    // Pad header to 4-byte boundary.
    let header_total = 110 + filename.len();
    let padding = (4 - (header_total % 4)) % 4;
    initrd_data.extend(std::iter::repeat(0u8).take(padding));

    initrd_data.extend_from_slice(filename);
    // Pad filename to 4-byte boundary (already included in header_total).

    initrd_data.extend_from_slice(agent_binary);
    // Pad data to 4-byte boundary.
    let data_padding = (4 - (agent_binary.len() % 4)) % 4;
    initrd_data.extend(std::iter::repeat(0u8).take(data_padding));

    // Append TRAILER entry.
    let trailer_name = b"TRAILER!!!\0";
    let trailer_header = format!(
        "070701\
         00000000\
         00000000\
         00000000\
         00000000\
         00000001\
         00000000\
         00000000\
         00000000\
         00000000\
         00000000\
         00000000\
         {:08X}\
         00000000",
        trailer_name.len()
    );
    initrd_data.extend_from_slice(trailer_header.as_bytes());
    let trailer_total = 110 + trailer_name.len();
    let trailer_padding = (4 - (trailer_total % 4)) % 4;
    initrd_data.extend(std::iter::repeat(0u8).take(trailer_padding));
    initrd_data.extend_from_slice(trailer_name);
    let name_padding = (4 - (trailer_name.len() % 4)) % 4;
    initrd_data.extend(std::iter::repeat(0u8).take(name_padding));
}

/// Convert a directory tree into a cpio "newc" format archive.
///
/// Walks the directory recursively, creating cpio entries for directories,
/// regular files, and symlinks. Returns the complete cpio archive as bytes.
pub fn dir_to_cpio(root_dir: &Path) -> io::Result<Vec<u8>> {
    let mut cpio = Vec::new();
    let mut ino: u32 = 1;

    // Collect and sort entries for deterministic output.
    let mut entries = Vec::new();
    collect_entries(root_dir, root_dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel_path, full_path) in &entries {
        let meta = std::fs::symlink_metadata(full_path)?;

        if meta.is_dir() {
            write_cpio_entry(&mut cpio, rel_path, 0o040755, &[], ino)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(full_path)?;
            let target_bytes = target.to_string_lossy().into_owned().into_bytes();
            write_cpio_entry(&mut cpio, rel_path, 0o120777, &target_bytes, ino)?;
        } else {
            // Regular file — preserve execute permission from the host filesystem.
            let data = std::fs::read(full_path)?;
            let mode = host_file_mode(&meta, rel_path);
            write_cpio_entry(&mut cpio, rel_path, mode, &data, ino)?;
        }

        ino += 1;
    }

    // Write TRAILER entry.
    write_cpio_entry(&mut cpio, "TRAILER!!!", 0, &[], 0)?;

    Ok(cpio)
}

/// Generate a simple init script.
///
/// Creates `#!/bin/sh\nexec cmd args...\n` suitable for use as `/init`.
pub fn create_init_script(cmd: &[&str]) -> Vec<u8> {
    let mut script = String::from("#!/bin/sh\nexec");
    for arg in cmd {
        script.push(' ');
        script.push_str(arg);
    }
    script.push('\n');
    script.into_bytes()
}

/// Inject a single file into a cpio archive buffer.
///
/// Appends a cpio entry with a TRAILER to the buffer. This is useful for
/// adding files (like /init) to an existing cpio archive — the Linux kernel
/// processes concatenated cpio archives.
pub fn inject_file(cpio: &mut Vec<u8>, path: &str, data: &[u8], mode: u32) {
    // Write the file entry.
    write_cpio_entry(cpio, path, mode, data, 1)
        .expect("writing to Vec should not fail");

    // Append TRAILER.
    write_cpio_entry(cpio, "TRAILER!!!", 0, &[], 0)
        .expect("writing to Vec should not fail");
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn collect_entries(
    root: &Path,
    current: &Path,
    entries: &mut Vec<(String, std::path::PathBuf)>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let full_path = entry.path();
        let rel = full_path
            .strip_prefix(root)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let meta = std::fs::symlink_metadata(&full_path)?;

        if meta.is_dir() {
            entries.push((rel_str.clone(), full_path.clone()));
            collect_entries(root, &full_path, entries)?;
        } else {
            entries.push((rel_str, full_path));
        }
    }
    Ok(())
}

/// Determine the cpio mode for a regular file.
///
/// Uses the host filesystem's execute bit when available (Unix),
/// falling back to path-based heuristics on other platforms.
#[cfg(unix)]
fn host_file_mode(meta: &std::fs::Metadata, _path: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let perm = meta.permissions().mode();
    // Preserve the full permission bits, combine with S_IFREG.
    0o100000 | (perm & 0o7777)
}

#[cfg(not(unix))]
fn host_file_mode(_meta: &std::fs::Metadata, path: &str) -> u32 {
    if path.starts_with("bin/")
        || path.starts_with("sbin/")
        || path.starts_with("usr/bin/")
        || path.starts_with("usr/sbin/")
        || path.starts_with("usr/local/bin/")
        || path.starts_with("usr/local/sbin/")
        || path.starts_with("lib/")
        || path.starts_with("usr/lib/")
        || path == "init"
        || path == "linuxrc"
    {
        0o100755
    } else {
        0o100644
    }
}

/// Write a single cpio "newc" entry.
///
/// Format: 110-byte header + filename (NUL-terminated) + pad to 4 + data + pad to 4.
fn write_cpio_entry(
    buf: &mut Vec<u8>,
    name: &str,
    mode: u32,
    data: &[u8],
    ino: u32,
) -> io::Result<()> {
    // Filename must be NUL-terminated in cpio.
    let namesize = name.len() + 1; // +1 for NUL

    // Write the 110-byte header.
    let header = format!(
        "070701\
         {:08X}\
         {:08X}\
         00000000\
         00000000\
         00000001\
         00000000\
         {:08X}\
         00000000\
         00000000\
         00000000\
         00000000\
         {:08X}\
         00000000",
        ino,
        mode,
        data.len(),
        namesize,
    );
    debug_assert_eq!(header.len(), 110);

    buf.extend_from_slice(header.as_bytes());

    // Write filename + NUL.
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);

    // Pad (header + name) to 4-byte boundary.
    let header_plus_name = 110 + namesize;
    let pad = (4 - (header_plus_name % 4)) % 4;
    buf.extend(std::iter::repeat(0u8).take(pad));

    // Write file data.
    buf.extend_from_slice(data);

    // Pad data to 4-byte boundary.
    let data_pad = (4 - (data.len() % 4)) % 4;
    buf.extend(std::iter::repeat(0u8).take(data_pad));

    Ok(())
}
