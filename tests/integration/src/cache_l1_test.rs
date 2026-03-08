//! L1 Blob Store integration tests.

use nova_runtime::image::blob_store::BlobStore;
use nova_runtime::{ImageCache, ImageFormat, ImageInfo, ImagePuller};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tempfile::TempDir;

fn make_digest(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

#[test]
fn test_blob_store_insert_and_retrieve() {
    let dir = TempDir::new().unwrap();
    let blob_dir = dir.path().join("blobs");

    let mut store = BlobStore::open(&blob_dir).unwrap();
    assert!(store.is_empty());

    let data = b"hello world layer data";
    let digest = make_digest(data);

    store
        .insert(&digest, "application/vnd.oci.image.layer.v1.tar+gzip", data)
        .unwrap();

    assert!(store.contains(&digest));
    assert_eq!(store.len(), 1);

    let path = store.blob_path(&digest).unwrap();
    assert!(path.exists());
    let stored = std::fs::read(&path).unwrap();
    assert_eq!(stored, data);
}

#[test]
fn test_blob_store_digest_verification() {
    let dir = TempDir::new().unwrap();
    let blob_dir = dir.path().join("blobs");

    let mut store = BlobStore::open(&blob_dir).unwrap();

    let data = b"some data";
    let wrong_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    let result = store.insert(wrong_digest, "application/octet-stream", data);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("digest mismatch"), "got: {err}");
}

#[test]
fn test_blob_store_persistence() {
    let dir = TempDir::new().unwrap();
    let blob_dir = dir.path().join("blobs");

    let data = b"persistent layer data";
    let digest = make_digest(data);

    // Insert and drop.
    {
        let mut store = BlobStore::open(&blob_dir).unwrap();
        store
            .insert(&digest, "application/vnd.oci.image.layer.v1.tar+gzip", data)
            .unwrap();
        assert_eq!(store.len(), 1);
    }

    // Reopen and verify.
    {
        let store = BlobStore::open(&blob_dir).unwrap();
        assert_eq!(store.len(), 1);
        assert!(store.contains(&digest));
        let path = store.blob_path(&digest).unwrap();
        let stored = std::fs::read(path).unwrap();
        assert_eq!(stored, data);
    }
}

#[test]
fn test_blob_store_gc() {
    let dir = TempDir::new().unwrap();
    let blob_dir = dir.path().join("blobs");

    let mut store = BlobStore::open(&blob_dir).unwrap();

    let data1 = b"layer one";
    let digest1 = make_digest(data1);
    store.insert(&digest1, "application/octet-stream", data1).unwrap();

    let data2 = b"layer two";
    let digest2 = make_digest(data2);
    store.insert(&digest2, "application/octet-stream", data2).unwrap();

    // Release both refs to zero.
    store.release_ref(&digest1, false).unwrap();
    store.release_ref(&digest2, false).unwrap();

    // GC with zero max_age should remove both.
    let removed = store.gc(Duration::from_secs(0)).unwrap();
    assert_eq!(removed, 2);
    assert!(store.is_empty());
}

#[test]
fn test_blob_store_ref_counting() {
    let dir = TempDir::new().unwrap();
    let blob_dir = dir.path().join("blobs");

    let mut store = BlobStore::open(&blob_dir).unwrap();

    let data = b"ref counted layer";
    let digest = make_digest(data);

    store.insert(&digest, "application/octet-stream", data).unwrap();
    // ref_count starts at 1.

    store.add_ref(&digest).unwrap(); // now 2
    store.add_ref(&digest).unwrap(); // now 3

    // Release with remove_if_zero=true, but count is still > 0.
    store.release_ref(&digest, true).unwrap(); // 2
    assert!(store.contains(&digest));

    store.release_ref(&digest, true).unwrap(); // 1
    assert!(store.contains(&digest));

    store.release_ref(&digest, true).unwrap(); // 0 -> removed
    assert!(!store.contains(&digest));
    assert!(store.is_empty());
}

#[test]
fn test_image_puller_with_blob_cache() {
    let dir = TempDir::new().unwrap();
    let image_dir = dir.path().join("images");
    let blob_dir = dir.path().join("blobs");

    let blob_store = BlobStore::open(&blob_dir).unwrap();
    let puller = ImagePuller::with_blob_store(&image_dir, ImageFormat::Raw, blob_store).unwrap();

    // Pull creates a minimal rootfs (not OCI layout, so no blob caching).
    let info = puller.pull("docker.io/library/test:latest").unwrap();
    assert!(info.rootfs_path.exists());
    assert_eq!(info.format, ImageFormat::Raw);
}

#[test]
fn test_image_cache_persistence() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("cache");

    // Insert and drop.
    {
        let mut cache = ImageCache::new(&cache_dir).unwrap();
        cache.insert(ImageInfo {
            image_ref: "alpine:3.18".to_string(),
            digest: "sha256:abc123".to_string(),
            rootfs_path: cache_dir.join("alpine.raw"),
            format: ImageFormat::Raw,
            size_bytes: 2048,
            config: None,
        });
        assert_eq!(cache.list().len(), 1);
    }

    // Reopen and verify persistence.
    {
        let cache = ImageCache::new(&cache_dir).unwrap();
        assert_eq!(cache.list().len(), 1);
        assert!(cache.contains("alpine:3.18"));
        let info = cache.get("alpine:3.18").unwrap();
        assert_eq!(info.digest, "sha256:abc123");
        assert_eq!(info.size_bytes, 2048);
    }
}

#[test]
fn test_blob_store_total_size() {
    let dir = TempDir::new().unwrap();
    let blob_dir = dir.path().join("blobs");

    let mut store = BlobStore::open(&blob_dir).unwrap();
    assert_eq!(store.total_size(), 0);

    let data1 = b"aaaa"; // 4 bytes
    let digest1 = make_digest(data1);
    store.insert(&digest1, "application/octet-stream", data1).unwrap();

    let data2 = b"bbbbbbbb"; // 8 bytes
    let digest2 = make_digest(data2);
    store.insert(&digest2, "application/octet-stream", data2).unwrap();

    assert_eq!(store.total_size(), 12);
    assert_eq!(store.len(), 2);
}
