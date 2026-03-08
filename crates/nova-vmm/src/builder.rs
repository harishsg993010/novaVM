//! VM builder: assembles all components into a running VM.

use std::collections::VecDeque;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use nova_boot::boot_params::E820Entry;
use nova_boot::{cpu_setup, layout, BootParams, BzImage, CmdlineBuilder, ElfKernel};
use nova_kvm::cap::KvmCap;
use nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion;
use nova_kvm::vcpu::VcpuFd;
use nova_kvm::vm::VmFd;
use nova_kvm::Kvm;
use nova_mem::{GuestAddress, GuestMemoryMmap};
use nova_virtio::console::Console;
use nova_virtio::mmio::MmioTransport;
use nova_virtio::net::Net;

use crate::config::VmConfig;
use crate::device_mgr::MmioBus;

/// MMIO device base address start.
const MMIO_BASE: u64 = 0xD000_0000;
/// MMIO region size per device.
const MMIO_SIZE: u64 = 0x1000;
/// IRQ start for MMIO devices.
const MMIO_IRQ_BASE: u32 = 5;

/// ELF magic bytes.
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// Detected kernel format.
enum KernelFormat {
    BzImage,
    Elf,
}

/// A fully built VM ready to run.
pub struct BuiltVm {
    pub kvm: Kvm,
    pub vm_fd: VmFd,
    pub vcpus: Vec<VcpuFd>,
    pub guest_memory: Arc<GuestMemoryMmap>,
    pub mmio_bus: MmioBus,
    pub console_output: Arc<Mutex<VecDeque<u8>>>,
    /// Network setup for cleanup on drop (if networking is configured).
    pub network_setup: Option<crate::network::NetworkSetup>,
}

/// Detect kernel format from the first 4 bytes.
fn detect_kernel_format(data: &[u8]) -> KernelFormat {
    if data.len() >= 4 && data[0..4] == ELF_MAGIC {
        KernelFormat::Elf
    } else {
        KernelFormat::BzImage
    }
}

