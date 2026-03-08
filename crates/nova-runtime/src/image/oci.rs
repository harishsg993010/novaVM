//! OCI Image Layout parser.
//!
//! Parses OCI image layouts on disk: validates `oci-layout`, reads `index.json`,
//! resolves manifests and configs, and provides blob paths for layer extraction.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Result, RuntimeError};

// ---------------------------------------------------------------------------
// OCI spec structs
// ---------------------------------------------------------------------------

/// OCI image layout file (`oci-layout`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciLayout {
    pub image_layout_version: String,
}

/// Platform specification within an OCI descriptor.
#[derive(Debug, Clone, Deserialize)]
pub struct OciPlatform {
    pub architecture: String,
    pub os: String,
    #[serde(default)]
    pub variant: Option<String>,
}

/// OCI content descriptor — references a blob by digest.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciDescriptor {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    #[serde(default)]
    pub platform: Option<OciPlatform>,
}

/// OCI image index (`index.json`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciIndex {
    pub schema_version: u32,
    pub manifests: Vec<OciDescriptor>,
}

/// OCI image manifest (blob).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciManifest {
    pub config: OciDescriptor,
    pub layers: Vec<OciDescriptor>,
}

/// Container runtime config within an OCI image config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OciContainerConfig {
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<Vec<String>>,
    #[serde(default)]
    pub working_dir: Option<String>,
}

/// Rootfs description within an OCI image config.
#[derive(Debug, Clone, Deserialize)]
pub struct OciRootfs {
    #[serde(rename = "type")]
    pub rootfs_type: String,
    pub diff_ids: Vec<String>,
}

/// OCI image configuration (blob).
#[derive(Debug, Clone, Deserialize)]
pub struct OciConfig {
    pub architecture: String,
    pub os: String,
    pub config: OciContainerConfig,
    pub rootfs: OciRootfs,
}

// ---------------------------------------------------------------------------
// OciImageLayout — main API
// ---------------------------------------------------------------------------

/// Parsed OCI image layout on disk.
pub struct OciImageLayout {
    /// Root directory of the OCI layout.
    layout_dir: PathBuf,
    /// Resolved manifest.
    pub manifest: OciManifest,
    /// Resolved image config.
    pub config: OciConfig,
}

impl OciImageLayout {
    /// Open and parse an OCI image layout directory.
    ///
    /// 1. Validates `oci-layout` file
    /// 2. Reads `index.json`
    /// 3. Resolves manifest for linux/amd64
    /// 4. Resolves image config
    pub fn open(path: &Path) -> Result<Self> {
        // 1. Validate oci-layout.
        let layout_file = path.join("oci-layout");
        let layout_data = fs::read_to_string(&layout_file).map_err(|e| {
            RuntimeError::Image(format!("failed to read oci-layout: {e}"))
        })?;
        let layout: OciLayout = serde_json::from_str(&layout_data).map_err(|e| {
            RuntimeError::Image(format!("invalid oci-layout: {e}"))
        })?;
        if layout.image_layout_version != "1.0.0" {
            return Err(RuntimeError::Image(format!(
                "unsupported OCI layout version: {}",
                layout.image_layout_version
            )));
        }

        // 2. Read index.json.
        let index_file = path.join("index.json");
        let index_data = fs::read_to_string(&index_file).map_err(|e| {
            RuntimeError::Image(format!("failed to read index.json: {e}"))
        })?;
        let index: OciIndex = serde_json::from_str(&index_data).map_err(|e| {
            RuntimeError::Image(format!("invalid index.json: {e}"))
        })?;

        // 3. Resolve manifest — find linux/amd64 or take the first one.
        let manifest_desc = Self::resolve_manifest_descriptor(&index)?;
        let manifest_blob = Self::read_and_verify_blob(path, &manifest_desc)?;
        let manifest: OciManifest =
            serde_json::from_slice(&manifest_blob).map_err(|e| {
                RuntimeError::Image(format!("invalid manifest: {e}"))
            })?;

        // 4. Resolve config.
        let config_blob = Self::read_and_verify_blob(path, &manifest.config)?;
        let config: OciConfig =
            serde_json::from_slice(&config_blob).map_err(|e| {
                RuntimeError::Image(format!("invalid config: {e}"))
            })?;

        tracing::info!(
            arch = %config.architecture,
            os = %config.os,
            layers = manifest.layers.len(),
            "parsed OCI image layout"
        );

        Ok(Self {
            layout_dir: path.to_path_buf(),
            manifest,
            config,
        })
    }

    /// Resolve the blob path for a descriptor.
    pub fn blob_path(&self, descriptor: &OciDescriptor) -> PathBuf {
        Self::descriptor_to_blob_path(&self.layout_dir, descriptor)
    }

    /// Get the layer descriptors.
    pub fn layers(&self) -> &[OciDescriptor] {
        &self.manifest.layers
    }

    /// Get the layout directory.
    pub fn layout_dir(&self) -> &Path {
        &self.layout_dir
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Convert a descriptor digest to a blob path (static version for use outside Self).
    pub fn descriptor_to_blob_path_static(
        layout_dir: &Path,
        descriptor: &OciDescriptor,
    ) -> PathBuf {
        Self::descriptor_to_blob_path(layout_dir, descriptor)
    }

    /// Find the best manifest descriptor — prefer linux/amd64.
    fn resolve_manifest_descriptor(index: &OciIndex) -> Result<OciDescriptor> {
        if index.manifests.is_empty() {
            return Err(RuntimeError::Image(
                "index.json has no manifests".to_string(),
            ));
        }

        // Look for linux/amd64.
        for desc in &index.manifests {
            if let Some(ref p) = desc.platform {
                if p.os == "linux" && p.architecture == "amd64" {
                    return Ok(desc.clone());
                }
            }
        }

        // Fallback: first manifest (common for single-arch layouts).
        Ok(index.manifests[0].clone())
    }

    /// Convert a descriptor digest to a blob path on disk.
    fn descriptor_to_blob_path(layout_dir: &Path, descriptor: &OciDescriptor) -> PathBuf {
        // Digest format: "sha256:<hex>"
        let parts: Vec<&str> = descriptor.digest.splitn(2, ':').collect();
        if parts.len() == 2 {
            layout_dir
                .join("blobs")
                .join(parts[0])
                .join(parts[1])
        } else {
            // Fallback for malformed digests.
            layout_dir.join("blobs").join(&descriptor.digest)
        }
    }

    /// Read a blob and verify its SHA-256 digest.
    fn read_and_verify_blob(
        layout_dir: &Path,
        descriptor: &OciDescriptor,
    ) -> Result<Vec<u8>> {
        let blob_path = Self::descriptor_to_blob_path(layout_dir, descriptor);
        let data = fs::read(&blob_path).map_err(|e| {
            RuntimeError::Image(format!(
                "failed to read blob {}: {e}",
                blob_path.display()
            ))
        })?;

        // Verify digest.
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let computed = format!("sha256:{}", hex::encode(hasher.finalize()));

        if computed != descriptor.digest {
            return Err(RuntimeError::Image(format!(
                "digest mismatch for {}: expected {}, got {computed}",
                blob_path.display(),
                descriptor.digest
            )));
        }

        Ok(data)
    }
}
