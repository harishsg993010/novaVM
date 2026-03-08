//! SandboxImageService gRPC implementation.
//!
//! Handles OCI image pull, list, remove, and inspect via gRPC.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::registry::{ImageReference, RegistryClient, sanitize_image_ref};
use crate::sandbox::sandbox_image_service_server::SandboxImageService;
use crate::sandbox::*;

use nova_runtime::{ImageCache, ImagePuller};

/// gRPC service for OCI image management.
pub struct ImageDaemonService {
    pub registry_client: Arc<RegistryClient>,
    pub image_puller: Arc<Mutex<ImagePuller>>,
    pub image_cache: Arc<Mutex<ImageCache>>,
    pub image_dir: PathBuf,
}

#[tonic::async_trait]
impl SandboxImageService for ImageDaemonService {
    async fn pull_image(
        &self,
        request: Request<PullImageRequest>,
    ) -> Result<Response<PullImageResponse>, Status> {
        let req = request.into_inner();
        let image_ref_str = req.image_ref;

        if image_ref_str.is_empty() {
            return Err(Status::invalid_argument("image_ref is required"));
        }

        // Check cache first.
        {
            let cache = self.image_cache.lock().await;
            if let Some(info) = cache.get(&image_ref_str) {
                if info.rootfs_path.exists() {
                    tracing::info!(image_ref = %image_ref_str, "image already cached");
                    return Ok(Response::new(PullImageResponse {
                        rootfs_path: info.rootfs_path.display().to_string(),
                        digest: info.digest.clone(),
                    }));
                }
            }
        }

        // Parse image reference.
        let image = ImageReference::parse(&image_ref_str)
            .map_err(|e| Status::invalid_argument(format!("{e}")))?;

        // Create OCI layout directory.
        let oci_dir = self
            .image_dir
            .join(format!("oci-{}", sanitize_image_ref(&image_ref_str)));

        // Pull from registry.
        tracing::info!(image_ref = %image_ref_str, oci_dir = %oci_dir.display(), "pulling from registry");
        let digest = self
            .registry_client
            .pull(&image, &oci_dir, None)
            .await
            .map_err(|e| Status::internal(format!("registry pull failed: {e}")))?;

        // Convert OCI layout to initramfs using ImagePuller.
        let mut puller = self.image_puller.lock().await;
        let mut info = puller
            .pull_oci_layout(&oci_dir)
            .map_err(|e| Status::internal(format!("image conversion failed: {e}")))?;

        // Override image_ref to use original reference (not layout path).
        info.image_ref = image_ref_str.clone();
        info.digest = digest;

        let rootfs_path = info.rootfs_path.display().to_string();
        let digest = info.digest.clone();

        // Cache the image info.
        {
            let mut cache = self.image_cache.lock().await;
            cache.insert(info);
        }

        Ok(Response::new(PullImageResponse {
            rootfs_path,
            digest,
        }))
    }

    async fn list_images(
        &self,
        _request: Request<ListImagesRequest>,
    ) -> Result<Response<ListImagesResponse>, Status> {
        let cache = self.image_cache.lock().await;
        let images = cache
            .list()
            .iter()
            .map(|info| ImageInfo {
                image_ref: info.image_ref.clone(),
                digest: info.digest.clone(),
                size_bytes: info.size_bytes,
                pulled_at: String::new(),
                format: info.format.to_string(),
                rootfs_path: info.rootfs_path.display().to_string(),
            })
            .collect();

        Ok(Response::new(ListImagesResponse { images }))
    }

    async fn remove_image(
        &self,
        request: Request<RemoveImageRequest>,
    ) -> Result<Response<RemoveImageResponse>, Status> {
        let req = request.into_inner();
        let mut cache = self.image_cache.lock().await;
        cache
            .remove(&req.image_ref)
            .map_err(|e| Status::not_found(format!("{e}")))?;
        Ok(Response::new(RemoveImageResponse {}))
    }

    async fn inspect_image(
        &self,
        request: Request<InspectImageRequest>,
    ) -> Result<Response<InspectImageResponse>, Status> {
        let req = request.into_inner();
        let cache = self.image_cache.lock().await;
        let info = cache
            .get(&req.image_ref)
            .ok_or_else(|| Status::not_found(format!("image not found: {}", req.image_ref)))?;

        Ok(Response::new(InspectImageResponse {
            info: Some(ImageInfo {
                image_ref: info.image_ref.clone(),
                digest: info.digest.clone(),
                size_bytes: info.size_bytes,
                pulled_at: String::new(),
                format: info.format.to_string(),
                rootfs_path: info.rootfs_path.display().to_string(),
            }),
        }))
    }
}
