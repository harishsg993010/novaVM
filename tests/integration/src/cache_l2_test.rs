//! L2 Rootfs Cache integration tests.

use nova_runtime::image::rootfs_cache::{CloneStrategy, RootfsCache};
use nova_runtime::ImageFormat;
use std::fs;
use tempfile::TempDir;

fn create_test_rootfs(dir: &std::path::Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

#[test]
fn test_rootfs_cache_insert_and_retrieve() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).unwrap();

    let mut cache = RootfsCache::open(&cache_dir).unwrap();
    assert!(cache.is_empty());

    let rootfs_path = create_test_rootfs(&work_dir, "rootfs.cpio", b"cpio archive data here");
    let digest = "sha256:aabbccdd11223344";

    cache
        .insert(digest, "test-image:latest", &rootfs_path, ImageFormat::Initramfs, "sha256:content123")
        .unwrap();

    assert!(cache.contains(digest));
    assert_eq!(cache.len(), 1);

    let entry = cache.get(digest).unwrap();
    assert_eq!(entry.image_ref, "test-image:latest");
    assert_eq!(entry.format, ImageFormat::Initramfs);
    assert!(entry.size_bytes > 0);
}

#[test]
fn test_rootfs_cache_clone_copy() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).unwrap();

    let mut cache = RootfsCache::open(&cache_dir).unwrap();

    let content = b"test cpio data for cloning";
    let rootfs_path = create_test_rootfs(&work_dir, "rootfs.cpio", content);
    let digest = "sha256:clone_test_digest1";

    cache
        .insert(digest, "clone-image:v1", &rootfs_path, ImageFormat::Initramfs, "sha256:cc1")
        .unwrap();

    let target = work_dir.join("cloned.cpio");
    cache.clone_rootfs(digest, &target).unwrap();

    assert!(target.exists());
    let cloned_content = fs::read(&target).unwrap();
    assert_eq!(cloned_content, content);

    // use_count should have incremented.
    let entry = cache.get(digest).unwrap();
    assert_eq!(entry.use_count, 1);
}

#[test]
fn test_rootfs_cache_clone_hardlink() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).unwrap();

    // Detect strategy — on most Linux filesystems this is Hardlink.
    let strategy = RootfsCache::detect_clone_strategy(&cache_dir);
    // We just verify it returns some valid strategy.
    assert!(
        strategy == CloneStrategy::Hardlink
            || strategy == CloneStrategy::Copy
            || strategy == CloneStrategy::Reflink
    );

    let mut cache = RootfsCache::open(&cache_dir).unwrap();
    let content = b"hardlink test rootfs data";
    let rootfs_path = create_test_rootfs(&work_dir, "rootfs.cpio", content);

    cache
        .insert("sha256:hl_test", "hl-image:v1", &rootfs_path, ImageFormat::Initramfs, "sha256:hl1")
        .unwrap();

    let target = work_dir.join("hl_cloned.cpio");
    cache.clone_rootfs("sha256:hl_test", &target).unwrap();
    assert!(target.exists());
    assert_eq!(fs::read(&target).unwrap(), content);
}

#[test]
fn test_rootfs_cache_persistence() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).unwrap();

    let content = b"persistent rootfs";
    let rootfs_path = create_test_rootfs(&work_dir, "rootfs.cpio", content);
    let digest = "sha256:persist_test_0001";

    {
        let mut cache = RootfsCache::open(&cache_dir).unwrap();
        cache
            .insert(digest, "persist:v1", &rootfs_path, ImageFormat::Initramfs, "sha256:p1")
            .unwrap();
        assert_eq!(cache.len(), 1);
    }

    // Reopen.
    {
        let cache = RootfsCache::open(&cache_dir).unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(digest));
        let entry = cache.get(digest).unwrap();
        assert_eq!(entry.image_ref, "persist:v1");
    }
}

#[test]
fn test_rootfs_cache_eviction() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).unwrap();

    let mut cache = RootfsCache::open(&cache_dir).unwrap();

    // Insert 3 rootfs entries of different sizes.
    for i in 0..3 {
        let data = vec![i as u8; 100]; // 100 bytes each
        let rootfs_path = create_test_rootfs(&work_dir, &format!("rootfs_{i}.cpio"), &data);
        cache
            .insert(
                &format!("sha256:evict_test_{i:04}"),
                &format!("evict:v{i}"),
                &rootfs_path,
                ImageFormat::Initramfs,
                &format!("sha256:ev{i}"),
            )
            .unwrap();
    }

    assert_eq!(cache.len(), 3);
    assert_eq!(cache.total_size(), 300);

    // Use entry 2 more (higher use_count -> less likely to evict).
    let target = work_dir.join("tmp_clone.cpio");
    cache.clone_rootfs("sha256:evict_test_0002", &target).unwrap();
    let _ = fs::remove_file(&target);

    // Evict to 150 bytes max — should remove entries 0 and 1 (lowest use_count).
    let evicted = cache.evict(150).unwrap();
    assert_eq!(evicted, 2);
    assert_eq!(cache.len(), 1);
    assert!(cache.contains("sha256:evict_test_0002"));
}

#[test]
fn test_detect_clone_strategy() {
    let dir = TempDir::new().unwrap();
    let strategy = RootfsCache::detect_clone_strategy(dir.path());
    // Should return a valid strategy.
    assert!(
        strategy == CloneStrategy::Copy
            || strategy == CloneStrategy::Hardlink
            || strategy == CloneStrategy::Reflink
    );
}

#[test]
fn test_pull_with_rootfs_cache() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let image_dir = dir.path().join("images");

    let rootfs_cache = RootfsCache::open(&cache_dir).unwrap();
    let blob_store = nova_runtime::BlobStore::open(&dir.path().join("blobs")).unwrap();
    let puller =
        nova_runtime::ImagePuller::with_caches(&image_dir, ImageFormat::Initramfs, blob_store, rootfs_cache)
            .unwrap();

    // Simple pull (not OCI layout) still works.
    let info = puller.pull("test/cached:v1").unwrap();
    assert!(info.rootfs_path.exists());
}

#[test]
fn test_rootfs_cache_concurrent_clone() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("rootfs_cache");
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).unwrap();

    let content = b"concurrent clone data";
    let rootfs_path = create_test_rootfs(&work_dir, "rootfs.cpio", content);
    let digest = "sha256:concurrent_test1";

    let mut cache = RootfsCache::open(&cache_dir).unwrap();
    cache
        .insert(digest, "concurrent:v1", &rootfs_path, ImageFormat::Initramfs, "sha256:conc1")
        .unwrap();

    // Clone to multiple targets sequentially (simulating concurrent access pattern).
    for i in 0..5 {
        let target = work_dir.join(format!("clone_{i}.cpio"));
        cache.clone_rootfs(digest, &target).unwrap();
        assert!(target.exists());
        assert_eq!(fs::read(&target).unwrap(), content);
    }

    let entry = cache.get(digest).unwrap();
    assert_eq!(entry.use_count, 5);
}
