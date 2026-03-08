//! OCI Distribution Spec v2 registry client.
//!
//! Implements the pull flow: ping → token → manifest → config → layers.
//! Produces a standard OCI image layout directory (`oci-layout`, `index.json`,
//! `blobs/sha256/`).

use std::path::Path;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("manifest not found for {0}")]
    ManifestNotFound(String),

    #[error("no linux/amd64 platform in manifest list")]
    NoPlatformMatch,

    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid image reference: {0}")]
    InvalidReference(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RegistryError>;

// ---------------------------------------------------------------------------
// Image reference
// ---------------------------------------------------------------------------

/// Parsed OCI image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    /// Registry hostname (e.g. "registry-1.docker.io").
    pub registry: String,
    /// Repository path (e.g. "library/nginx").
    pub repository: String,
    /// Tag or digest (e.g. "alpine" or "sha256:abc...").
    pub reference: String,
}

impl ImageReference {
    /// Parse a Docker/OCI image reference string.
    ///
    /// Parsing rules:
    /// - `nginx` → `registry-1.docker.io/library/nginx:latest`
    /// - `nginx:alpine` → `registry-1.docker.io/library/nginx:alpine`
    /// - `ghcr.io/owner/repo:v1` → `ghcr.io/owner/repo:v1`
    /// - `quay.io/org/img@sha256:abc` → `quay.io/org/img@sha256:abc`
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(RegistryError::InvalidReference(
                "empty image reference".into(),
            ));
        }

        // Split off @sha256: digest reference.
        let (name_part, reference) = if let Some(idx) = input.find('@') {
            let (name, digest) = input.split_at(idx);
            (name, &digest[1..]) // skip '@'
        } else if let Some(idx) = input.rfind(':') {
            // Check if the colon is part of a registry (e.g. "localhost:5000/repo")
            // by seeing if it's before the first '/'.
            let first_slash = input.find('/');
            if first_slash.is_none() || idx > first_slash.unwrap() {
                // Colon is after the first slash — it's a tag separator.
                let (name, tag) = input.split_at(idx);
                (name, &tag[1..]) // skip ':'
            } else {
                // Colon is in the registry part (e.g. "localhost:5000/repo").
                (input, "latest")
            }
        } else {
            (input, "latest")
        };

        // Determine if the first component is a registry or a repository.
        let (registry, repository) = if let Some(slash_idx) = name_part.find('/') {
            let first = &name_part[..slash_idx];
            // First component is a registry if it contains '.' or ':'
            if first.contains('.') || first.contains(':') {
                (first.to_string(), name_part[slash_idx + 1..].to_string())
            } else {
                // Docker Hub with explicit org (e.g. "myuser/myrepo").
                (
                    "registry-1.docker.io".to_string(),
                    name_part.to_string(),
                )
            }
        } else {
            // Single-component name — Docker Hub official image.
            (
                "registry-1.docker.io".to_string(),
                format!("library/{name_part}"),
            )
        };

        Ok(Self {
            registry,
            repository,
            reference: reference.to_string(),
        })
    }

    /// Reconstruct a display string.
    pub fn display_ref(&self) -> String {
        if self.reference.starts_with("sha256:") {
            format!("{}/{}@{}", self.registry, self.repository, self.reference)
        } else {
            format!("{}/{}:{}", self.registry, self.repository, self.reference)
        }
    }
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_ref())
    }
}

// ---------------------------------------------------------------------------
// Registry client
// ---------------------------------------------------------------------------

/// Client for pulling OCI images from container registries.
pub struct RegistryClient {
    http: reqwest::Client,
}

/// Progress callback: (bytes_downloaded, total_bytes_or_0, layer_index, total_layers).
pub type ProgressFn = Box<dyn Fn(u64, u64, usize, usize) + Send + Sync>;

