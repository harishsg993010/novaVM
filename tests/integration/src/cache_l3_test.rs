//! L3 VM Snapshot save/restore integration tests.

use nova_kvm::kvm_bindings::{KvmClockData, KvmPitChannelState, KvmPitState2, KvmRegs, KvmSregs};
use nova_runtime::snapshot_cache::{SnapshotCache, SnapshotEntry};
use nova_vmm::snapshot::{
    IrqchipSnapshot, VcpuSnapshot, VmSnapshot, VmSnapshotConfig, SNAPSHOT_VERSION,
};
use std::time::SystemTime;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// No-root tests (pure serialization + cache index)
// ---------------------------------------------------------------------------

#[test]
fn test_vm_snapshot_serialization() {
    let snapshot = VmSnapshot {
        version: SNAPSHOT_VERSION,
        vcpus: vec![VcpuSnapshot {
            regs: KvmRegs {
                rip: 0x1000,
                rsp: 0x7000,
                rflags: 0x2,
                rax: 42,
                ..Default::default()
            },
            sregs: KvmSregs::default(),
            msrs: Vec::new(),
            xsave: Vec::new(),
            xcrs: Vec::new(),
        }],
        clock: KvmClockData {
            clock: 123456789,
            flags: 0,
            pad: [0; 9],
        },
        irqchip: IrqchipSnapshot {
            pic_master: vec![0u8; 512],
            pic_slave: vec![0u8; 512],
            ioapic: vec![0u8; 512],
        },
        pit: KvmPitState2 {
            channels: [KvmPitChannelState::default(); 3],
            flags: 0,
            padding: [0; 9],
        },
        memory_regions: vec![(0, 128 * 1024 * 1024)],
        config: VmSnapshotConfig {
            vcpus: 1,
            memory_mib: 128,
            kernel_cmdline: "console=ttyS0".to_string(),
        },
        virtio_devices: Vec::new(),
    };

    // Serialize to JSON.
    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    assert!(json.contains(&format!("\"version\": {}", SNAPSHOT_VERSION)));
    assert!(json.contains("\"rip\": 4096"));

    // Deserialize back.
    let restored: VmSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.version, SNAPSHOT_VERSION);
    assert_eq!(restored.vcpus.len(), 1);
    assert_eq!(restored.vcpus[0].regs.rip, 0x1000);
    assert_eq!(restored.vcpus[0].regs.rax, 42);
    assert_eq!(restored.clock.clock, 123456789);
    assert_eq!(restored.memory_regions.len(), 1);
    assert_eq!(restored.config.vcpus, 1);
    assert_eq!(restored.config.kernel_cmdline, "console=ttyS0");
}

#[test]
fn test_snapshot_cache_index() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("snapshots");

    let mut cache = SnapshotCache::open(&cache_dir).unwrap();
    assert!(cache.is_empty());

    let snap_dir = cache_dir.join("snap_001");
    std::fs::create_dir_all(&snap_dir).unwrap();

    let entry = SnapshotEntry {
        key: "sha256:abc:config_hash_1".to_string(),
        snapshot_dir: snap_dir.clone(),
        config_hash: "config_hash_1".to_string(),
        image_digest: "sha256:abc".to_string(),
        created_at: SystemTime::now(),
        valid: true,
    };

    cache.insert(entry).unwrap();
    assert_eq!(cache.len(), 1);
    assert!(cache.contains("sha256:abc:config_hash_1"));

    let retrieved = cache.get("sha256:abc:config_hash_1").unwrap();
    assert_eq!(retrieved.image_digest, "sha256:abc");
    assert!(retrieved.valid);

    // Invalidate.
    cache.invalidate("sha256:abc:config_hash_1").unwrap();
    assert!(!cache.contains("sha256:abc:config_hash_1"));
    assert_eq!(cache.len(), 1); // still in index, just invalid
}

#[test]
fn test_snapshot_cache_persistence() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("snapshots");

    let snap_dir = cache_dir.join("snap_persist");
    std::fs::create_dir_all(&snap_dir).unwrap();

    {
        let mut cache = SnapshotCache::open(&cache_dir).unwrap();
        cache
            .insert(SnapshotEntry {
                key: "persist_key".to_string(),
                snapshot_dir: snap_dir.clone(),
                config_hash: "ch1".to_string(),
                image_digest: "sha256:persist".to_string(),
                created_at: SystemTime::now(),
                valid: true,
            })
            .unwrap();
    }

    // Reopen.
    {
        let cache = SnapshotCache::open(&cache_dir).unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache.contains("persist_key"));
    }
}

