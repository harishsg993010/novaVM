//! OCI layer extraction with whiteout handling.
//!
//! Extracts tar.gz layer blobs into a flattened rootfs directory,
//! processing OCI whiteout files to handle deletions between layers.

use std::fs;
use std::path::Path;

use flate2::read::GzDecoder;
use tar::Archive;

use crate::error::{Result, RuntimeError};
use crate::image::oci::OciDescriptor;

/// Extract OCI layers into a single rootfs directory.
///
/// Layers are applied in order (index 0 = base layer, applied first).
/// OCI whiteout files are handled:
/// - `.wh.<name>` → deletes the named file/dir from output
/// - `.wh..wh..opq` → clears parent directory contents (opaque whiteout)
pub fn extract_layers(
    layout_dir: &Path,
    layers: &[OciDescriptor],
    output_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(output_dir)?;

    for (i, layer) in layers.iter().enumerate() {
        let blob_path = crate::image::oci::OciImageLayout::descriptor_to_blob_path_static(
            layout_dir, layer,
        );

        tracing::debug!(
            layer = i,
            digest = %layer.digest,
            "extracting layer"
        );

        let file = fs::File::open(&blob_path).map_err(|e| {
            RuntimeError::Image(format!(
                "failed to open layer blob {}: {e}",
                blob_path.display()
            ))
        })?;

        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        // First pass: collect whiteout entries, extract normal entries.
        let mut whiteouts: Vec<std::path::PathBuf> = Vec::new();
        let mut opaque_dirs: Vec<std::path::PathBuf> = Vec::new();

        for entry_result in archive.entries().map_err(|e| {
            RuntimeError::Image(format!("failed to read tar entries: {e}"))
        })? {
            let mut entry = entry_result.map_err(|e| {
                RuntimeError::Image(format!("failed to read tar entry: {e}"))
            })?;

            let path = entry.path().map_err(|e| {
                RuntimeError::Image(format!("invalid tar entry path: {e}"))
            })?;
            let path = path.to_path_buf();

            // Check for whiteout entries.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == ".wh..wh..opq" {
                    // Opaque whiteout: clear parent directory.
                    if let Some(parent) = path.parent() {
                        opaque_dirs.push(parent.to_path_buf());
                    }
                    continue;
                }

                if let Some(target) = name.strip_prefix(".wh.") {
                    // Regular whiteout: delete the target.
                    if let Some(parent) = path.parent() {
                        whiteouts.push(parent.join(target));
                    } else {
                        whiteouts.push(std::path::PathBuf::from(target));
                    }
                    continue;
                }
            }

            // Normal entry — extract it, preserving file permissions (execute bits).
            entry.set_preserve_permissions(true);
            entry.unpack_in(output_dir).map_err(|e| {
                RuntimeError::Image(format!(
                    "failed to extract {}: {e}",
                    path.display()
                ))
            })?;
        }

        // Process opaque whiteouts: clear directory contents.
        for opaque_dir in &opaque_dirs {
            let full_path = output_dir.join(opaque_dir);
            if full_path.is_dir() {
                // Remove all existing contents but keep the directory.
                for entry in fs::read_dir(&full_path).into_iter().flatten() {
                    if let Ok(entry) = entry {
                        let p = entry.path();
                        if p.is_dir() {
                            let _ = fs::remove_dir_all(&p);
                        } else {
                            let _ = fs::remove_file(&p);
                        }
                    }
                }
            }
        }

        // Process regular whiteouts: delete targets.
        for whiteout in &whiteouts {
            let full_path = output_dir.join(whiteout);
            if full_path.is_dir() {
                let _ = fs::remove_dir_all(&full_path);
            } else if full_path.exists() {
                let _ = fs::remove_file(&full_path);
            }
        }

        tracing::debug!(
            layer = i,
            whiteouts = whiteouts.len(),
            opaque_dirs = opaque_dirs.len(),
            "layer extracted"
        );
    }

    Ok(())
}
