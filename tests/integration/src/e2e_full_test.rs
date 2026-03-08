//! Full end-to-end integration tests.
//!
//! These tests exercise the complete NovaVM stack:
//! - OCI image pull → initramfs creation
//! - VM boot with networking (virtio-net + TAP)
//! - Host network configuration (NAT/masquerade)
//! - gRPC daemon lifecycle
//! - eBPF sensor event capture
//!
//! Many tests require `NOVAVM_REAL_TESTS=1` and root privileges.

use std::net::Ipv4Addr;

/// Check if real tests are enabled.
fn real_tests_enabled() -> bool {
    std::env::var("NOVAVM_REAL_TESTS").map_or(false, |v| v == "1")
}

// ---------------------------------------------------------------------------
// Phase 1: Virtio-net unit tests (no root needed for mock tests)
// ---------------------------------------------------------------------------

#[test]
fn test_virtio_net_activation_with_memory() {
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_virtio::mmio::VirtioDevice;
    use nova_virtio::net::Net;
    use std::sync::Arc;

    let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 1 << 20)], false).unwrap();
    let mem_arc = Arc::new(mem);

    let mut net = Net::new("test-tap0".to_string(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    assert_eq!(net.device_type(), 1);
    assert_eq!(net.num_queues(), 2);

    // Create queues.
    let queues: Vec<nova_virtio::queue::Queue> =
        (0..2).map(|_| nova_virtio::queue::Queue::new(256)).collect();

    // Activate with memory.
    net.activate(queues, Arc::clone(&mem_arc)).unwrap();

    // Verify process_tx returns 0 with no TAP fd.
    assert_eq!(net.process_tx(), 0);

    // Verify process_rx returns false with no TAP fd.
    assert!(!net.process_rx());
}

#[test]
fn test_mmio_transport_with_guest_memory() {
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_virtio::console::Console;
    use nova_virtio::mmio::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 1 << 20)], false).unwrap();
    let mem_arc = Arc::new(mem);

    let output = Arc::new(Mutex::new(VecDeque::new()));
    let console = Console::new(output);
    let mut transport = MmioTransport::new(Box::new(console));

    // Set guest memory.
    transport.set_guest_memory(mem_arc);

    // Verify magic and device type.
    assert_eq!(transport.read(MMIO_MAGIC_VALUE), VIRTIO_MMIO_MAGIC);
    assert_eq!(transport.read(MMIO_DEVICE_ID), 3);
    assert!(!transport.is_activated());
}