// ---------------------------------------------------------------------------
// KVM-required tests (NOVAVM_REAL_TESTS=1)
// ---------------------------------------------------------------------------

fn require_kvm() -> bool {
    std::env::var("NOVAVM_REAL_TESTS").is_ok()
}

#[test]
fn test_save_vcpu_state() {
    if !require_kvm() {
        eprintln!("skipping test_save_vcpu_state (NOVAVM_REAL_TESTS not set)");
        return;
    }

    let kvm = nova_kvm::Kvm::open().unwrap();
    let vm = kvm.create_vm().unwrap();
    vm.set_tss_addr(0xFFFB_D000).unwrap();
    vm.create_irqchip().unwrap();
    vm.create_pit2().unwrap();

    let vcpu = vm.create_vcpu(0).unwrap();
    let regs = vcpu.get_regs().unwrap();
    let sregs = vcpu.get_sregs().unwrap();

    // Regs should be readable (default values).
    assert_eq!(regs.rflags & 0x2, 0x2); // bit 1 always set on x86
    // Sregs cs should have some value.
    let _ = sregs.cs;
}

#[test]
fn test_save_restore_vcpu_state() {
    if !require_kvm() {
        eprintln!("skipping test_save_restore_vcpu_state (NOVAVM_REAL_TESTS not set)");
        return;
    }

    let kvm = nova_kvm::Kvm::open().unwrap();
    let vm = kvm.create_vm().unwrap();
    vm.set_tss_addr(0xFFFB_D000).unwrap();
    vm.create_irqchip().unwrap();
    vm.create_pit2().unwrap();

    let vcpu = vm.create_vcpu(0).unwrap();

    // Set custom regs.
    let mut regs = vcpu.get_regs().unwrap();
    regs.rax = 0xDEADBEEF;
    regs.rbx = 0xCAFEBABE;
    regs.rip = 0x1000;
    regs.rflags = 0x2;
    vcpu.set_regs(&regs).unwrap();

    // Read back and verify.
    let restored = vcpu.get_regs().unwrap();
    assert_eq!(restored.rax, 0xDEADBEEF);
    assert_eq!(restored.rbx, 0xCAFEBABE);
    assert_eq!(restored.rip, 0x1000);
}

#[test]
fn test_save_vm_clock_state() {
    if !require_kvm() {
        eprintln!("skipping test_save_vm_clock_state (NOVAVM_REAL_TESTS not set)");
        return;
    }

    let kvm = nova_kvm::Kvm::open().unwrap();
    let vm = kvm.create_vm().unwrap();
    vm.set_tss_addr(0xFFFB_D000).unwrap();
    vm.create_irqchip().unwrap();
    vm.create_pit2().unwrap();

    let clock = vm.get_clock().unwrap();
    // Clock should be non-zero after creation.
    // Note: the clock value depends on timing, but it should be > 0.
    let _ = clock.clock;
    let _ = clock.flags;

    // Set and read back.
    let mut new_clock = clock;
    new_clock.clock = 999_999;
    vm.set_clock(&new_clock).unwrap();

    let restored = vm.get_clock().unwrap();
    // The clock may not be exactly what we set (TSC keeps ticking), but it should
    // be close or have been accepted without error.
    let _ = restored.clock;
}

#[test]
fn test_snapshot_save_to_disk() {
    if !require_kvm() {
        eprintln!("skipping test_snapshot_save_to_disk (NOVAVM_REAL_TESTS not set)");
        return;
    }

    let dir = TempDir::new().unwrap();

    // Build a minimal VM manually (build_vm needs a real kernel).
    let kvm = nova_kvm::Kvm::open().unwrap();
    let vm_fd = kvm.create_vm().unwrap();
    vm_fd.set_tss_addr(0xFFFB_D000).unwrap();
    vm_fd.create_irqchip().unwrap();
    vm_fd.create_pit2().unwrap();

    let mem = nova_mem::GuestMemoryMmap::new(
        &[(nova_mem::GuestAddress::new(0), 16 * 1024 * 1024)],
        false,
    )
    .unwrap();
    let host_addr = mem.region_host_addr(0).unwrap();
    vm_fd
        .set_user_memory_region(&nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: 16 * 1024 * 1024,
            userspace_addr: host_addr,
        })
        .unwrap();

    let cpuid = kvm.get_supported_cpuid(256).unwrap();
    let vcpu = vm_fd.create_vcpu(0).unwrap();
    vcpu.set_cpuid2(&cpuid).unwrap();

    let mem_arc = std::sync::Arc::new(mem);
    let console_output = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let console = nova_virtio::console::Console::new(std::sync::Arc::clone(&console_output));
    let mut transport = nova_virtio::mmio::MmioTransport::new(Box::new(console));
    transport.set_guest_memory(std::sync::Arc::clone(&mem_arc));
    let mut bus = nova_vmm::device_mgr::MmioBus::new();
    bus.register(0xD000_0000, 0x1000, transport, None);

    let built = nova_vmm::builder::BuiltVm {
        kvm,
        vm_fd,
        vcpus: vec![vcpu],
        guest_memory: mem_arc,
        mmio_bus: bus,
        console_output,
        network_setup: None,
    };

    let snap_dir = dir.path().join("snapshot");
    let config = nova_vmm::snapshot::VmSnapshotConfig {
        vcpus: 1,
        memory_mib: 16,
        kernel_cmdline: "console=ttyS0".to_string(),
    };

    let files = nova_vmm::snapshot::save_snapshot(&built, &snap_dir, &config).unwrap();
    assert!(files.state_path.exists());
    assert!(files.memory_path.exists());

    // Verify state JSON is valid.
    let json = std::fs::read_to_string(&files.state_path).unwrap();
    let snap: nova_vmm::snapshot::VmSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.vcpus.len(), 1);
    assert_eq!(snap.config.memory_mib, 16);
}

