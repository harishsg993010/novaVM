//! Content-addressable blob storage for OCI layer caching (L1).
//!
//! Stores individual OCI layer blobs by their sha256 digest on disk.
//! Second pull of the same image skips re-extraction from the OCI layout.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Result, RuntimeError};

/// Metadata for a single cached blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobEntry {
    /// Content digest (e.g. "sha256:abcdef...").
    pub digest: String,
    /// OCI media type of the blob.
    pub media_type: String,
    /// Size of the blob in bytes.
    pub size: u64,
    /// Number of references to this blob.
    pub ref_count: u32,
    /// Last time this blob was accessed.
    pub last_accessed: SystemTime,
}

/// Persisted index mapping digest -> BlobEntry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BlobStoreIndex {
    blobs: HashMap<String, BlobEntry>,
}

/// Content-addressable blob store backed by the filesystem.
pub struct BlobStore {
    base_dir: PathBuf,
    index: BlobStoreIndex,
}

impl BlobStore {
    /// Open or create a blob store at the given directory.
    pub fn open(dir: &Path) -> Result<Self> {
        let blobs_dir = dir.join("sha256");
        fs::create_dir_all(&blobs_dir)?;

        let index_path = dir.join("index.json");
        let index = if index_path.exists() {
            let data = fs::read_to_string(&index_path)?;
            serde_json::from_str(&data).map_err(|e| {
                RuntimeError::Cache(format!("failed to parse blob index: {e}"))
            })?
        } else {
            BlobStoreIndex::default()
        };

        tracing::debug!(dir = %dir.display(), blobs = index.blobs.len(), "opened blob store");

        Ok(Self {
            base_dir: dir.to_path_buf(),
            index,
        })
    }

    /// Check if a blob with the given digest exists in the store.
    pub fn contains(&self, digest: &str) -> bool {
        let key = Self::normalize_digest(digest);
        if let Some(entry) = self.index.blobs.get(&key) {
            // Also check the file actually exists on disk.
            self.blob_path_for_key(&key)
                .map(|p| p.exists())
                .unwrap_or(false)
                && entry.size > 0
        } else {
            false
        }
    }

    /// Get the filesystem path for a cached blob.
    pub fn blob_path(&self, digest: &str) -> Option<PathBuf> {
        let key = Self::normalize_digest(digest);
        if self.index.blobs.contains_key(&key) {
            self.blob_path_for_key(&key)
        } else {
            None
        }
    }

    /// Insert a blob from raw bytes, verifying the digest.
    pub fn insert(&mut self, digest: &str, media_type: &str, data: &[u8]) -> Result<()> {
        let key = Self::normalize_digest(digest);

        // Verify digest.
        let computed = Self::compute_sha256(data);
        let expected_hex = Self::hex_from_digest(&key);
        if computed != expected_hex {
            return Err(RuntimeError::Cache(format!(
                "digest mismatch: expected {expected_hex}, got {computed}"
            )));
        }

        let blob_path = self.blob_path_for_key(&key).ok_or_else(|| {
            RuntimeError::Cache("invalid digest format".to_string())
        })?;

        // Write to tmp file then rename for atomicity.
        let tmp_path = blob_path.with_extension("tmp");
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(data)?;
        f.flush()?;
        fs::rename(&tmp_path, &blob_path)?;

        self.index.blobs.insert(
            key.clone(),
            BlobEntry {
                digest: key,
                media_type: media_type.to_string(),
                size: data.len() as u64,
                ref_count: 1,
                last_accessed: SystemTime::now(),
            },
        );

        self.flush()?;
        Ok(())
    }