/// Build a VM from the given configuration.
pub fn build_vm(config: &VmConfig) -> Result<BuiltVm> {
    // 1. Open KVM and check capabilities.
    let kvm = Kvm::open().context("failed to open /dev/kvm")?;
    kvm.require_capability(KvmCap::UserMemory)
        .context("KVM_CAP_USER_MEMORY required")?;
    kvm.require_capability(KvmCap::Irqchip)
        .context("KVM_CAP_IRQCHIP required")?;

    // 2. Create VM.
    let vm_fd = kvm.create_vm().context("failed to create VM")?;

    // Set TSS address (required for Intel VT-x).
    vm_fd
        .set_tss_addr(0xFFFB_D000)
        .context("failed to set TSS address")?;

    // Create in-kernel irqchip and PIT.
    vm_fd.create_irqchip().context("failed to create irqchip")?;
    vm_fd.create_pit2().context("failed to create PIT2")?;

    // 3. Set up guest memory.
    let mem_size = config.memory_bytes() as usize;
    let guest_memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false)
        .context("failed to allocate guest memory")?;

    // Register the memory region with KVM.
    let host_addr = guest_memory
        .region_host_addr(0)
        .context("no memory region")?;
    let region = KvmUserspaceMemoryRegion {
        slot: 0,
        flags: 0,
        guest_phys_addr: 0,
        memory_size: mem_size as u64,
        userspace_addr: host_addr,
    };
    vm_fd
        .set_user_memory_region(&region)
        .context("failed to set user memory region")?;

    // 4. Read kernel image and auto-detect format.
    let mut kernel_data = Vec::new();
    std::fs::File::open(&config.kernel.path)
        .context("failed to open kernel image")?
        .read_to_end(&mut kernel_data)
        .context("failed to read kernel image")?;

    let format = if config.kernel.boot_method == "elf" {
        KernelFormat::Elf
    } else if config.kernel.boot_method == "bzimage" {
        // Still auto-detect if the file is actually an ELF.
        detect_kernel_format(&kernel_data)
    } else {
        detect_kernel_format(&kernel_data)
    };

    let guest_memory_arc = Arc::new(guest_memory);

    // 5. Set up MMIO device bus.
    let mut mmio_bus = MmioBus::new();
    // Store VM fd for KVM_IRQ_LINE injection in poll_devices().
    mmio_bus.set_vm_fd(vm_fd.as_raw_fd());
    let mut next_mmio_addr = MMIO_BASE;
    let mut next_irq = MMIO_IRQ_BASE;

    // Console device.
    let console_output = Arc::new(Mutex::new(VecDeque::new()));
    let console = Console::new(Arc::clone(&console_output));
    let mut console_transport = MmioTransport::new(Box::new(console));
    console_transport.set_guest_memory(Arc::clone(&guest_memory_arc));
    mmio_bus.register(next_mmio_addr, MMIO_SIZE, console_transport, Some(next_irq));
    let console_mmio_addr = next_mmio_addr;
    let console_irq = next_irq;
    next_mmio_addr += MMIO_SIZE;
    next_irq += 1;

    // Net device (if configured).
    let mut net_mmio_info = None;
    if let Some(ref net_cfg) = config.network {
        let mac = if let Some(ref mac_str) = net_cfg.mac {
            parse_mac(mac_str).unwrap_or([0x52, 0x54, 0x00, 0x12, 0x34, 0x56])
        } else {
            [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]
        };
        let mut net_dev = Net::new(net_cfg.tap.clone(), mac);

        // Try to open the TAP device.
        match nova_virtio::tap::Tap::open(&net_cfg.tap) {
            Ok(tap) => {
                if let Err(e) = tap.set_nonblocking() {
                    tracing::warn!(error = %e, "failed to set TAP non-blocking");
                }
                net_dev.set_tap_fd(tap.fd());
                // Leak the Tap so the fd stays open (it will be closed when the VM shuts down).
                std::mem::forget(tap);
                tracing::info!(tap = %net_cfg.tap, "TAP device opened");
            }
            Err(e) => {
                tracing::warn!(error = %e, tap = %net_cfg.tap, "failed to open TAP device, net device will be stub-only");
            }
        }

        let mut net_transport = MmioTransport::new(Box::new(net_dev));
        net_transport.set_guest_memory(Arc::clone(&guest_memory_arc));
        mmio_bus.register(next_mmio_addr, MMIO_SIZE, net_transport, Some(next_irq));
        net_mmio_info = Some((next_mmio_addr, next_irq));
        let _ = next_mmio_addr + MMIO_SIZE; // Future devices would go here.
        let _ = next_irq + 1;
    }

    // 5b. Get host CPUID entries for vCPU setup.
    let cpuid_entries = kvm
        .get_supported_cpuid(256)
        .context("failed to get supported CPUID")?;

    // Collect MMIO device info for kernel command line.
    let mut mmio_device_params: Vec<(u64, u64, u32)> = Vec::new();
    mmio_device_params.push((console_mmio_addr, MMIO_SIZE, console_irq));
    if let Some((addr, irq)) = net_mmio_info {
        mmio_device_params.push((addr, MMIO_SIZE, irq));
    }

    // 6. Build and load kernel based on format.
    let mut vcpus = Vec::new();

    match format {
        KernelFormat::BzImage => {
            build_bzimage(
                config,
                &kernel_data,
                &guest_memory_arc,
                mem_size,
                &vm_fd,
                &cpuid_entries,
                &mut mmio_bus,
                &mmio_device_params,
                &mut vcpus,
            )?;
        }
        KernelFormat::Elf => {
            build_elf(
                config,
                &kernel_data,
                &guest_memory_arc,
                mem_size,
                &vm_fd,
                &cpuid_entries,
                &mut mmio_bus,
                &mmio_device_params,
                &mut vcpus,
            )?;
        }
    }

    tracing::info!(
        vcpus = config.vcpus,
        memory_mib = config.memory_mib,
        devices = mmio_bus.device_count(),
        "VM built successfully"
    );

    // Set up host networking if a net device was configured.
    let network_setup = if config.network.is_some() {
        let tap_name = config.network.as_ref().unwrap().tap.clone();
        let mut net_setup = crate::network::NetworkSetup::default_for_tap(&tap_name);
        if let Err(e) = net_setup.setup() {
            tracing::warn!(error = %e, "host network setup failed (requires root)");
        }
        Some(net_setup)
    } else {
        None
    };

    Ok(BuiltVm {
        kvm,
        vm_fd,
        vcpus,
        guest_memory: guest_memory_arc,
        mmio_bus,
        console_output,
        network_setup,
    })
}

/// Parse a MAC address string like "52:54:00:12:34:56" into bytes.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

