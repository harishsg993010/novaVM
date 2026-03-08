//! NovaVM container runtime.
//!
//! Provides the core runtime for managing microVM sandboxes:
//! - Image management (pull, convert, cache)
//! - Sandbox lifecycle (create, start, stop, destroy)
//! - Network setup (TAP devices, bridges)

pub mod error;
pub mod image;
pub mod network;
pub mod pool;
pub mod sandbox;
pub mod snapshot_cache;

pub use error::{Result, RuntimeError};
pub use image::{BlobStore, CloneStrategy, ImageCache, ImageFormat, ImageInfo, ImagePuller, RootfsCache};
pub use network::TapDevice;
pub use pool::{PoolConfig, VmPool, WarmStrategy};
pub use sandbox::{Sandbox, SandboxConfig, SandboxKind, SandboxOrchestrator, SandboxState};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            vcpus: 2,
            memory_mib: 256,
            kernel: PathBuf::from("/boot/vmlinux"),
            rootfs: PathBuf::from("/images/rootfs.ext4"),
            cmdline: "console=ttyS0 reboot=k panic=1".to_string(),
            network: None,
            kind: SandboxKind::Vm,
        }
    }

    // -- Sandbox lifecycle tests -------------------------------------------

    #[test]
    fn test_sandbox_create_start_stop() {
        let config = test_config();
        let mut sb = Sandbox::new("test-1".to_string(), config);

        assert_eq!(sb.state(), SandboxState::Created);
        assert!(sb.pid().is_none());

        sb.start().expect("start should succeed");
        assert_eq!(sb.state(), SandboxState::Running);
        assert!(sb.pid().is_some());

        sb.stop().expect("stop should succeed");
        assert_eq!(sb.state(), SandboxState::Stopped);
        assert!(sb.pid().is_none());
    }

    #[test]
    fn test_sandbox_invalid_state_transition() {
        let config = test_config();
        let mut sb = Sandbox::new("test-2".to_string(), config);

        // Cannot stop a sandbox that isn't running.
        let err = sb.stop();
        assert!(err.is_err());

        // Start it.
        sb.start().unwrap();

        // Cannot start again.
        let err = sb.start();
        assert!(err.is_err());
    }

    // -- Orchestrator tests ------------------------------------------------

    #[test]
    fn test_orchestrator_lifecycle() {
        let mut orch = SandboxOrchestrator::new();
        assert_eq!(orch.count(), 0);

        // Create two sandboxes.
        orch.create("sb-1".to_string(), test_config()).unwrap();
        orch.create("sb-2".to_string(), test_config()).unwrap();
        assert_eq!(orch.count(), 2);

        // Duplicate ID should fail.
        let err = orch.create("sb-1".to_string(), test_config());
        assert!(err.is_err());

        // Start one.
        orch.start("sb-1").unwrap();
        assert_eq!(orch.get("sb-1").unwrap().state(), SandboxState::Running);

        // Stop it.
        orch.stop("sb-1").unwrap();
        assert_eq!(orch.get("sb-1").unwrap().state(), SandboxState::Stopped);

        // Destroy it.
        orch.destroy("sb-1").unwrap();
        assert_eq!(orch.count(), 1);

        // Cannot destroy non-existent.
        assert!(orch.destroy("sb-1").is_err());
    }

    #[test]
    fn test_orchestrator_cannot_destroy_running() {
        let mut orch = SandboxOrchestrator::new();
        orch.create("sb-run".to_string(), test_config()).unwrap();
        orch.start("sb-run").unwrap();

        // Cannot destroy a running sandbox.
        let err = orch.destroy("sb-run");
        assert!(err.is_err());

        // Stop first, then destroy.
        orch.stop("sb-run").unwrap();
        orch.destroy("sb-run").unwrap();
    }

    // -- Image puller tests ------------------------------------------------

    #[test]
    fn test_image_pull_creates_rootfs() {
        let dir = std::env::temp_dir().join("nova-runtime-test-images");
        let _ = std::fs::remove_dir_all(&dir);

        let puller = ImagePuller::new(&dir, ImageFormat::Raw).unwrap();
        let info = puller.pull("docker.io/library/nginx:latest").unwrap();

        assert_eq!(info.image_ref, "docker.io/library/nginx:latest");
        assert!(info.digest.starts_with("sha256:"));
        assert!(info.rootfs_path.exists());
        assert_eq!(info.format, ImageFormat::Raw);
        assert!(info.size_bytes > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Image cache tests -------------------------------------------------

    #[test]
    fn test_image_cache_insert_get_remove() {
        let dir = std::env::temp_dir().join("nova-runtime-test-cache");
        let _ = std::fs::remove_dir_all(&dir);

        let mut cache = ImageCache::new(&dir).unwrap();
        assert!(cache.list().is_empty());

        let info = ImageInfo {
            image_ref: "nginx:latest".to_string(),
            digest: "sha256:abc123".to_string(),
            rootfs_path: dir.join("nginx.raw"),
            format: ImageFormat::Raw,
            size_bytes: 1024,
            config: None,
        };

        cache.insert(info);
        assert!(cache.contains("nginx:latest"));
        assert!(!cache.contains("alpine:latest"));

        let retrieved = cache.get("nginx:latest").unwrap();
        assert_eq!(retrieved.digest, "sha256:abc123");

        assert_eq!(cache.list().len(), 1);

        // Remove.
        cache.remove("nginx:latest").unwrap();
        assert!(!cache.contains("nginx:latest"));
        assert!(cache.list().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- TAP device tests --------------------------------------------------

    #[test]
    fn test_tap_device_name_too_long() {
        let err = TapDevice::create("this_name_is_way_too_long_for_tap");
        assert!(err.is_err());
    }

    #[test]
    fn test_tap_device_creation_fallback() {
        // In CI/non-root environments, TAP creation gracefully falls back.
        let tap = TapDevice::create("tap_test0").unwrap();
        assert_eq!(tap.name(), "tap_test0");
        // fd may be -1 if /dev/net/tun is not available.
    }

    // -- Network config tests -----------------------------------------------

    #[test]
    fn test_sandbox_with_network() {
        let config = SandboxConfig {
            vcpus: 1,
            memory_mib: 128,
            kernel: PathBuf::from("/boot/vmlinux"),
            rootfs: PathBuf::from("/images/rootfs.ext4"),
            cmdline: "console=ttyS0".to_string(),
            kind: SandboxKind::Vm,
            network: Some(sandbox::NetworkConfig {
                tap_device: "tap0".to_string(),
                guest_ip: "172.16.0.2/24".to_string(),
                host_ip: "172.16.0.1/24".to_string(),
                mac_address: "AA:BB:CC:DD:EE:01".to_string(),
            }),
        };

        let sb = Sandbox::new("net-test".to_string(), config);
        assert!(sb.config().network.is_some());
        let net = sb.config().network.as_ref().unwrap();
        assert_eq!(net.tap_device, "tap0");
        assert_eq!(net.guest_ip, "172.16.0.2/24");
    }
}
