//! VM snapshot save/restore (L3).
//!
//! Saves the full VM state (vCPU registers, clock, IRQ chip, PIT, guest memory)
//! to disk and restores it for near-instant boot (<30ms).

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use nova_kvm::kvm_bindings::{
    KvmClockData, KvmMsrEntry, KvmPitState2, KvmRegs, KvmSregs, KvmUserspaceMemoryRegion,
    KvmXcr, SNAPSHOT_MSRS,
};
use nova_kvm::Kvm;
use nova_mem::{GuestAddress, GuestMemoryMmap};
use nova_virtio::console::Console;
use nova_virtio::mmio::MmioTransport;
use nova_virtio::net::Net;

use crate::builder::BuiltVm;
use crate::device_mgr::MmioBus;

/// MMIO device base address start (must match builder.rs).
const MMIO_BASE: u64 = 0xD000_0000;
const MMIO_SIZE: u64 = 0x1000;

/// Files written by save_snapshot.
pub struct SnapshotFiles {
    pub state_path: PathBuf,
    pub memory_path: PathBuf,
}

/// Saved vCPU state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcpuSnapshot {
    pub regs: KvmRegs,
    pub sregs: KvmSregs,
    /// MSR values (TSC, SYSENTER, STAR/LSTAR/CSTAR, etc.)
    #[serde(default)]
    pub msrs: Vec<KvmMsrEntry>,
    /// XSAVE area (FPU + SSE + AVX state), 4096 bytes as u32 array.
    #[serde(default)]
    pub xsave: Vec<u32>,
    /// Extended control registers (XCR0, etc.)
    #[serde(default)]
    pub xcrs: Vec<KvmXcr>,
}

/// Serialized IRQ chip state (byte arrays for PIC master/slave/IOAPIC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrqchipSnapshot {
    /// Raw bytes from KVM_GET_IRQCHIP chip_id=0 (PIC master).
    pub pic_master: Vec<u8>,
    /// Raw bytes from KVM_GET_IRQCHIP chip_id=1 (PIC slave).
    pub pic_slave: Vec<u8>,
    /// Raw bytes from KVM_GET_IRQCHIP chip_id=2 (IOAPIC).
    pub ioapic: Vec<u8>,
}

/// VM configuration stored alongside the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSnapshotConfig {
    pub vcpus: u32,
    pub memory_mib: u32,
    pub kernel_cmdline: String,
}

/// Saved virtio queue state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioQueueState {
    pub max_size: u16,
    pub size: u16,
    pub ready: bool,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub next_avail: u16,
    pub next_used: u16,
}

/// Saved virtio device state (queues after activation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioDeviceState {
    pub device_type: u32,
    pub queues: Vec<VirtioQueueState>,
}

/// Full serializable VM snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSnapshot {
    /// Snapshot format version.
    pub version: u32,
    /// Per-vCPU register state.
    pub vcpus: Vec<VcpuSnapshot>,
    /// VM clock.
    pub clock: KvmClockData,
    /// IRQ chip state.
    pub irqchip: IrqchipSnapshot,
    /// PIT state.
    pub pit: KvmPitState2,
    /// Guest memory region layout: (guest_phys_addr, size).
    pub memory_regions: Vec<(u64, u64)>,
    /// VM configuration.
    pub config: VmSnapshotConfig,
    /// Virtio device states (queue addresses, etc.) for snapshot restore.
    #[serde(default)]
    pub virtio_devices: Vec<VirtioDeviceState>,
}

/// Current snapshot format version.
pub const SNAPSHOT_VERSION: u32 = 3;