impl RegistryClient {
    /// Create a new registry client.
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("novavm/0.1")
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .expect("failed to build HTTP client");
        Self { http }
    }

    /// Pull an OCI image and write it as an OCI image layout to `output_dir`.
    ///
    /// Returns the manifest digest (sha256:...).
    pub async fn pull(
        &self,
        image: &ImageReference,
        output_dir: &Path,
        progress: Option<ProgressFn>,
    ) -> Result<String> {
        std::fs::create_dir_all(output_dir)?;

        // 1. Get auth token.
        let token = self.authenticate(image).await?;

        // 2. Fetch manifest (resolve manifest list if needed).
        let (manifest, _manifest_digest) =
            self.fetch_manifest(image, &token).await?;

        // 3. Write OCI layout metadata.
        let blobs_dir = output_dir.join("blobs/sha256");
        std::fs::create_dir_all(&blobs_dir)?;

        // Write oci-layout file.
        std::fs::write(
            output_dir.join("oci-layout"),
            r#"{"imageLayoutVersion":"1.0.0"}"#,
        )?;

        // Write manifest blob.
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        let manifest_hash = sha256_hex(&manifest_bytes);
        std::fs::write(blobs_dir.join(&manifest_hash), &manifest_bytes)?;

        // 4. Download config blob.
        let config_digest = manifest.config.digest.clone();
        let config_hash = digest_to_hash(&config_digest);
        let config_path = blobs_dir.join(&config_hash);
        if !config_path.exists() {
            let config_bytes = self
                .fetch_blob(image, &config_digest, &token)
                .await?;
            verify_and_write(&config_path, &config_bytes, &config_hash)?;
        }

        // 5. Download layer blobs (streaming with SHA-256 verification).
        let total_layers = manifest.layers.len();
        for (i, layer) in manifest.layers.iter().enumerate() {
            let layer_hash = digest_to_hash(&layer.digest);
            let layer_path = blobs_dir.join(&layer_hash);
            if layer_path.exists() {
                tracing::debug!(layer = i, digest = %layer.digest, "layer already exists, skipping");
                continue;
            }
            tracing::info!(
                layer = i + 1,
                total = total_layers,
                digest = %layer.digest,
                size = layer.size,
                "downloading layer"
            );
            self.stream_blob(image, &layer.digest, &layer_path, &token, &progress, i, total_layers)
                .await?;
        }

        // 6. Write index.json pointing to the manifest.
        let index = OciIndex {
            schema_version: 2,
            manifests: vec![OciDescriptor {
                media_type: "application/vnd.oci.image.manifest.v1+json".into(),
                digest: format!("sha256:{manifest_hash}"),
                size: manifest_bytes.len() as u64,
            }],
        };
        let index_bytes = serde_json::to_vec_pretty(&index)?;
        std::fs::write(output_dir.join("index.json"), &index_bytes)?;

        // 7. Write manifest.json (Docker-style, for OciImageLayout::open compatibility).
        let manifest_json = serde_json::json!([{
            "Config": format!("blobs/sha256/{config_hash}"),
            "RepoTags": [image.display_ref()],
            "Layers": manifest.layers.iter().map(|l| {
                format!("blobs/sha256/{}", digest_to_hash(&l.digest))
            }).collect::<Vec<_>>()
        }]);
        std::fs::write(
            output_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest_json)?,
        )?;

        let digest = format!("sha256:{manifest_hash}");
        tracing::info!(digest = %digest, layers = total_layers, "image pull complete");
        Ok(digest)
    }

    // -----------------------------------------------------------------------
    // Auth
    // -----------------------------------------------------------------------

    /// Authenticate with the registry (Bearer token flow).
    async fn authenticate(&self, image: &ImageReference) -> Result<String> {
        // Step 1: Ping /v2/ to get WWW-Authenticate header.
        let ping_url = format!("https://{}/v2/", image.registry);
        let resp = self.http.get(&ping_url).send().await?;

        if resp.status() == reqwest::StatusCode::OK {
            // No auth required (rare, but possible for some registries).
            return Ok(String::new());
        }

        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Err(RegistryError::Auth(format!(
                "unexpected status from /v2/: {}",
                resp.status()
            )));
        }

        // Parse WWW-Authenticate: Bearer realm="...",service="..."
        let www_auth = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                RegistryError::Auth("missing WWW-Authenticate header".into())
            })?
            .to_string();

        let realm = extract_auth_param(&www_auth, "realm")
            .ok_or_else(|| RegistryError::Auth("missing realm in WWW-Authenticate".into()))?;
        let service = extract_auth_param(&www_auth, "service")
            .unwrap_or_default();

        // Step 2: Fetch token.
        let mut token_url = format!("{realm}?scope=repository:{}:pull", image.repository);
        if !service.is_empty() {
            token_url = format!("{token_url}&service={service}");
        }

        let token_resp = self.http.get(&token_url).send().await?;
        if !token_resp.status().is_success() {
            return Err(RegistryError::Auth(format!(
                "token request failed: {}",
                token_resp.status()
            )));
        }

        let token_json: serde_json::Value = token_resp.json().await?;
        let token = token_json
            .get("token")
            .or_else(|| token_json.get("access_token"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| RegistryError::Auth("no token in response".into()))?
            .to_string();

        Ok(token)
    }

    // -----------------------------------------------------------------------
    // Manifest
    // -----------------------------------------------------------------------

    /// Fetch the image manifest, resolving manifest lists to linux/amd64.
    async fn fetch_manifest(
        &self,
        image: &ImageReference,
        token: &str,
    ) -> Result<(OciManifest, String)> {
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            image.registry, image.repository, image.reference
        );

        let accept = [
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
            "application/vnd.oci.image.index.v1+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
        ]
        .join(", ");

        let mut req = self.http.get(&url).header("Accept", &accept);
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::ManifestNotFound(image.display_ref()));
        }
        if !resp.status().is_success() {
            return Err(RegistryError::ManifestNotFound(format!(
                "{}: status {}",
                image.display_ref(),
                resp.status()
            )));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.bytes().await?;

        // Check if this is a manifest list / OCI index.
        if content_type.contains("manifest.list") || content_type.contains("image.index") {
            let list: ManifestList = serde_json::from_slice(&body)?;
            let target = list
                .manifests
                .iter()
                .find(|m| {
                    m.platform.as_ref().map_or(false, |p| {
                        p.architecture == "amd64" && p.os == "linux"
                    })
                })
                .ok_or(RegistryError::NoPlatformMatch)?;

            // Re-fetch the platform-specific manifest by digest.
            let digest = target.digest.clone();
            let url2 = format!(
                "https://{}/v2/{}/manifests/{}",
                image.registry, image.repository, digest
            );
            let accept2 = [
                "application/vnd.oci.image.manifest.v1+json",
                "application/vnd.docker.distribution.manifest.v2+json",
            ]
            .join(", ");

            let mut req2 = self.http.get(&url2).header("Accept", &accept2);
            if !token.is_empty() {
                req2 = req2.bearer_auth(token);
            }
            let resp2 = req2.send().await?;
            let body2 = resp2.bytes().await?;
            let manifest: OciManifest = serde_json::from_slice(&body2)?;
            let hash = sha256_hex(&body2);
            Ok((manifest, format!("sha256:{hash}")))
        } else {
            let manifest: OciManifest = serde_json::from_slice(&body)?;
            let hash = sha256_hex(&body);
            Ok((manifest, format!("sha256:{hash}")))
        }
    }

    // -----------------------------------------------------------------------
    // Blob downloads
    // -----------------------------------------------------------------------

    /// Fetch a small blob (config) entirely into memory.
    async fn fetch_blob(
        &self,
        image: &ImageReference,
        digest: &str,
        token: &str,
    ) -> Result<Vec<u8>> {
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            image.registry, image.repository, digest
        );
        let mut req = self.http.get(&url);
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?.error_for_status()?;
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// Stream a layer blob to disk with SHA-256 verification.
    async fn stream_blob(
        &self,
        image: &ImageReference,
        digest: &str,
        output_path: &Path,
        token: &str,
        progress: &Option<ProgressFn>,
        layer_idx: usize,
        total_layers: usize,
    ) -> Result<()> {
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            image.registry, image.repository, digest
        );
        let mut req = self.http.get(&url);
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?.error_for_status()?;

        let total_size = resp.content_length().unwrap_or(0);
        let tmp_path = output_path.with_extension("tmp");

        let mut file = std::fs::File::create(&tmp_path)?;
        let mut hasher = Sha256::new();
        let mut downloaded: u64 = 0;

        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            std::io::Write::write_all(&mut file, &chunk)?;
            hasher.update(&chunk);
            downloaded += chunk.len() as u64;

            if let Some(ref cb) = progress {
                cb(downloaded, total_size, layer_idx, total_layers);
            }
        }

        // Verify digest.
        let actual_hash = hex::encode(hasher.finalize());
        let expected_hash = digest_to_hash(digest);
        if actual_hash != expected_hash {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(RegistryError::DigestMismatch {
                expected: expected_hash,
                actual: actual_hash,
            });
        }

        // Atomic rename.
        std::fs::rename(&tmp_path, output_path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OCI manifest types (minimal, for deserialization)
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OciManifest {
    #[serde(default)]
    schema_version: u32,
    config: OciManifestDescriptor,
    layers: Vec<OciManifestDescriptor>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OciManifestDescriptor {
    #[serde(default, rename = "mediaType")]
    media_type: String,
    digest: String,
    size: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestList {
    manifests: Vec<ManifestListEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestListEntry {
    digest: String,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Debug, serde::Deserialize)]
struct Platform {
    architecture: String,
    os: String,
}

/// OCI index (for index.json in the layout).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OciIndex {
    schema_version: u32,
    manifests: Vec<OciDescriptor>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OciDescriptor {
    media_type: String,
    digest: String,
    size: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a parameter from a `WWW-Authenticate: Bearer ...` header.
fn extract_auth_param(header: &str, param: &str) -> Option<String> {
    let pattern = format!("{param}=\"");
    let start = header.find(&pattern)? + pattern.len();
    let rest = &header[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Compute SHA-256 hex digest of bytes.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Extract the hex hash from a "sha256:abc..." digest string.
fn digest_to_hash(digest: &str) -> String {
    digest
        .strip_prefix("sha256:")
        .unwrap_or(digest)
        .to_string()
}

/// Write data to a file and verify its SHA-256 hash.
fn verify_and_write(path: &Path, data: &[u8], expected_hash: &str) -> Result<()> {
    let actual = sha256_hex(data);
    if actual != expected_hash {
        return Err(RegistryError::DigestMismatch {
            expected: expected_hash.to_string(),
            actual,
        });
    }
    std::fs::write(path, data)?;
    Ok(())
}

/// Sanitize an image reference into a safe directory name.
pub fn sanitize_image_ref(image_ref: &str) -> String {
    image_ref
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_docker_hub_short() {
        let r = ImageReference::parse("nginx").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn test_parse_docker_hub_with_tag() {
        let r = ImageReference::parse("nginx:alpine").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "alpine");
    }

    #[test]
    fn test_parse_docker_hub_with_org() {
        let r = ImageReference::parse("myuser/myrepo:v1").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "myuser/myrepo");
        assert_eq!(r.reference, "v1");
    }

    #[test]
    fn test_parse_ghcr_ref() {
        let r = ImageReference::parse("ghcr.io/owner/repo:v1").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "owner/repo");
        assert_eq!(r.reference, "v1");
    }

    #[test]
    fn test_parse_quay_digest() {
        let r = ImageReference::parse("quay.io/org/img@sha256:abc123").unwrap();
        assert_eq!(r.registry, "quay.io");
        assert_eq!(r.repository, "org/img");
        assert_eq!(r.reference, "sha256:abc123");
    }

    #[test]
    fn test_parse_default_tag() {
        let r = ImageReference::parse("alpine").unwrap();
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn test_parse_empty_fails() {
        assert!(ImageReference::parse("").is_err());
    }

    #[test]
    fn test_display_ref() {
        let r = ImageReference::parse("nginx:alpine").unwrap();
        assert_eq!(r.display_ref(), "registry-1.docker.io/library/nginx:alpine");
    }

    #[test]
    fn test_sanitize_image_ref() {
        assert_eq!(sanitize_image_ref("nginx:alpine"), "nginx-alpine");
        assert_eq!(
            sanitize_image_ref("docker.io/library/nginx:latest"),
            "docker-io-library-nginx-latest"
        );
    }

    #[test]
    fn test_extract_auth_param() {
        let header = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io""#;
        assert_eq!(
            extract_auth_param(header, "realm"),
            Some("https://auth.docker.io/token".into())
        );
        assert_eq!(
            extract_auth_param(header, "service"),
            Some("registry.docker.io".into())
        );
        assert_eq!(extract_auth_param(header, "scope"), None);
    }

    #[test]
    fn test_digest_to_hash() {
        assert_eq!(digest_to_hash("sha256:abcdef"), "abcdef");
        assert_eq!(digest_to_hash("abcdef"), "abcdef");
    }
}