/// Build VM with a bzImage kernel.
#[allow(clippy::too_many_arguments)]
fn build_bzimage(
    config: &VmConfig,
    kernel_data: &[u8],
    guest_memory: &GuestMemoryMmap,
    mem_size: usize,
    vm_fd: &VmFd,
    cpuid_entries: &[nova_kvm::kvm_bindings::KvmCpuidEntry2],
    _mmio_bus: &mut MmioBus,
    mmio_devices: &[(u64, u64, u32)],
    vcpus: &mut Vec<VcpuFd>,
) -> Result<()> {
    let mut bzimage =
        BzImage::parse(std::io::Cursor::new(kernel_data)).context("failed to parse bzImage")?;

    // E820 memory map.
    let e820_entries = [
        E820Entry {
            addr: 0,
            size: 0x9FC00,
            type_: layout::E820_RAM,
        },
        E820Entry {
            addr: 0x100000,
            size: (mem_size as u64).saturating_sub(0x100000),
            type_: layout::E820_RAM,
        },
    ];
    for (i, entry) in e820_entries.iter().enumerate() {
        bzimage.boot_params.set_e820_entry(i, *entry);
    }
    bzimage.boot_params.set_e820_count(e820_entries.len() as u8);

    // Kernel command line with all MMIO device params.
    let mut cmdline_builder = CmdlineBuilder::new().raw(&config.kernel.cmdline);
    for &(addr, size, irq) in mmio_devices {
        cmdline_builder = cmdline_builder.raw(&format!(
            "virtio_mmio.device=0x{:x}@0x{:x}:{}",
            size, addr, irq
        ));
    }

    cmdline_builder
        .write_to_memory(guest_memory, &mut bzimage.boot_params)
        .context("failed to write kernel command line")?;

    // Load initrd if specified.
    if let Some(ref initrd_path) = config.kernel.initrd {
        let mut initrd_file = std::fs::File::open(initrd_path).context("failed to open initrd")?;
        nova_boot::initrd::load_initrd(&mut initrd_file, guest_memory, &mut bzimage.boot_params)
            .context("failed to load initrd")?;
    }

    // Write kernel to guest memory.
    bzimage
        .load_into_memory(guest_memory)
        .context("failed to load kernel into guest memory")?;

    // Create vCPUs with proper segment attributes for bzImage boot.
    for i in 0..config.vcpus {
        let vcpu = vm_fd
            .create_vcpu(i as u64)
            .context(format!("failed to create vCPU {i}"))?;

        vcpu.set_cpuid2(cpuid_entries)
            .context(format!("failed to set CPUID for vCPU {i}"))?;

        let mut sregs = vcpu.get_sregs().context("failed to get sregs")?;
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        sregs.cs.limit = 0xFFFF_FFFF;
        sregs.cs.type_ = 11;
        sregs.cs.present = 1;
        sregs.cs.s = 1;
        sregs.cs.g = 1;
        sregs.cs.db = 1;
        // Set proper data segments.
        let data_seg = nova_kvm::kvm_bindings::KvmSegment {
            base: 0,
            limit: 0xFFFF_FFFF,
            selector: 0,
            type_: 3,
            present: 1,
            dpl: 0,
            db: 1,
            s: 1,
            l: 0,
            g: 1,
            avl: 0,
            unusable: 0,
            padding: 0,
        };
        sregs.ds = data_seg;
        sregs.es = data_seg;
        sregs.ss = data_seg;
        sregs.cr0 |= 1; // Protected mode.
        vcpu.set_sregs(&sregs).context("failed to set sregs")?;

        let mut regs = vcpu.get_regs().context("failed to get regs")?;
        regs.rip = layout::KERNEL_LOAD_ADDR;
        regs.rsi = layout::ZERO_PAGE_ADDR;
        regs.rsp = layout::BOOT_STACK_ADDR;
        regs.rflags = 0x2;
        vcpu.set_regs(&regs).context("failed to set regs")?;

        // Configure LAPIC LINT0=ExtINT, LINT1=NMI for timer interrupt delivery.
        vcpu.set_lint().context("failed to set LAPIC LINT")?;

        vcpus.push(vcpu);
    }

    Ok(())
}

