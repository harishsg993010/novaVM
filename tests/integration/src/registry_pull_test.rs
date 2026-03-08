//! OCI registry pull integration tests.
//!
//! Tests 1-5 are offline (parsing). Tests 6-7 require network access
//! and are gated behind `NOVAVM_REGISTRY_TESTS=1`. Test 8 is offline (cache).

use nova_api::registry::{ImageReference, RegistryClient};
use nova_runtime::ImageCache;

// ---------------------------------------------------------------------------
// Parsing tests (offline)
// ---------------------------------------------------------------------------

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
fn test_parse_ghcr_ref() {
    let r = ImageReference::parse("ghcr.io/owner/repo:v1").unwrap();
    assert_eq!(r.registry, "ghcr.io");
    assert_eq!(r.repository, "owner/repo");
    assert_eq!(r.reference, "v1");
}

#[test]
fn test_parse_default_tag() {
    let r = ImageReference::parse("busybox").unwrap();
    assert_eq!(r.reference, "latest");
    assert_eq!(r.repository, "library/busybox");
}

#[test]
fn test_parse_digest_ref() {
    let r = ImageReference::parse("quay.io/org/img@sha256:abcdef1234567890").unwrap();
    assert_eq!(r.registry, "quay.io");
    assert_eq!(r.repository, "org/img");
    assert_eq!(r.reference, "sha256:abcdef1234567890");
}

// ---------------------------------------------------------------------------
// Real pull tests (network-gated)
// ---------------------------------------------------------------------------

fn registry_tests_enabled() -> bool {
    std::env::var("NOVAVM_REGISTRY_TESTS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[tokio::test]
async fn test_real_pull_busybox() {
    if !registry_tests_enabled() {
        eprintln!("skipping test_real_pull_busybox (set NOVAVM_REGISTRY_TESTS=1)");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let client = RegistryClient::new();
    let image = ImageReference::parse("busybox:latest").unwrap();

    let digest = client.pull(&image, tmp.path(), None).await.unwrap();

    // Verify OCI layout was created.
    assert!(tmp.path().join("oci-layout").exists());
    assert!(tmp.path().join("index.json").exists());
    assert!(tmp.path().join("manifest.json").exists());
    assert!(tmp.path().join("blobs/sha256").exists());
    assert!(digest.starts_with("sha256:"));

    // Verify at least one blob exists.
    let blob_count = std::fs::read_dir(tmp.path().join("blobs/sha256"))
        .unwrap()
        .count();
    assert!(blob_count >= 2, "expected at least config + 1 layer, got {blob_count}");
}

#[tokio::test]
async fn test_real_pull_and_convert() {
    if !registry_tests_enabled() {
        eprintln!("skipping test_real_pull_and_convert (set NOVAVM_REGISTRY_TESTS=1)");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let oci_dir = tmp.path().join("busybox-oci");
    let image_dir = tmp.path().join("images");

    // Pull from registry.
    let client = RegistryClient::new();
    let image = ImageReference::parse("busybox:latest").unwrap();
    let digest = client.pull(&image, &oci_dir, None).await.unwrap();
    assert!(digest.starts_with("sha256:"));

    // Convert using ImagePuller.
    let mut puller = nova_runtime::ImagePuller::new(&image_dir, nova_runtime::ImageFormat::Initramfs).unwrap();
    let info = puller.pull_oci_layout(&oci_dir).unwrap();

    assert!(info.rootfs_path.exists());
    assert!(info.size_bytes > 0);
    assert_eq!(info.format, nova_runtime::ImageFormat::Initramfs);
}

// ---------------------------------------------------------------------------
// Cache integration test (offline)
// ---------------------------------------------------------------------------

#[test]
fn test_image_cache_integration() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = ImageCache::new(tmp.path()).unwrap();

    assert!(cache.list().is_empty());

    let info = nova_runtime::ImageInfo {
        image_ref: "nginx:alpine".to_string(),
        digest: "sha256:abc123".to_string(),
        rootfs_path: tmp.path().join("rootfs.cpio"),
        format: nova_runtime::ImageFormat::Initramfs,
        size_bytes: 1024,
        config: None,
    };

    // Write a dummy rootfs so it "exists".
    std::fs::write(&info.rootfs_path, b"dummy").unwrap();

    cache.insert(info);
    assert_eq!(cache.list().len(), 1);
    assert!(cache.contains("nginx:alpine"));

    let got = cache.get("nginx:alpine").unwrap();
    assert_eq!(got.digest, "sha256:abc123");
    assert_eq!(got.size_bytes, 1024);

    // Persistence: create a new cache from the same dir.
    let cache2 = ImageCache::new(tmp.path()).unwrap();
    assert_eq!(cache2.list().len(), 1);
}