    /// Insert a blob by moving a file into the store, verifying digest.
    pub fn insert_file(
        &mut self,
        digest: &str,
        media_type: &str,
        src_path: &Path,
    ) -> Result<()> {
        let key = Self::normalize_digest(digest);
        let blob_path = self.blob_path_for_key(&key).ok_or_else(|| {
            RuntimeError::Cache("invalid digest format".to_string())
        })?;

        let size = src_path.metadata()?.len();

        // Try rename first (same filesystem), fall back to copy+delete.
        if fs::rename(src_path, &blob_path).is_err() {
            fs::copy(src_path, &blob_path)?;
            let _ = fs::remove_file(src_path);
        }

        self.index.blobs.insert(
            key.clone(),
            BlobEntry {
                digest: key,
                media_type: media_type.to_string(),
                size,
                ref_count: 1,
                last_accessed: SystemTime::now(),
            },
        );

        self.flush()?;
        Ok(())
    }

    /// Increment reference count for a blob.
    pub fn add_ref(&mut self, digest: &str) -> Result<()> {
        let key = Self::normalize_digest(digest);
        if let Some(entry) = self.index.blobs.get_mut(&key) {
            entry.ref_count += 1;
            entry.last_accessed = SystemTime::now();
            self.flush()?;
            Ok(())
        } else {
            Err(RuntimeError::Cache(format!("blob not found: {key}")))
        }
    }

    /// Decrement reference count. If `remove_if_zero` is true and the count
    /// reaches zero, delete the blob from disk.
    pub fn release_ref(&mut self, digest: &str, remove_if_zero: bool) -> Result<()> {
        let key = Self::normalize_digest(digest);
        let should_remove = if let Some(entry) = self.index.blobs.get_mut(&key) {
            entry.ref_count = entry.ref_count.saturating_sub(1);
            remove_if_zero && entry.ref_count == 0
        } else {
            return Err(RuntimeError::Cache(format!("blob not found: {key}")));
        };

        if should_remove {
            if let Some(path) = self.blob_path_for_key(&key) {
                let _ = fs::remove_file(&path);
            }
            self.index.blobs.remove(&key);
        }

        self.flush()?;
        Ok(())
    }

    /// Persist the index to disk atomically.
    pub fn flush(&self) -> Result<()> {
        let index_path = self.base_dir.join("index.json");
        let tmp_path = self.base_dir.join("index.json.tmp");
        let data = serde_json::to_string_pretty(&self.index).map_err(|e| {
            RuntimeError::Cache(format!("failed to serialize blob index: {e}"))
        })?;
        fs::write(&tmp_path, data)?;
        fs::rename(&tmp_path, &index_path)?;
        Ok(())
    }

    /// Garbage-collect blobs with ref_count == 0 that are older than `max_age`.
    pub fn gc(&mut self, max_age: std::time::Duration) -> Result<usize> {
        let now = SystemTime::now();
        let mut to_remove = Vec::new();

        for (key, entry) in &self.index.blobs {
            if entry.ref_count == 0 {
                if let Ok(age) = now.duration_since(entry.last_accessed) {
                    if age > max_age {
                        to_remove.push(key.clone());
                    }
                }
            }
        }

        let count = to_remove.len();
        for key in &to_remove {
            if let Some(path) = self.blob_path_for_key(key) {
                let _ = fs::remove_file(&path);
            }
            self.index.blobs.remove(key);
        }

        if count > 0 {
            self.flush()?;
            tracing::info!(removed = count, "blob store GC complete");
        }

        Ok(count)
    }

    /// Total size of all cached blobs in bytes.
    pub fn total_size(&self) -> u64 {
        self.index.blobs.values().map(|e| e.size).sum()
    }

    /// Number of blobs in the store.
    pub fn len(&self) -> usize {
        self.index.blobs.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.index.blobs.is_empty()
    }

    // -- helpers --

    fn normalize_digest(digest: &str) -> String {
        if digest.starts_with("sha256:") {
            digest.to_string()
        } else {
            format!("sha256:{digest}")
        }
    }

    fn hex_from_digest(digest: &str) -> String {
        digest
            .strip_prefix("sha256:")
            .unwrap_or(digest)
            .to_string()
    }

    fn blob_path_for_key(&self, key: &str) -> Option<PathBuf> {
        let hex = Self::hex_from_digest(key);
        if hex.is_empty() {
            None
        } else {
            Some(self.base_dir.join("sha256").join(&hex))
        }
    }

    fn compute_sha256(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }
}