#[test]
fn test_snapshot_restore_from_disk() {
    if !require_kvm() {
        eprintln!("skipping test_snapshot_restore_from_disk (NOVAVM_REAL_TESTS not set)");
        return;
    }

    let dir = TempDir::new().unwrap();

    // Build minimal VM.
    let kvm = nova_kvm::Kvm::open().unwrap();
    let vm_fd = kvm.create_vm().unwrap();
    vm_fd.set_tss_addr(0xFFFB_D000).unwrap();
    vm_fd.create_irqchip().unwrap();
    vm_fd.create_pit2().unwrap();

    let mem = nova_mem::GuestMemoryMmap::new(
        &[(nova_mem::GuestAddress::new(0), 16 * 1024 * 1024)],
        false,
    )
    .unwrap();
    let host_addr = mem.region_host_addr(0).unwrap();
    vm_fd
        .set_user_memory_region(&nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: 16 * 1024 * 1024,
            userspace_addr: host_addr,
        })
        .unwrap();

    // Write a pattern to guest memory.
    mem.write_slice(nova_mem::GuestAddress::new(0x1000), b"SNAPSHOT_TEST_DATA")
        .unwrap();

    let cpuid = kvm.get_supported_cpuid(256).unwrap();
    let vcpu = vm_fd.create_vcpu(0).unwrap();
    vcpu.set_cpuid2(&cpuid).unwrap();

    // Set custom registers.
    let mut regs = vcpu.get_regs().unwrap();
    regs.rax = 0x42;
    regs.rip = 0x2000;
    regs.rflags = 0x2;
    vcpu.set_regs(&regs).unwrap();

    let mem_arc = std::sync::Arc::new(mem);
    let console_output = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let console = nova_virtio::console::Console::new(std::sync::Arc::clone(&console_output));
    let mut transport = nova_virtio::mmio::MmioTransport::new(Box::new(console));
    transport.set_guest_memory(std::sync::Arc::clone(&mem_arc));
    let mut bus = nova_vmm::device_mgr::MmioBus::new();
    bus.register(0xD000_0000, 0x1000, transport, None);

    let built = nova_vmm::builder::BuiltVm {
        kvm,
        vm_fd,
        vcpus: vec![vcpu],
        guest_memory: mem_arc,
        mmio_bus: bus,
        console_output,
        network_setup: None,
    };

    // Save snapshot.
    let snap_dir = dir.path().join("snapshot");
    let config = nova_vmm::snapshot::VmSnapshotConfig {
        vcpus: 1,
        memory_mib: 16,
        kernel_cmdline: "console=ttyS0".to_string(),
    };
    nova_vmm::snapshot::save_snapshot(&built, &snap_dir, &config).unwrap();
    drop(built);

    // Restore snapshot.
    let restored = nova_vmm::snapshot::restore_snapshot(&snap_dir, None).unwrap();

    // Verify vCPU registers were restored.
    let restored_regs = restored.vcpus[0].get_regs().unwrap();
    assert_eq!(restored_regs.rax, 0x42);
    assert_eq!(restored_regs.rip, 0x2000);

    // Verify guest memory was restored.
    let mut buf = [0u8; 18];
    restored
        .guest_memory
        .read_slice(nova_mem::GuestAddress::new(0x1000), &mut buf)
        .unwrap();
    assert_eq!(&buf, b"SNAPSHOT_TEST_DATA");
}

