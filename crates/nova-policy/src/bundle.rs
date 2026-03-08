//! Policy bundle loading and management.
//!
//! A policy bundle contains one or more compiled OPA Wasm modules.
//! Bundles can be loaded from disk and hot-reloaded at runtime.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::engine::{CompiledPolicy, PolicyEngine};
use crate::error::{PolicyError, Result};

/// Information about a loaded policy bundle.
#[derive(Debug, Clone)]
pub struct BundleInfo {
    /// Bundle identifier.
    pub bundle_id: String,
    /// SHA-256 digest of the bundle.
    pub digest: String,
    /// Number of policies in the bundle.
    pub policy_count: u32,
    /// When the bundle was loaded (seconds since epoch).
    pub loaded_at: u64,
}

/// Manages policy bundles and their lifecycle.
pub struct BundleManager {
    /// Loaded bundles: bundle_id -> (BundleInfo, compiled policies).
    bundles: HashMap<String, (BundleInfo, Vec<CompiledPolicy>)>,
    /// Directory for storing bundles on disk.
    bundle_dir: PathBuf,
}

impl BundleManager {
    /// Create a new bundle manager.
    pub fn new(bundle_dir: &Path) -> Result<Self> {
        fs::create_dir_all(bundle_dir)?;
        tracing::info!(dir = %bundle_dir.display(), "initialized bundle manager");
        Ok(Self {
            bundles: HashMap::new(),
            bundle_dir: bundle_dir.to_path_buf(),
        })
    }

    /// Load a policy bundle from raw Wasm bytes.
    pub fn load_bundle(
        &mut self,
        bundle_id: &str,
        wasm_bytes: &[u8],
        engine: &PolicyEngine,
    ) -> Result<()> {
        if wasm_bytes.is_empty() {
            return Err(PolicyError::Bundle(format!("empty bundle: {bundle_id}")));
        }

        // Compute digest.
        let mut hasher = Sha256::new();
        hasher.update(wasm_bytes);
        let digest = format!("sha256:{}", hex::encode(hasher.finalize()));

        // Compile the policy.
        let compiled = engine.compile(bundle_id, wasm_bytes)?;

        let info = BundleInfo {
            bundle_id: bundle_id.to_string(),
            digest,
            policy_count: 1,
            loaded_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        tracing::info!(bundle_id, "loaded policy bundle");
        self.bundles
            .insert(bundle_id.to_string(), (info, vec![compiled]));
        Ok(())
    }

    /// Remove a bundle.
    pub fn remove_bundle(&mut self, bundle_id: &str) -> Result<()> {
        if self.bundles.remove(bundle_id).is_none() {
            return Err(PolicyError::Bundle(format!(
                "bundle not found: {bundle_id}"
            )));
        }
        tracing::info!(bundle_id, "removed policy bundle");
        Ok(())
    }

    /// Get bundle info.
    pub fn get_info(&self, bundle_id: &str) -> Option<&BundleInfo> {
        self.bundles.get(bundle_id).map(|(info, _)| info)
    }

    /// Get compiled policies for a bundle.
    pub fn get_policies(&self, bundle_id: &str) -> Option<&[CompiledPolicy]> {
        self.bundles
            .get(bundle_id)
            .map(|(_, policies)| policies.as_slice())
    }

    /// List all loaded bundles.
    pub fn list_bundles(&self) -> Vec<&BundleInfo> {
        self.bundles.values().map(|(info, _)| info).collect()
    }

    /// Returns the number of loaded bundles.
    pub fn bundle_count(&self) -> usize {
        self.bundles.len()
    }

    /// Returns the bundle directory.
    pub fn bundle_dir(&self) -> &Path {
        &self.bundle_dir
    }
}
