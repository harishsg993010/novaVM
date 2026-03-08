//! OCI image pulling and conversion.
//!
//! Pulls container images from OCI registries, extracts layers, and
//! converts them into rootfs images suitable for microVM boot.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Result;

use super::blob_store::BlobStore;
use super::rootfs_cache::RootfsCache;

/// Supported rootfs image formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    /// Raw ext4 filesystem image.
    Raw,
    /// QCOW2 disk image.
    Qcow2,
    /// cpio initramfs archive.
    Initramfs,
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw => write!(f, "raw"),
            Self::Qcow2 => write!(f, "qcow2"),
            Self::Initramfs => write!(f, "initramfs"),
        }
    }
}

/// Information about a pulled image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    /// OCI image reference.
    pub image_ref: String,
    /// SHA-256 digest of the image content.
    pub digest: String,
    /// Local path to the rootfs file.
    pub rootfs_path: PathBuf,
    /// Image format.
    pub format: ImageFormat,
    /// Total size in bytes.
    pub size_bytes: u64,
    /// OCI container config (if pulled from an OCI layout).
    #[serde(skip)]
    pub config: Option<super::oci::OciConfig>,
}

/// Pulls OCI images and converts them to rootfs images.
pub struct ImagePuller {
    /// Base directory for storing images.
    image_dir: PathBuf,
    /// Target image format.
    format: ImageFormat,
    /// Optional L1 blob cache.
    blob_store: Option<BlobStore>,
    /// Optional L2 rootfs cache.
    rootfs_cache: Option<RootfsCache>,
}

impl ImagePuller {
    /// Create a new image puller.
    pub fn new(image_dir: &Path, format: ImageFormat) -> Result<Self> {
        fs::create_dir_all(image_dir)?;
        tracing::info!(dir = %image_dir.display(), "initialized image puller");
        Ok(Self {
            image_dir: image_dir.to_path_buf(),
            format,
            blob_store: None,
            rootfs_cache: None,
        })
    }

    /// Create a puller with a blob store for L1 caching.
    pub fn with_blob_store(
        image_dir: &Path,
        format: ImageFormat,
        blob_store: BlobStore,
    ) -> Result<Self> {
        fs::create_dir_all(image_dir)?;
        Ok(Self {
            image_dir: image_dir.to_path_buf(),
            format,
            blob_store: Some(blob_store),
            rootfs_cache: None,
        })
    }

    /// Create a puller with both L1 blob and L2 rootfs caches.
    pub fn with_caches(
        image_dir: &Path,
        format: ImageFormat,
        blob_store: BlobStore,
        rootfs_cache: RootfsCache,
    ) -> Result<Self> {
        fs::create_dir_all(image_dir)?;
        Ok(Self {
            image_dir: image_dir.to_path_buf(),
            format,
            blob_store: Some(blob_store),
            rootfs_cache: Some(rootfs_cache),
        })
    }

    /// Pull an OCI image and convert it to a rootfs.
    ///
    /// In a full implementation, this would:
    /// 1. Resolve the image reference to a manifest
    /// 2. Download each layer blob
    /// 3. Extract and flatten layers into a rootfs
    /// 4. Create an ext4/qcow2 image from the rootfs
    ///
    /// This implementation creates a minimal rootfs structure for testing.
    pub fn pull(&self, image_ref: &str) -> Result<ImageInfo> {
        tracing::info!(image_ref, "pulling image");

        // Compute a deterministic directory name from the image ref.
        let mut hasher = Sha256::new();
        hasher.update(image_ref.as_bytes());
        let digest = hex::encode(hasher.finalize());
        let short_digest = &digest[..12];

        let image_name = image_ref
            .rsplit('/')
            .next()
            .unwrap_or(image_ref)
            .replace(':', "_");

        let rootfs_dir = self.image_dir.join(format!("{image_name}_{short_digest}"));
        fs::create_dir_all(&rootfs_dir)?;

        // Create a minimal rootfs directory structure.
        for dir in ["bin", "dev", "etc", "proc", "sys", "tmp", "var", "run"] {
            fs::create_dir_all(rootfs_dir.join(dir))?;
        }

        // Write a minimal /etc/hostname.
        let hostname_path = rootfs_dir.join("etc/hostname");
        let mut f = fs::File::create(&hostname_path)?;
        f.write_all(b"nova-guest\n")?;

        // Write a minimal /etc/resolv.conf.
        let resolv_path = rootfs_dir.join("etc/resolv.conf");
        let mut f = fs::File::create(&resolv_path)?;
        f.write_all(b"nameserver 8.8.8.8\n")?;

        let rootfs_path = rootfs_dir.join(format!("rootfs.{}", self.format));

        // Create a placeholder rootfs file.
        // In production, this would be an actual ext4 or qcow2 image.
        let mut rootfs_file = fs::File::create(&rootfs_path)?;
        rootfs_file.write_all(b"NOVA_ROOTFS_PLACEHOLDER")?;

        let size_bytes = rootfs_path.metadata()?.len();

        tracing::info!(
            image_ref,
            digest = &digest[..16],
            path = %rootfs_path.display(),
            "image pulled successfully"
        );

        Ok(ImageInfo {
            image_ref: image_ref.to_string(),
            digest: format!("sha256:{digest}"),
            rootfs_path,
            format: self.format,
            size_bytes,
            config: None,
        })
    }