/// Save a full VM snapshot to disk.
pub fn save_snapshot(vm: &BuiltVm, output_dir: &Path, config: &VmSnapshotConfig) -> Result<SnapshotFiles> {
    fs::create_dir_all(output_dir)?;

    // 1. Save vCPU regs/sregs/msrs/xsave/xcrs.
    let mut vcpu_snapshots = Vec::new();
    for (i, vcpu) in vm.vcpus.iter().enumerate() {
        let regs = vcpu.get_regs().context(format!("get regs for vCPU {i}"))?;
        let sregs = vcpu.get_sregs().context(format!("get sregs for vCPU {i}"))?;
        let msrs = vcpu.get_msrs(SNAPSHOT_MSRS)
            .context(format!("get MSRs for vCPU {i}"))?;

        // XSAVE and XCRS are optional — some KVM versions don't support them.
        let xsave = match vcpu.get_xsave() {
            Ok(state) => state.region.to_vec(),
            Err(e) => {
                tracing::debug!(vcpu = i, error = %e, "XSAVE not available, skipping");
                Vec::new()
            }
        };
        let xcrs = match vcpu.get_xcrs() {
            Ok(state) => (0..state.nr_xcrs as usize)
                .map(|j| state.xcrs[j])
                .collect(),
            Err(e) => {
                tracing::debug!(vcpu = i, error = %e, "XCRS not available, skipping");
                Vec::new()
            }
        };

        vcpu_snapshots.push(VcpuSnapshot {
            regs,
            sregs,
            msrs,
            xsave,
            xcrs,
        });
    }

    // 2. Save VM clock.
    let clock = vm.vm_fd.get_clock().context("get VM clock")?;

    // 3. Save IRQ chip state (PIC master=0, PIC slave=1, IOAPIC=2).
    let pic_master = vm.vm_fd.get_irqchip(0).context("get PIC master")?;
    let pic_slave = vm.vm_fd.get_irqchip(1).context("get PIC slave")?;
    let ioapic = vm.vm_fd.get_irqchip(2).context("get IOAPIC")?;

    let irqchip = IrqchipSnapshot {
        pic_master: pic_master.chip.to_vec(),
        pic_slave: pic_slave.chip.to_vec(),
        ioapic: ioapic.chip.to_vec(),
    };

    // 4. Save PIT state.
    let pit = vm.vm_fd.get_pit2().context("get PIT state")?;

    // 5. Dump guest memory regions.
    let mut memory_regions = Vec::new();
    for i in 0..vm.guest_memory.num_regions() {
        if let Some((base, size)) = vm.guest_memory.region_info(i) {
            memory_regions.push((base.raw(), size as u64));
        }
    }

    let memory_path = output_dir.join("vm_memory.bin");
    dump_guest_memory(&vm.guest_memory, &memory_path)?;

    // 6. Save virtio device queue states.
    let virtio_devices: Vec<VirtioDeviceState> = vm
        .mmio_bus
        .snapshot_virtio_queues()
        .into_iter()
        .map(|(device_type, qs)| VirtioDeviceState {
            device_type,
            queues: qs
                .into_iter()
                .map(|(max_size, size, ready, desc, avail, used, next_avail, next_used)| {
                    VirtioQueueState {
                        max_size,
                        size,
                        ready,
                        desc_table: desc,
                        avail_ring: avail,
                        used_ring: used,
                        next_avail,
                        next_used,
                    }
                })
                .collect(),
        })
        .collect();
    tracing::info!(count = virtio_devices.len(), "saved virtio device states");

    // 7. Serialize VmSnapshot to JSON.
    let snapshot = VmSnapshot {
        version: SNAPSHOT_VERSION,
        vcpus: vcpu_snapshots,
        clock,
        irqchip,
        pit,
        memory_regions,
        config: config.clone(),
        virtio_devices,
    };

    let state_path = output_dir.join("vm_state.json");
    let json = serde_json::to_string_pretty(&snapshot)
        .context("serialize VM snapshot")?;
    fs::write(&state_path, json)?;

    tracing::info!(
        dir = %output_dir.display(),
        vcpus = snapshot.vcpus.len(),
        memory_regions = snapshot.memory_regions.len(),
        "VM snapshot saved"
    );

    Ok(SnapshotFiles {
        state_path,
        memory_path,
    })
}