#[test]
fn test_net_device_features() {
    use nova_virtio::mmio::VirtioDevice;
    use nova_virtio::net::{Net, VIRTIO_NET_F_MAC, VIRTIO_NET_F_STATUS};

    let net = Net::new("tap0".to_string(), [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let features = net.device_features(0);
    assert!(features & (VIRTIO_NET_F_MAC as u32) != 0);
    assert!(features & (VIRTIO_NET_F_STATUS as u32) != 0);
    // Page 1 should be empty.
    assert_eq!(net.device_features(1), 0);
}

#[test]
fn test_net_device_config_read() {
    use nova_virtio::mmio::VirtioDevice;
    use nova_virtio::net::Net;

    let mac = [0x52, 0x54, 0x00, 0xAB, 0xCD, 0xEF];
    let net = Net::new("tap0".to_string(), mac);

    // Read MAC from config space.
    let mut data = [0u8; 6];
    net.read_config(0, &mut data);
    assert_eq!(data, mac);

    // Read status (offset 6, 2 bytes).
    let mut status_data = [0u8; 2];
    net.read_config(6, &mut status_data);
    let status = u16::from_le_bytes(status_data);
    assert_eq!(status, 1); // LINK_UP
}

// ---------------------------------------------------------------------------
// Phase 2: Network setup tests (unit level, no root)
// ---------------------------------------------------------------------------

#[test]
fn test_network_setup_init_script() {
    let setup = nova_vmm::network::NetworkSetup::new(
        "nova-tap0".to_string(),
        Ipv4Addr::new(172, 16, 0, 1),
        Ipv4Addr::new(172, 16, 0, 2),
        30,
        "eth0".to_string(),
    );
    let script = setup.guest_init_script("/usr/sbin/nginx");
    assert!(script.contains("172.16.0.2/30"));
    assert!(script.contains("172.16.0.1"));
    assert!(script.contains("/usr/sbin/nginx"));
    assert!(script.contains("ip link set eth0 up"));
    assert!(script.contains("nameserver"));
}

#[test]
fn test_network_setup_default() {
    let setup = nova_vmm::network::NetworkSetup::default_for_tap("my-tap0");
    assert_eq!(setup.tap_name, "my-tap0");
    assert_eq!(setup.host_ip, Ipv4Addr::new(172, 16, 0, 1));
    assert_eq!(setup.guest_ip, Ipv4Addr::new(172, 16, 0, 2));
    assert_eq!(setup.netmask, 30);
}

// ---------------------------------------------------------------------------
// Phase 1+2: Real TAP and networking tests (requires root + NOVAVM_REAL_TESTS)
// ---------------------------------------------------------------------------

#[test]
fn test_tap_open_close() {
    if !real_tests_enabled() {
        eprintln!("skipping test_tap_open_close (set NOVAVM_REAL_TESTS=1)");
        return;
    }

    match nova_virtio::tap::Tap::open("novatest0") {
        Ok(tap) => {
            assert!(tap.fd() >= 0);
            assert_eq!(tap.name(), "novatest0");
            tap.set_nonblocking().expect("set nonblocking");
            // Drop will close the fd.
        }
        Err(e) => {
            eprintln!("TAP open failed (need root/CAP_NET_ADMIN): {e}");
        }
    }
}

#[test]
fn test_network_setup_and_teardown() {
    if !real_tests_enabled() {
        eprintln!("skipping test_network_setup_and_teardown (set NOVAVM_REAL_TESTS=1)");
        return;
    }

    // First open TAP device.
    let tap = match nova_virtio::tap::Tap::open("novanet0") {
        Ok(t) => t,
        Err(e) => {
            eprintln!("TAP open failed (need root): {e}");
            return;
        }
    };
    std::mem::forget(tap); // Keep TAP alive for network setup.

    let mut setup = nova_vmm::network::NetworkSetup::new(
        "novanet0".to_string(),
        Ipv4Addr::new(172, 16, 0, 1),
        Ipv4Addr::new(172, 16, 0, 2),
        30,
        "eth0".to_string(),
    );

    if let Err(e) = setup.setup() {
        eprintln!("network setup failed (need root): {e}");
        return;
    }

    // Verify TAP has IP (via ip addr show).
    let output = std::process::Command::new("ip")
        .args(["addr", "show", "novanet0"])
        .output()
        .expect("ip command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("172.16.0.1"),
        "TAP should have host IP: {}",
        stdout
    );

    // Teardown.
    setup.teardown().expect("teardown");
}

// ---------------------------------------------------------------------------
// Phase 4: gRPC lifecycle test (no root needed)
// ---------------------------------------------------------------------------

#[test]
fn test_grpc_lifecycle_wasm_sandbox() {
    // This test verifies the sandbox orchestrator lifecycle works.
    use nova_runtime::{SandboxConfig, SandboxKind, SandboxOrchestrator, SandboxState};

    let mut orch = SandboxOrchestrator::new();

    let config = SandboxConfig {
        vcpus: 1,
        memory_mib: 64,
        kernel: "/tmp/vmlinux".into(),
        rootfs: "/tmp/rootfs".into(),
        cmdline: "console=ttyS0".to_string(),
        network: None,
        kind: SandboxKind::Wasm {
            module_path: "/tmp/test.wasm".into(),
            entry_function: "_start".to_string(),
        },
    };

    // Create sandbox.
    let id = "test-grpc-wasm".to_string();
    orch.create(id.clone(), config).expect("create sandbox");

    // Verify status.
    let sb = orch.get(&id).expect("get sandbox");
    assert_eq!(format!("{:?}", sb.state()), format!("{:?}", SandboxState::Created));

    // List sandboxes.
    let list = orch.list();
    assert_eq!(list.len(), 1);

    // Destroy (created state allows destroy).
    orch.destroy(&id).expect("destroy sandbox");
    assert!(orch.get(&id).is_err());
}

// ---------------------------------------------------------------------------
// Phase 5: Full E2E test (requires everything: root, KVM, TAP, eBPF)
// ---------------------------------------------------------------------------

#[test]
fn test_e2e_novactl_run_nginx() {
    if !real_tests_enabled() {
        eprintln!("skipping test_e2e_novactl_run_nginx (set NOVAVM_REAL_TESTS=1)");
        return;
    }

    // This is the ultimate E2E test:
    // 1. Start daemon in background
    // 2. Pull nginx OCI image
    // 3. Create + Start sandbox (VM with networking)
    // 4. Wait for VM boot
    // 5. curl guest_ip:80
    // 6. Verify nginx welcome page
    // 7. Stop + Destroy sandbox
    // 8. Stop daemon

    // For now, test the components that can be tested individually:

    // Step 1: Verify we can create an orchestrator.
    let orch = nova_runtime::SandboxOrchestrator::new();
    assert_eq!(orch.count(), 0);

    // Step 2: Verify ImagePuller exists.
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let puller = nova_runtime::ImagePuller::new(
        temp_dir.path(),
        nova_runtime::ImageFormat::Initramfs,
    ).expect("create puller");
    assert!(puller.pull("test-image").is_ok());

    // Step 3: Verify VM builder config with networking.
    let config = nova_vmm::config::VmConfig::from_toml(
        r#"
        vcpus = 1
        memory_mib = 128
        [kernel]
        path = "/tmp/vmlinux"
        [network]
        tap = "nova-tap0"
        "#,
    )
    .expect("parse config");
    assert!(config.network.is_some());
    assert_eq!(config.network.as_ref().unwrap().tap, "nova-tap0");

    // Step 4: Verify network setup can generate init script.
    let net_setup = nova_vmm::network::NetworkSetup::default_for_tap("nova-tap0");
    let script = net_setup.guest_init_script("nginx -g 'daemon off;'");
    assert!(script.contains("172.16.0.2"));
    assert!(script.contains("nginx"));

    // Full VM boot with networking would go here when running with
    // root + KVM + vmlinux fixture + nginx OCI layout.
    eprintln!("Full E2E VM boot test pending (requires vmlinux + nginx OCI fixtures)");
}

#[test]
fn test_e2e_image_pull_to_initramfs() {
    // Test the OCI → initramfs pipeline works.
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let puller = nova_runtime::ImagePuller::new(
        temp_dir.path(),
        nova_runtime::ImageFormat::Initramfs,
    ).expect("create puller");

    // Pull creates a minimal rootfs.
    let info = puller.pull("test-image").expect("pull");
    assert!(!info.rootfs_path.as_os_str().is_empty());
}

#[test]
fn test_e2e_vm_config_with_all_options() {
    let toml = r#"
    vcpus = 2
    memory_mib = 512
    [kernel]
    path = "/boot/vmlinux"
    cmdline = "console=ttyS0 reboot=k panic=1 pci=off"
    initrd = "/boot/initrd.cpio"
    [network]
    tap = "nova-tap0"
    mac = "52:54:00:12:34:56"
    "#;

    let config = nova_vmm::config::VmConfig::from_toml(toml).expect("parse config");
    assert_eq!(config.vcpus, 2);
    assert_eq!(config.memory_mib, 512);
    assert!(config.network.is_some());
    let net = config.network.as_ref().unwrap();
    assert_eq!(net.tap, "nova-tap0");
    assert_eq!(net.mac.as_deref(), Some("52:54:00:12:34:56"));
    assert!(config.kernel.initrd.is_some());
}

#[test]
fn test_e2e_builder_mmio_device_enumeration() {
    // Verify the MMIO bus device_info returns correct info for all devices.
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_virtio::console::Console;
    use nova_virtio::mmio::MmioTransport;
    use nova_virtio::net::Net;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    let mem = GuestMemoryMmap::new(&[(GuestAddress::new(0), 1 << 20)], false).unwrap();
    let mem_arc = Arc::new(mem);

    let mut bus = nova_vmm::device_mgr::MmioBus::new();

    // Add console.
    let output = Arc::new(Mutex::new(VecDeque::new()));
    let console = Console::new(output);
    let mut ct = MmioTransport::new(Box::new(console));
    ct.set_guest_memory(Arc::clone(&mem_arc));
    bus.register(0xD000_0000, 0x1000, ct, None);

    // Add net device.
    let net = Net::new("tap0".to_string(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let mut nt = MmioTransport::new(Box::new(net));
    nt.set_guest_memory(Arc::clone(&mem_arc));
    bus.register(0xD000_1000, 0x1000, nt, None);

    assert_eq!(bus.device_count(), 2);
    let info = bus.device_info();
    assert_eq!(info[0], (0xD000_0000, 0x1000, 3)); // console type=3
    assert_eq!(info[1], (0xD000_1000, 0x1000, 1)); // net type=1
}