    /// Pull from an OCI image layout directory on disk.
    ///
    /// 1. Parse OCI layout -> resolve manifest (linux/amd64) -> resolve config
    /// 2. Check L2 rootfs cache (by manifest digest) -> if hit, clone and return
    /// 3. Extract layers (checking L1 blob cache for each)
    /// 4. Convert rootfs dir to cpio initramfs
    /// 5. Cache result in L2 rootfs cache
    pub fn pull_oci_layout(&mut self, layout_dir: &Path) -> Result<ImageInfo> {
        use super::extract;
        use super::oci::OciImageLayout;

        tracing::info!(path = %layout_dir.display(), "pulling from OCI layout");

        // 1. Parse OCI layout.
        let layout = OciImageLayout::open(layout_dir)?;

        // Compute manifest digest for L2 cache key.
        let manifest_digest = {
            let manifest_path = layout_dir.join("manifest.json");
            if manifest_path.exists() {
                let data = fs::read(&manifest_path)?;
                let mut hasher = Sha256::new();
                hasher.update(&data);
                format!("sha256:{}", hex::encode(hasher.finalize()))
            } else {
                // Hash the index.json as fallback.
                let index_path = layout_dir.join("index.json");
                let data = fs::read(&index_path)?;
                let mut hasher = Sha256::new();
                hasher.update(&data);
                format!("sha256:{}", hex::encode(hasher.finalize()))
            }
        };

        // 2. Check L2 rootfs cache.
        if let Some(ref mut rootfs_cache) = self.rootfs_cache {
            if rootfs_cache.contains(&manifest_digest) {
                let target_path = self.image_dir.join(format!(
                    "cached_{}.cpio",
                    &manifest_digest[7..19]
                ));
                rootfs_cache.clone_rootfs(&manifest_digest, &target_path)?;
                let size_bytes = target_path.metadata()?.len();

                tracing::info!(
                    digest = &manifest_digest[..23],
                    "L2 rootfs cache hit — cloned rootfs"
                );

                return Ok(ImageInfo {
                    image_ref: layout_dir.display().to_string(),
                    digest: manifest_digest,
                    rootfs_path: target_path,
                    format: ImageFormat::Initramfs,
                    size_bytes,
                    config: Some(layout.config),
                });
            }
        }

        // 3. Extract layers (L1 blob cache checked per layer).
        let rootfs_dir = self.image_dir.join("_rootfs_tmp");
        if rootfs_dir.exists() {
            fs::remove_dir_all(&rootfs_dir)?;
        }

        // Check blob store for each layer.
        let layers = layout.layers();
        for layer in layers {
            if let Some(ref blob_store) = self.blob_store {
                if blob_store.contains(&layer.digest) {
                    tracing::debug!(digest = %layer.digest, "L1 blob cache hit");
                    // Blob is cached; extraction will use it from layout path (same data).
                }
            }
        }

        extract::extract_layers(layout_dir, layout.layers(), &rootfs_dir)?;

        // Cache extracted layer blobs in L1.
        if let Some(ref mut blob_store) = self.blob_store {
            for layer in layout.layers() {
                if !blob_store.contains(&layer.digest) {
                    let blob_path = layout_dir
                        .join("blobs")
                        .join("sha256")
                        .join(layer.digest.strip_prefix("sha256:").unwrap_or(&layer.digest));
                    if blob_path.exists() {
                        let data = fs::read(&blob_path)?;
                        // Insert with raw data (digest verified inside).
                        if let Err(e) = blob_store.insert(&layer.digest, &layer.media_type, &data) {
                            tracing::warn!(digest = %layer.digest, error = %e, "failed to cache blob");
                        }
                    }
                }
            }
        }

        // 4. Convert rootfs to cpio.
        let cpio_data = nova_boot::initrd::dir_to_cpio(&rootfs_dir)
            .map_err(|e| crate::error::RuntimeError::Image(format!("cpio creation failed: {e}")))?;

        // Clean up temp rootfs.
        let _ = fs::remove_dir_all(&rootfs_dir);

        // 5. Write cpio file and compute digest.
        let mut hasher = Sha256::new();
        hasher.update(&cpio_data);
        let content_digest = hex::encode(hasher.finalize());

        let cpio_path = self.image_dir.join(format!("{}.cpio", &content_digest[..12]));
        fs::write(&cpio_path, &cpio_data)?;

        let size_bytes = cpio_data.len() as u64;

        // Cache in L2 rootfs cache.
        if let Some(ref mut rootfs_cache) = self.rootfs_cache {
            if let Err(e) = rootfs_cache.insert(
                &manifest_digest,
                &layout_dir.display().to_string(),
                &cpio_path,
                ImageFormat::Initramfs,
                &format!("sha256:{content_digest}"),
            ) {
                tracing::warn!(error = %e, "failed to cache rootfs in L2");
            }
        }

        tracing::info!(
            manifest_digest = &manifest_digest[..23],
            content_digest = &content_digest[..16],
            size = size_bytes,
            path = %cpio_path.display(),
            "OCI image converted to initramfs"
        );

        // Use manifest_digest (not content_digest) so the image identity is
        // consistent between L2-miss and L2-hit paths.  The manifest digest
        // is a stable OCI-level identifier; the content digest varies because
        // inject_init_into_cpio later mutates the cpio file.
        Ok(ImageInfo {
            image_ref: layout_dir.display().to_string(),
            digest: manifest_digest,
            rootfs_path: cpio_path,
            format: ImageFormat::Initramfs,
            size_bytes,
            config: Some(layout.config),
        })
    }

    /// Returns the image directory path.
    pub fn image_dir(&self) -> &Path {
        &self.image_dir
    }
}