/// Restore a VM from a snapshot directory.
///
/// If `tap_name` is provided, the virtio-net device and host networking are
/// recreated so the restored VM has full network connectivity.
pub fn restore_snapshot(snapshot_dir: &Path, tap_name: Option<&str>) -> Result<BuiltVm> {
    // 1. Read and deserialize VmSnapshot.
    let state_path = snapshot_dir.join("vm_state.json");
    let json = fs::read_to_string(&state_path).context("read snapshot state")?;
    let snapshot: VmSnapshot =
        serde_json::from_str(&json).context("deserialize snapshot")?;

    if snapshot.version > SNAPSHOT_VERSION {
        anyhow::bail!(
            "snapshot version too new: max supported {}, got {}",
            SNAPSHOT_VERSION,
            snapshot.version
        );
    }

    // 2. Open KVM and create VM.
    let kvm = Kvm::open().context("open /dev/kvm")?;
    let vm_fd = kvm.create_vm().context("create VM for restore")?;

    vm_fd.set_tss_addr(0xFFFB_D000).context("set TSS addr")?;
    vm_fd.create_irqchip().context("create IRQ chip")?;
    vm_fd.create_pit2().context("create PIT2")?;

    // 3. Demand-page guest memory from snapshot file (MAP_PRIVATE).
    //
    // Instead of allocating anonymous memory and reading the entire dump
    // upfront (~500ms for 256MB), we mmap the snapshot file with MAP_PRIVATE.
    // Pages fault in lazily as the guest accesses them — only the pages
    // actually touched trigger disk I/O. Writes go to private COW copies,
    // so the snapshot file is never modified.
    let memory_path = snapshot_dir.join("vm_memory.bin");
    let memory_file = fs::File::open(&memory_path)
        .context("open memory dump for demand paging")?;
    let memory_fd = memory_file.as_raw_fd();

    // Build (guest_addr, size, file_offset) tuples. Regions are stored
    // contiguously in the dump file, so offsets accumulate.
    let mut file_regions = Vec::new();
    let mut file_offset: i64 = 0;
    for &(addr, size) in &snapshot.memory_regions {
        file_regions.push((GuestAddress::new(addr), size as usize, file_offset));
        file_offset += size as i64;
    }

    let guest_memory = GuestMemoryMmap::from_file(memory_fd, &file_regions)
        .context("mmap snapshot memory (demand-paged)")?;

    // fd can be closed now — kernel keeps its own file reference for the mapping.
    drop(memory_file);

    // 4. Register memory regions with KVM.
    for (i, &(addr, size)) in snapshot.memory_regions.iter().enumerate() {
        let host_addr = guest_memory
            .region_host_addr(i)
            .context("get region host addr")?;
        let region = KvmUserspaceMemoryRegion {
            slot: i as u32,
            flags: 0,
            guest_phys_addr: addr,
            memory_size: size,
            userspace_addr: host_addr,
        };
        vm_fd
            .set_user_memory_region(&region)
            .context("set memory region")?;
    }

    let guest_memory_arc = Arc::new(guest_memory);

    // 5. Restore IRQ chips (before vCPUs, per KVM requirements).
    let mut pic_master = nova_kvm::kvm_bindings::KvmIrqchip::default();
    pic_master.chip_id = 0;
    pic_master.chip[..snapshot.irqchip.pic_master.len()]
        .copy_from_slice(&snapshot.irqchip.pic_master);
    vm_fd.set_irqchip(&pic_master).context("restore PIC master")?;

    let mut pic_slave = nova_kvm::kvm_bindings::KvmIrqchip::default();
    pic_slave.chip_id = 1;
    pic_slave.chip[..snapshot.irqchip.pic_slave.len()]
        .copy_from_slice(&snapshot.irqchip.pic_slave);
    vm_fd.set_irqchip(&pic_slave).context("restore PIC slave")?;

    let mut ioapic = nova_kvm::kvm_bindings::KvmIrqchip::default();
    ioapic.chip_id = 2;
    ioapic.chip[..snapshot.irqchip.ioapic.len()]
        .copy_from_slice(&snapshot.irqchip.ioapic);
    vm_fd.set_irqchip(&ioapic).context("restore IOAPIC")?;

    vm_fd.set_pit2(&snapshot.pit).context("restore PIT")?;

    // 6. Create and restore vCPUs.
    let cpuid_entries = kvm
        .get_supported_cpuid(256)
        .context("get supported CPUID")?;

    let mut vcpus = Vec::new();
    for (i, vcpu_snap) in snapshot.vcpus.iter().enumerate() {
        let vcpu = vm_fd
            .create_vcpu(i as u64)
            .context(format!("create vCPU {i}"))?;

        vcpu.set_cpuid2(&cpuid_entries)
            .context(format!("set CPUID for vCPU {i}"))?;

        vcpu.set_sregs(&vcpu_snap.sregs)
            .context(format!("restore sregs for vCPU {i}"))?;

        // Restore XSAVE (FPU + SSE + AVX) BEFORE regs — xsave includes
        // FPU control state that affects instruction execution.
        if vcpu_snap.xsave.len() == 1024 {
            let mut xsave = nova_kvm::kvm_bindings::KvmXsave::default();
            xsave.region.copy_from_slice(&vcpu_snap.xsave);
            vcpu.set_xsave(&xsave)
                .context(format!("restore XSAVE for vCPU {i}"))?;
        }

        // Restore XCRS (XCR0 etc.) — non-fatal if unsupported.
        if !vcpu_snap.xcrs.is_empty() {
            let mut xcrs = nova_kvm::kvm_bindings::KvmXcrs::default();
            xcrs.nr_xcrs = vcpu_snap.xcrs.len() as u32;
            for (j, xcr) in vcpu_snap.xcrs.iter().enumerate() {
                xcrs.xcrs[j] = *xcr;
            }
            if let Err(e) = vcpu.set_xcrs(&xcrs) {
                tracing::debug!(vcpu = i, error = %e, "XCRS restore failed, skipping");
            }
        }

        // Restore MSRs (TSC, SYSENTER, STAR/LSTAR/CSTAR, etc.)
        if !vcpu_snap.msrs.is_empty() {
            vcpu.set_msrs(&vcpu_snap.msrs)
                .context(format!("restore MSRs for vCPU {i}"))?;
        }

        vcpu.set_regs(&vcpu_snap.regs)
            .context(format!("restore regs for vCPU {i}"))?;

        vcpus.push(vcpu);
    }

    // 7. Restore VM clock AFTER vCPUs are created (Firecracker pattern).
    // KVM needs vCPUs to exist to properly compute TSC offsets.
    // Clear flags and pad to avoid KVM misinterpreting extended fields
    // (e.g., KVM_CLOCK_REALTIME in flags would make KVM read pad[1:2] as
    // realtime nanoseconds, but those contain garbage from GET_CLOCK).
    let mut clock = snapshot.clock.clone();
    clock.flags = 0;
    clock.pad = [0; 9];
    vm_fd.set_clock(&clock).context("restore clock")?;

    // 8. Set up MMIO bus + console device.
    let vm_raw_fd = vm_fd.as_raw_fd();
    let mut mmio_bus = MmioBus::new();
    mmio_bus.set_vm_fd(vm_raw_fd);
    let console_output = Arc::new(Mutex::new(VecDeque::new()));
    let console = Console::new(Arc::clone(&console_output));
    let mut console_transport = MmioTransport::new(Box::new(console));
    console_transport.set_guest_memory(Arc::clone(&guest_memory_arc));
    mmio_bus.register(MMIO_BASE, MMIO_SIZE, console_transport, Some(5));

    // 9. Register virtio-net device on MMIO bus (without TAP fd).
    //    The caller is responsible for opening the TAP via mmio_bus.open_tap_for_net()
    //    and setting up host networking, since TAP is exclusive (one fd at a time).
    if tap_name.is_some() {
        let mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01];
        let tap = tap_name.unwrap();
        let net_dev = Net::new(tap.to_string(), mac);
        let net_mmio_addr = MMIO_BASE + MMIO_SIZE; // 0xD0001000
        let net_irq = 6u32;
        let mut net_transport = MmioTransport::new(Box::new(net_dev));
        net_transport.set_guest_memory(Arc::clone(&guest_memory_arc));
        mmio_bus.register(net_mmio_addr, MMIO_SIZE, net_transport, Some(net_irq));
    }

    // 10. Restore virtio device queue states (force-activate).
    //     The guest already negotiated the device before snapshot, so we
    //     reconstruct the queues from saved addresses and activate directly.
    for dev_state in &snapshot.virtio_devices {
        let queues: Vec<nova_virtio::queue::Queue> = dev_state
            .queues
            .iter()
            .map(|qs| {
                nova_virtio::queue::Queue::from_saved(
                    qs.max_size,
                    qs.size,
                    qs.ready,
                    GuestAddress::new(qs.desc_table),
                    GuestAddress::new(qs.avail_ring),
                    GuestAddress::new(qs.used_ring),
                    qs.next_avail,
                    qs.next_used,
                )
            })
            .collect();
        tracing::info!(
            device_type = dev_state.device_type,
            num_queues = queues.len(),
            "restoring virtio device queue state"
        );
        mmio_bus.force_activate_device(dev_state.device_type, queues);
    }

    tracing::info!(
        vcpus = vcpus.len(),
        memory_regions = snapshot.memory_regions.len(),
        has_network = tap_name.is_some(),
        virtio_devices = snapshot.virtio_devices.len(),
        "VM restored from snapshot"
    );

    Ok(BuiltVm {
        kvm,
        vm_fd,
        vcpus,
        guest_memory: guest_memory_arc,
        mmio_bus,
        console_output,
        network_setup: None,
    })
}

/// Dump guest memory regions to a file.
pub fn dump_guest_memory(mem: &GuestMemoryMmap, path: &Path) -> Result<()> {
    let mut file = fs::File::create(path).context("create memory dump file")?;

    for (base, size, _ptr) in mem.iter_regions() {
        let mut buf = vec![0u8; size];
        mem.read_slice(base, &mut buf)
            .map_err(|e| anyhow::anyhow!("read region at {:#x}: {e}", base.raw()))?;
        file.write_all(&buf)?;
    }

    file.flush()?;
    Ok(())
}

/// Load guest memory from a dump file.
pub fn load_guest_memory(mem: &GuestMemoryMmap, path: &Path) -> Result<()> {
    let mut file = fs::File::open(path).context("open memory dump file")?;

    for (base, size, _ptr) in mem.iter_regions() {
        let mut buf = vec![0u8; size];
        file.read_exact(&mut buf)
            .context(format!("read region at {:#x}", base.raw()))?;
        mem.write_slice(base, &buf)
            .map_err(|e| anyhow::anyhow!("write region at {:#x}: {e}", base.raw()))?;
    }

    Ok(())
}
