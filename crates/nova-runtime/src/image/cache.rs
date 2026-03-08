//! Image cache for locally stored rootfs images.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, RuntimeError};
use crate::image::pull::ImageInfo;

const INDEX_FILENAME: &str = "image_cache_index.json";

/// Cache for locally stored rootfs images.
pub struct ImageCache {
    /// Base directory for the cache.
    cache_dir: PathBuf,
    /// In-memory index: image_ref -> ImageInfo.
    index: HashMap<String, ImageInfo>,
}

impl ImageCache {
    /// Create or open an image cache at the given directory.
    /// If a persisted index exists, it is loaded automatically.
    pub fn new(cache_dir: &Path) -> Result<Self> {
        fs::create_dir_all(cache_dir)?;

        let index = Self::try_load_index(cache_dir).unwrap_or_default();

        tracing::info!(
            dir = %cache_dir.display(),
            cached = index.len(),
            "initialized image cache"
        );

        Ok(Self {
            cache_dir: cache_dir.to_path_buf(),
            index,
        })
    }

    /// Add an image to the cache and persist the index.
    pub fn insert(&mut self, info: ImageInfo) {
        tracing::debug!(image_ref = %info.image_ref, "caching image");
        self.index.insert(info.image_ref.clone(), info);
        let _ = self.save_index();
    }

    /// Look up an image by reference.
    pub fn get(&self, image_ref: &str) -> Option<&ImageInfo> {
        self.index.get(image_ref)
    }

    /// Check if an image is cached.
    pub fn contains(&self, image_ref: &str) -> bool {
        self.index.contains_key(image_ref)
    }

    /// Remove an image from the cache.
    pub fn remove(&mut self, image_ref: &str) -> Result<()> {
        if let Some(info) = self.index.remove(image_ref) {
            // Remove the rootfs file if it exists.
            if info.rootfs_path.exists() {
                if let Some(parent) = info.rootfs_path.parent() {
                    fs::remove_dir_all(parent)?;
                }
            }
            tracing::info!(image_ref, "removed image from cache");
            let _ = self.save_index();
            Ok(())
        } else {
            Err(RuntimeError::Image(format!(
                "image not in cache: {image_ref}"
            )))
        }
    }

    /// List all cached images.
    pub fn list(&self) -> Vec<&ImageInfo> {
        self.index.values().collect()
    }

    /// Returns the cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Persist the index to disk.
    pub fn save_index(&self) -> Result<()> {
        let index_path = self.cache_dir.join(INDEX_FILENAME);
        let tmp_path = self.cache_dir.join(format!("{INDEX_FILENAME}.tmp"));
        let data = serde_json::to_string_pretty(&self.index).map_err(|e| {
            RuntimeError::Cache(format!("failed to serialize image cache index: {e}"))
        })?;
        fs::write(&tmp_path, data)?;
        fs::rename(&tmp_path, &index_path)?;
        Ok(())
    }

    /// Load the index from disk.
    pub fn load_index(cache_dir: &Path) -> Result<HashMap<String, ImageInfo>> {
        let index_path = cache_dir.join(INDEX_FILENAME);
        if !index_path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(&index_path)?;
        serde_json::from_str(&data).map_err(|e| {
            RuntimeError::Cache(format!("failed to parse image cache index: {e}"))
        })
    }

    fn try_load_index(cache_dir: &Path) -> Option<HashMap<String, ImageInfo>> {
        Self::load_index(cache_dir).ok()
    }
}
