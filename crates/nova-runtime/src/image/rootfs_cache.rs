//! Pre-built rootfs cache with CoW clone support (L2).
//!
//! Caches the finished rootfs (cpio) keyed by OCI manifest digest.
//! Second run clones the cached rootfs (reflink -> hardlink -> copy fallback)
//! and boots directly.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Result, RuntimeError};
use crate::image::pull::ImageFormat;

/// Strategy for cloning cached rootfs files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloneStrategy {
    /// Copy-on-write reflink (btrfs/xfs/APFS).
    Reflink,
    /// Hard link (same filesystem).
    Hardlink,
    /// Full copy (always works).
    Copy,
}

impl std::fmt::Display for CloneStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reflink => write!(f, "reflink"),
            Self::Hardlink => write!(f, "hardlink"),
            Self::Copy => write!(f, "copy"),
        }
    }
}

/// Metadata for a cached rootfs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootfsCacheEntry {
    /// OCI manifest digest used as the cache key.
    pub manifest_digest: String,
    /// Original OCI image reference.
    pub image_ref: String,
    /// Filename of the cached rootfs in the cache dir.
    pub rootfs_filename: String,
    /// Format of the rootfs.
    pub format: ImageFormat,
    /// Size in bytes.
    pub size_bytes: u64,
    /// SHA-256 content digest of the rootfs file.
    pub content_digest: String,
    /// When this entry was created.
    pub created_at: SystemTime,
    /// Number of times this entry has been used.
    pub use_count: u64,
}

/// Persisted index for the rootfs cache.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RootfsCacheIndex {
    entries: HashMap<String, RootfsCacheEntry>,
}

const INDEX_FILENAME: &str = "rootfs_index.json";

/// Cache of pre-built rootfs images with CoW clone support.
pub struct RootfsCache {
    cache_dir: PathBuf,
    index: RootfsCacheIndex,
    clone_strategy: CloneStrategy,
}

impl RootfsCache {
    /// Open or create a rootfs cache at the given directory.
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;

        let index_path = dir.join(INDEX_FILENAME);
        let index = if index_path.exists() {
            let data = fs::read_to_string(&index_path)?;
            serde_json::from_str(&data).map_err(|e| {
                RuntimeError::Cache(format!("failed to parse rootfs cache index: {e}"))
            })?
        } else {
            RootfsCacheIndex::default()
        };

        let clone_strategy = Self::detect_clone_strategy_for_dir(dir);

        tracing::debug!(
            dir = %dir.display(),
            entries = index.entries.len(),
            strategy = %clone_strategy,
            "opened rootfs cache"
        );

