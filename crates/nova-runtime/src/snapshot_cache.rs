//! Snapshot cache index for L3 snapshot save/restore.
//!
//! Maps (image_digest + config_hash) -> snapshot directory on disk.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Result, RuntimeError};

const INDEX_FILENAME: &str = "snapshot_cache_index.json";

/// A single cached snapshot entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Cache key (image_digest + config_hash).
    pub key: String,
    /// Directory containing the snapshot files.
    pub snapshot_dir: PathBuf,
    /// Hash of the VM config used to create this snapshot.
    pub config_hash: String,
    /// OCI image digest this snapshot was created from.
    pub image_digest: String,
    /// When the snapshot was created.
    pub created_at: SystemTime,
    /// Whether this snapshot is still valid.
    pub valid: bool,
}

/// Index for cached VM snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SnapshotCacheIndex {
    entries: HashMap<String, SnapshotEntry>,
}

/// Manages cached VM snapshots on disk.
pub struct SnapshotCache {
    base_dir: PathBuf,
    index: SnapshotCacheIndex,
}

impl SnapshotCache {
    /// Open or create a snapshot cache.
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;

        let index_path = dir.join(INDEX_FILENAME);
        let index = if index_path.exists() {
            let data = fs::read_to_string(&index_path)?;
            serde_json::from_str(&data).map_err(|e| {
                RuntimeError::Snapshot(format!("failed to parse snapshot cache index: {e}"))
            })?
        } else {
            SnapshotCacheIndex::default()
        };

        Ok(Self {
            base_dir: dir.to_path_buf(),
            index,
        })
    }

    /// Get a valid snapshot entry by key.
    pub fn get(&self, key: &str) -> Option<&SnapshotEntry> {
        self.index
            .entries
            .get(key)
            .filter(|e| e.valid && e.snapshot_dir.exists())
    }

    /// Insert a snapshot entry.
    pub fn insert(&mut self, entry: SnapshotEntry) -> Result<()> {
        self.index.entries.insert(entry.key.clone(), entry);
        self.flush()
    }

    /// Invalidate a snapshot by key.
    pub fn invalidate(&mut self, key: &str) -> Result<()> {
        if let Some(entry) = self.index.entries.get_mut(key) {
            entry.valid = false;
            self.flush()?;
        }
        Ok(())
    }

    /// Check if a valid snapshot exists for the given key.
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Number of entries (including invalid ones).
    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }

    /// Persist the index to disk.
    pub fn flush(&self) -> Result<()> {
        let index_path = self.base_dir.join(INDEX_FILENAME);
        let tmp_path = self.base_dir.join(format!("{INDEX_FILENAME}.tmp"));
        let data = serde_json::to_string_pretty(&self.index).map_err(|e| {
            RuntimeError::Snapshot(format!("failed to serialize snapshot cache index: {e}"))
        })?;
        fs::write(&tmp_path, data)?;
        fs::rename(&tmp_path, &index_path)?;
        Ok(())
    }

    /// Build a cache key from image digest and config hash.
    pub fn make_key(image_digest: &str, config_hash: &str) -> String {
        format!("{image_digest}:{config_hash}")
    }
}