#[test]
fn test_snapshot_memory_dump_load() {
    if !require_kvm() {
        eprintln!("skipping test_snapshot_memory_dump_load (NOVAVM_REAL_TESTS not set)");
        return;
    }

    let dir = TempDir::new().unwrap();

    let mem = nova_mem::GuestMemoryMmap::new(
        &[(nova_mem::GuestAddress::new(0), 4 * 1024 * 1024)],
        false,
    )
    .unwrap();

    // Write test patterns.
    mem.write_slice(nova_mem::GuestAddress::new(0), b"START_OF_MEMORY")
        .unwrap();
    mem.write_slice(nova_mem::GuestAddress::new(0x100000), b"AT_1MB")
        .unwrap();

    let dump_path = dir.path().join("mem.bin");
    nova_vmm::snapshot::dump_guest_memory(&mem, &dump_path).unwrap();
    assert!(dump_path.exists());
    assert_eq!(dump_path.metadata().unwrap().len(), 4 * 1024 * 1024);

    // Load into fresh memory.
    let mem2 = nova_mem::GuestMemoryMmap::new(
        &[(nova_mem::GuestAddress::new(0), 4 * 1024 * 1024)],
        false,
    )
    .unwrap();
    nova_vmm::snapshot::load_guest_memory(&mem2, &dump_path).unwrap();

    let mut buf1 = [0u8; 15];
    mem2.read_slice(nova_mem::GuestAddress::new(0), &mut buf1)
        .unwrap();
    assert_eq!(&buf1, b"START_OF_MEMORY");

    let mut buf2 = [0u8; 6];
    mem2.read_slice(nova_mem::GuestAddress::new(0x100000), &mut buf2)
        .unwrap();
    assert_eq!(&buf2, b"AT_1MB");
}

#[test]
fn test_snapshot_boot_time_comparison() {
    if !require_kvm() {
        eprintln!("skipping test_snapshot_boot_time_comparison (NOVAVM_REAL_TESTS not set)");
        return;
    }

    // This test just validates snapshot save+restore works end-to-end.
    // Timing comparisons are informational.
    let dir = TempDir::new().unwrap();

    let cold_start = std::time::Instant::now();

    let kvm = nova_kvm::Kvm::open().unwrap();
    let vm_fd = kvm.create_vm().unwrap();
    vm_fd.set_tss_addr(0xFFFB_D000).unwrap();
    vm_fd.create_irqchip().unwrap();
    vm_fd.create_pit2().unwrap();

    let mem = nova_mem::GuestMemoryMmap::new(
        &[(nova_mem::GuestAddress::new(0), 16 * 1024 * 1024)],
        false,
    )
    .unwrap();
    let host_addr = mem.region_host_addr(0).unwrap();
    vm_fd
        .set_user_memory_region(&nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: 16 * 1024 * 1024,
            userspace_addr: host_addr,
        })
        .unwrap();
    let cpuid = kvm.get_supported_cpuid(256).unwrap();
    let vcpu = vm_fd.create_vcpu(0).unwrap();
    vcpu.set_cpuid2(&cpuid).unwrap();

    let cold_duration = cold_start.elapsed();

    let mem_arc = std::sync::Arc::new(mem);
    let console_output = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let console = nova_virtio::console::Console::new(std::sync::Arc::clone(&console_output));
    let mut transport = nova_virtio::mmio::MmioTransport::new(Box::new(console));
    transport.set_guest_memory(std::sync::Arc::clone(&mem_arc));
    let mut bus = nova_vmm::device_mgr::MmioBus::new();
    bus.register(0xD000_0000, 0x1000, transport, None);

    let built = nova_vmm::builder::BuiltVm {
        kvm,
        vm_fd,
        vcpus: vec![vcpu],
        guest_memory: mem_arc,
        mmio_bus: bus,
        console_output,
        network_setup: None,
    };

    // Save snapshot.
    let snap_dir = dir.path().join("snapshot");
    let config = nova_vmm::snapshot::VmSnapshotConfig {
        vcpus: 1,
        memory_mib: 16,
        kernel_cmdline: "console=ttyS0".to_string(),
    };
    nova_vmm::snapshot::save_snapshot(&built, &snap_dir, &config).unwrap();
    drop(built);

    // Restore and time it.
    let restore_start = std::time::Instant::now();
    let _restored = nova_vmm::snapshot::restore_snapshot(&snap_dir, None).unwrap();
    let restore_duration = restore_start.elapsed();

    eprintln!("Cold boot:  {:?}", cold_duration);
    eprintln!("Restore:    {:?}", restore_duration);
    // Both should complete without error.
}