        Ok(Self {
            cache_dir: dir.to_path_buf(),
            index,
            clone_strategy,
        })
    }

    /// Check if a rootfs is cached for the given manifest digest.
    pub fn contains(&self, manifest_digest: &str) -> bool {
        if let Some(entry) = self.index.entries.get(manifest_digest) {
            self.cache_dir.join(&entry.rootfs_filename).exists()
        } else {
            false
        }
    }

    /// Get the cache entry for a manifest digest.
    pub fn get(&self, manifest_digest: &str) -> Option<&RootfsCacheEntry> {
        self.index.entries.get(manifest_digest)
    }

    /// Insert a rootfs into the cache. Copies the file into the cache directory.
    pub fn insert(
        &mut self,
        manifest_digest: &str,
        image_ref: &str,
        rootfs_path: &Path,
        format: ImageFormat,
        content_digest: &str,
    ) -> Result<()> {
        // Use SHA-256 of the manifest_digest as filename to avoid collisions.
        let digest_hex = manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(manifest_digest);
        let filename = format!("rootfs_{}.cpio", digest_hex);
        let cached_path = self.cache_dir.join(&filename);

        // Copy rootfs into cache dir.
        fs::copy(rootfs_path, &cached_path)?;
        let size_bytes = cached_path.metadata()?.len();

        self.index.entries.insert(
            manifest_digest.to_string(),
            RootfsCacheEntry {
                manifest_digest: manifest_digest.to_string(),
                image_ref: image_ref.to_string(),
                rootfs_filename: filename,
                format,
                size_bytes,
                content_digest: content_digest.to_string(),
                created_at: SystemTime::now(),
                use_count: 0,
            },
        );

        self.flush()?;
        Ok(())
    }

    /// Clone a cached rootfs to the target path using the best available strategy.
    pub fn clone_rootfs(
        &mut self,
        manifest_digest: &str,
        target_path: &Path,
    ) -> Result<()> {
        let entry = self.index.entries.get_mut(manifest_digest).ok_or_else(|| {
            RuntimeError::Cache(format!("rootfs not cached: {manifest_digest}"))
        })?;

        entry.use_count += 1;

        let src_path = self.cache_dir.join(&entry.rootfs_filename);
        if !src_path.exists() {
            return Err(RuntimeError::Cache(format!(
                "cached rootfs file missing: {}",
                src_path.display()
            )));
        }

        Self::clone_file(&src_path, target_path, self.clone_strategy)?;

        self.flush()?;
        Ok(())
    }

    /// Detect the best clone strategy for the given directory.
    pub fn detect_clone_strategy(test_dir: &Path) -> CloneStrategy {
        Self::detect_clone_strategy_for_dir(test_dir)
    }

    /// Get the current clone strategy.
    pub fn clone_strategy(&self) -> CloneStrategy {
        self.clone_strategy
    }

    /// Number of cached rootfs entries.
    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }

    /// Persist the index to disk.
    pub fn flush(&self) -> Result<()> {
        let index_path = self.cache_dir.join(INDEX_FILENAME);
        let tmp_path = self.cache_dir.join(format!("{INDEX_FILENAME}.tmp"));
        let data = serde_json::to_string_pretty(&self.index).map_err(|e| {
            RuntimeError::Cache(format!("failed to serialize rootfs cache index: {e}"))
        })?;
        fs::write(&tmp_path, data)?;
        fs::rename(&tmp_path, &index_path)?;
        Ok(())
    }

    /// Evict entries until total size is below `max_total_bytes`, using LRU
    /// ordering by `use_count` (lowest use_count evicted first).
    pub fn evict(&mut self, max_total_bytes: u64) -> Result<usize> {
        let total: u64 = self.index.entries.values().map(|e| e.size_bytes).sum();
        if total <= max_total_bytes {
            return Ok(0);
        }

        // Sort by use_count ascending (LRU).
        let mut entries: Vec<_> = self.index.entries.values().cloned().collect();
        entries.sort_by_key(|e| e.use_count);

        let mut evicted = 0;
        let mut current_total = total;

        for entry in entries {
            if current_total <= max_total_bytes {
                break;
            }
            let path = self.cache_dir.join(&entry.rootfs_filename);
            let _ = fs::remove_file(&path);
            self.index.entries.remove(&entry.manifest_digest);
            current_total = current_total.saturating_sub(entry.size_bytes);
            evicted += 1;
        }

        if evicted > 0 {
            self.flush()?;
            tracing::info!(evicted, "rootfs cache eviction complete");
        }

        Ok(evicted)
    }

    /// Total size of all cached rootfs files.
    pub fn total_size(&self) -> u64 {
        self.index.entries.values().map(|e| e.size_bytes).sum()
    }

    // -- internal helpers --

    fn detect_clone_strategy_for_dir(dir: &Path) -> CloneStrategy {
        // Try reflink first.
        let test_src = dir.join(".clone_test_src");
        let test_dst = dir.join(".clone_test_dst");

        if fs::write(&test_src, b"test").is_ok() {
            // Try hardlink.
            if fs::hard_link(&test_src, &test_dst).is_ok() {
                let _ = fs::remove_file(&test_dst);
                let _ = fs::remove_file(&test_src);
                return CloneStrategy::Hardlink;
            }
            let _ = fs::remove_file(&test_src);
        }

        CloneStrategy::Copy
    }

    fn clone_file(src: &Path, dst: &Path, _strategy: CloneStrategy) -> Result<()> {
        // Always use full copy: the cloned rootfs file will be mutated by
        // inject_init_into_cpio (which appends init/entry overlays). Hardlinks
        // share the inode so the append would corrupt the cached source. Worse,
        // if dst already exists as a hardlink to src, fs::copy(src, dst)
        // truncates both (same inode) producing a 0-byte file.
        if dst.exists() {
            fs::remove_file(dst)?;
        }
        fs::copy(src, dst)?;
        Ok(())
    }
}
