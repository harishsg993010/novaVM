//! Networking and virtio integration tests.
//!
//! Tests that virtio devices, MMIO transport, and runtime networking
//! components work together correctly.

use nova_virtio::console::{Console, ConsoleOutput};
use nova_virtio::mmio::{
    MmioTransport, MMIO_DEVICE_ID, MMIO_MAGIC_VALUE, MMIO_STATUS, MMIO_VERSION, STATUS_ACKNOWLEDGE,
    STATUS_DRIVER, VIRTIO_MMIO_MAGIC, VIRTIO_MMIO_VERSION,
};
use nova_virtio::queue::Queue;

/// Test that MmioTransport reads correct magic and version for console device.
#[test]
fn test_mmio_transport_console_identity() {
    let output: ConsoleOutput = Default::default();
    let console = Console::new(output);
    let transport = MmioTransport::new(Box::new(console));

    // Magic value should be "virt" (0x74726976).
    assert_eq!(transport.read(MMIO_MAGIC_VALUE as u64), VIRTIO_MMIO_MAGIC);

    // MMIO version should be 2 (modern).
    assert_eq!(transport.read(MMIO_VERSION as u64), VIRTIO_MMIO_VERSION);

    // Device type should be 3 (console).
    assert_eq!(transport.read(MMIO_DEVICE_ID as u64), 3);
}

/// Test the MMIO status register negotiation flow.
#[test]
fn test_mmio_status_negotiation() {
    let output: ConsoleOutput = Default::default();
    let console = Console::new(output);
    let mut transport = MmioTransport::new(Box::new(console));

    // Initial status should be 0.
    assert_eq!(transport.read(MMIO_STATUS as u64), 0);

    // Guest acknowledges the device.
    transport.write(MMIO_STATUS as u64, STATUS_ACKNOWLEDGE);
    assert_eq!(
        transport.read(MMIO_STATUS as u64) & STATUS_ACKNOWLEDGE,
        STATUS_ACKNOWLEDGE
    );

    // Guest sets DRIVER bit.
    transport.write(MMIO_STATUS as u64, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    let status = transport.read(MMIO_STATUS as u64);
    assert_ne!(status & STATUS_DRIVER, 0);
}

/// Test that a virtqueue can be created with valid size.
#[test]
fn test_virtqueue_creation() {
    let _queue = Queue::new(256);

    // Verify queue with max size.
    let _big_queue = Queue::new(1024);

    // Zero-size queue.
    let _zero_queue = Queue::new(0);
}

/// Test console device creation and output handle sharing.
#[test]
fn test_console_output_sharing() {
    let output: ConsoleOutput = Default::default();
    let console = Console::new(output.clone());
    let handle = console.output_handle();

    // Write to the shared output buffer directly.
    {
        let mut buf = handle.lock().unwrap();
        buf.push_back(b'H');
        buf.push_back(b'i');
    }

    // Both handles should see the same data.
    let buf = output.lock().unwrap();
    assert_eq!(buf.len(), 2);
    assert_eq!(buf[0], b'H');
    assert_eq!(buf[1], b'i');
}

/// Test that MMIO transport reset zeroes the status.
#[test]
fn test_mmio_transport_reset() {
    let output: ConsoleOutput = Default::default();
    let console = Console::new(output);
    let mut transport = MmioTransport::new(Box::new(console));

    // Set some status.
    transport.write(MMIO_STATUS as u64, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    assert_ne!(transport.read(MMIO_STATUS as u64), 0);

    // Writing 0 to status triggers device reset.
    transport.write(MMIO_STATUS as u64, 0);
    assert_eq!(transport.read(MMIO_STATUS as u64), 0);
}

/// Test runtime sandbox orchestrator lifecycle with network config.
#[test]
fn test_sandbox_lifecycle_with_network() {
    use nova_runtime::sandbox::NetworkConfig;
    use nova_runtime::{SandboxConfig, SandboxKind, SandboxOrchestrator, SandboxState};

    let mut orchestrator = SandboxOrchestrator::new();

    let config = SandboxConfig {
        vcpus: 2,
        memory_mib: 256,
        kernel: "/boot/vmlinuz".into(),
        rootfs: "/images/rootfs.ext4".into(),
        cmdline: "console=ttyS0 reboot=k".to_string(),
        kind: SandboxKind::Vm,
        network: Some(NetworkConfig {
            tap_device: "tap0".to_string(),
            guest_ip: "10.0.0.2/24".to_string(),
            host_ip: "10.0.0.1/24".to_string(),
            mac_address: "02:00:00:00:00:01".to_string(),
        }),
    };

    // Create.
    orchestrator.create("net-sb-1".to_string(), config).unwrap();
    let sb = orchestrator.get("net-sb-1").unwrap();
    assert_eq!(sb.state(), SandboxState::Created);
    assert!(sb.config().network.is_some());

    let net = sb.config().network.as_ref().unwrap();
    assert_eq!(net.tap_device, "tap0");
    assert_eq!(net.guest_ip, "10.0.0.2/24");

    // Start.
    orchestrator.start("net-sb-1").unwrap();
    let sb = orchestrator.get("net-sb-1").unwrap();
    assert_eq!(sb.state(), SandboxState::Running);
    assert!(sb.pid().is_some());

    // Stop.
    orchestrator.stop("net-sb-1").unwrap();
    let sb = orchestrator.get("net-sb-1").unwrap();
    assert_eq!(sb.state(), SandboxState::Stopped);
    assert!(sb.pid().is_none());

    // Destroy.
    orchestrator.destroy("net-sb-1").unwrap();
    assert!(orchestrator.get("net-sb-1").is_err());
}