/// Build VM with an ELF (vmlinux) kernel in 64-bit long mode.
#[allow(clippy::too_many_arguments)]
fn build_elf(
    config: &VmConfig,
    kernel_data: &[u8],
    guest_memory: &GuestMemoryMmap,
    mem_size: usize,
    vm_fd: &VmFd,
    cpuid_entries: &[nova_kvm::kvm_bindings::KvmCpuidEntry2],
    _mmio_bus: &mut MmioBus,
    mmio_devices: &[(u64, u64, u32)],
    vcpus: &mut Vec<VcpuFd>,
) -> Result<()> {
    // Parse and load ELF segments.
    let elf = ElfKernel::parse(std::io::Cursor::new(kernel_data)).context("failed to parse ELF")?;
    elf.load_into_memory(guest_memory)
        .context("failed to load ELF into guest memory")?;

    tracing::info!(
        entry = format!("{:#x}", elf.entry_point),
        segments = elf.segments.len(),
        "loaded ELF kernel"
    );

    // Set up 64-bit page tables and GDT in guest memory.
    cpu_setup::setup_long_mode_page_tables(guest_memory).context("failed to set up page tables")?;
    cpu_setup::setup_gdt(guest_memory).context("failed to set up GDT")?;

    // Build boot_params with E820 map.
    let mut boot_params = BootParams::new();
    let e820_entries = [
        E820Entry {
            addr: 0,
            size: 0x9FC00,
            type_: layout::E820_RAM,
        },
        E820Entry {
            addr: 0x100000,
            size: (mem_size as u64).saturating_sub(0x100000),
            type_: layout::E820_RAM,
        },
    ];
    for (i, entry) in e820_entries.iter().enumerate() {
        boot_params.set_e820_entry(i, *entry);
    }
    boot_params.set_e820_count(e820_entries.len() as u8);

    // Kernel command line with all MMIO device params.
    let mut cmdline_builder = CmdlineBuilder::new().raw(&config.kernel.cmdline);
    for &(addr, size, irq) in mmio_devices {
        cmdline_builder = cmdline_builder.raw(&format!(
            "virtio_mmio.device=0x{:x}@0x{:x}:{}",
            size, addr, irq
        ));
    }

    cmdline_builder
        .write_to_memory(guest_memory, &mut boot_params)
        .context("failed to write kernel command line")?;

    // Set boot protocol fields required by the kernel.
    {
        let header = boot_params.setup_header_mut();
        header.type_of_loader = 0xFF; // undefined boot loader
        header.loadflags |= 0x01;     // LOADED_HIGH: kernel loaded at 1 MiB+
    }

    // Load initrd if specified — place it above the kernel end to avoid collision.
    if let Some(ref initrd_path) = config.kernel.initrd {
        // Find the highest address used by the kernel.
        let kernel_end = elf.segments.iter()
            .map(|s| s.guest_addr + s.mem_size as u64)
            .max()
            .unwrap_or(0x200_0000); // 32 MiB fallback
        // Align initrd to 4 KiB page boundary, with 1 MiB padding.
        let initrd_addr = ((kernel_end + 0x10_0000) + 0xFFF) & !0xFFF;

        tracing::info!(
            kernel_end = format!("{:#x}", kernel_end),
            initrd_addr = format!("{:#x}", initrd_addr),
            "placing initrd above kernel"
        );

        let mut initrd_file = std::fs::File::open(initrd_path).context("failed to open initrd")?;
        nova_boot::initrd::load_initrd_at(&mut initrd_file, guest_memory, &mut boot_params, initrd_addr)
            .context("failed to load initrd")?;
    }

    // Write boot_params to zero page AFTER initrd is configured so ramdisk fields are set.
    guest_memory
        .write_slice(
            GuestAddress::new(layout::ZERO_PAGE_ADDR),
            boot_params.as_bytes(),
        )
        .context("failed to write boot params")?;

    // Create vCPUs with 64-bit long mode state.
    for i in 0..config.vcpus {
        let vcpu = vm_fd
            .create_vcpu(i as u64)
            .context(format!("failed to create vCPU {i}"))?;

        vcpu.set_cpuid2(cpuid_entries)
            .context(format!("failed to set CPUID for vCPU {i}"))?;

        let mut sregs = vcpu.get_sregs().context("failed to get sregs")?;
        cpu_setup::configure_64bit_sregs(&mut sregs);
        vcpu.set_sregs(&sregs).context("failed to set sregs")?;

        let mut regs = vcpu.get_regs().context("failed to get regs")?;
        cpu_setup::configure_64bit_regs(&mut regs, elf.entry_point);
        regs.rsi = layout::ZERO_PAGE_ADDR; // boot_params pointer
        vcpu.set_regs(&regs).context("failed to set regs")?;

        // Configure LAPIC LINT0=ExtINT, LINT1=NMI for timer interrupt delivery.
        vcpu.set_lint().context("failed to set LAPIC LINT")?;

        vcpus.push(vcpu);
    }

    Ok(())
}
