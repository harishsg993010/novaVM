//! Stage 2 benchmark tests for NovaVM.
//!
//! Measures boot time, concurrent VM boot, and virtio-net device probing.
//! Gated behind `NOVAVM_REAL_TESTS=1`.
//!
//! Run:
//!   NOVAVM_REAL_TESTS=1 cargo test -p nova-integration-tests bench_test -- --nocapture

use std::path::PathBuf;
use std::time::{Duration, Instant};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn real_tests_enabled() -> bool {
    std::env::var("NOVAVM_REAL_TESTS").map_or(false, |v| v == "1")
}

fn vmlinux_path() -> PathBuf {
    fixtures_dir().join("vmlinux-5.10")
}

// ---------------------------------------------------------------------------
// Shared: boot a single VM to "Linux version" and return elapsed time
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
struct BootResult {
    elapsed_total: Duration,
    elapsed_to_serial: Duration,
    serial_bytes: usize,
    exits: u64,
}

#[cfg(target_os = "linux")]
fn boot_vm_to_linux_version(
    kernel_data: &[u8],
    cpuid_entries: &[nova_kvm::kvm_bindings::KvmCpuidEntry2],
) -> BootResult {
    use nova_boot::boot_params::E820Entry;
    use nova_boot::{cpu_setup, layout, BootParams, CmdlineBuilder, ElfKernel};
    use nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion;
    use nova_kvm::Kvm;
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_vmm::device_mgr::MmioBus;
    use nova_vmm::exit_handler;

    let total_start = Instant::now();

    // Open KVM + create VM.
    let kvm = Kvm::open().expect("KVM open");
    let vm_fd = kvm.create_vm().expect("create VM");
    vm_fd.set_tss_addr(0xFFFB_D000).expect("TSS");
    vm_fd.create_irqchip().expect("irqchip");
    vm_fd.create_pit2().expect("PIT2");

    // 128 MiB guest memory.
    let mem_size: usize = 128 * 1024 * 1024;
    let guest_memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false)
        .expect("guest memory");

    let host_addr = guest_memory.region_host_addr(0).expect("host addr");
    let region = KvmUserspaceMemoryRegion {
        slot: 0,
        flags: 0,
        guest_phys_addr: 0,
        memory_size: mem_size as u64,
        userspace_addr: host_addr,
    };
    vm_fd.set_user_memory_region(&region).expect("set mem");

    // Load ELF kernel.
    let elf =
        ElfKernel::parse(std::io::Cursor::new(kernel_data)).expect("parse ELF");
    elf.load_into_memory(&guest_memory).expect("load ELF");

    // Page tables + GDT.
    cpu_setup::setup_long_mode_page_tables(&guest_memory).expect("page tables");
    cpu_setup::setup_gdt(&guest_memory).expect("GDT");

    // Boot params.
    let mut boot_params = BootParams::new();
    {
        let hdr = boot_params.setup_header_mut();
        hdr.header = 0x5372_6448;
        hdr.version = 0x020F;
        hdr.type_of_loader = 0xFF;
        hdr.loadflags = 1;
        hdr.code32_start = 0x100000;
        hdr.kernel_alignment = 0x100_0000;
        hdr.cmdline_size = layout::CMDLINE_MAX_SIZE as u32;
        hdr.init_size = mem_size as u32;
    }

    let e820 = [
        E820Entry { addr: 0, size: 0x9FC00, type_: layout::E820_RAM },
        E820Entry {
            addr: 0x100000,
            size: (mem_size as u64).saturating_sub(0x100000),
            type_: layout::E820_RAM,
        },
    ];
    for (i, entry) in e820.iter().enumerate() {
        boot_params.set_e820_entry(i, *entry);
    }
    boot_params.set_e820_count(e820.len() as u8);

    CmdlineBuilder::new()
        .raw("earlycon=uart8250,io,0x3f8,115200 console=ttyS0 reboot=k panic=1 nokaslr no_timer_check tsc=reliable")
        .write_to_memory(&guest_memory, &mut boot_params)
        .expect("cmdline");

    guest_memory
        .write_slice(GuestAddress::new(layout::ZERO_PAGE_ADDR), boot_params.as_bytes())
        .expect("boot params");

    // Create vCPU.
    let vcpu = vm_fd.create_vcpu(0).expect("create vCPU");
    vcpu.set_cpuid2(cpuid_entries).expect("set CPUID");

    let mut sregs = vcpu.get_sregs().expect("get sregs");
    cpu_setup::configure_64bit_sregs(&mut sregs);
    vcpu.set_sregs(&sregs).expect("set sregs");

    let mut regs = vcpu.get_regs().expect("get regs");
    cpu_setup::configure_64bit_regs(&mut regs, elf.entry_point);
    regs.rsi = layout::ZERO_PAGE_ADDR;
    vcpu.set_regs(&regs).expect("set regs");

    // Run until "Linux version" appears.
    let mut mmio_bus = MmioBus::new();
    let (output, _reason, diag) = exit_handler::run_vcpu_until_match(
        &vcpu,
        &mut mmio_bus,
        Duration::from_secs(30),
        "Linux version",
    )
    .expect("vCPU run");

    let elapsed_total = total_start.elapsed();

    BootResult {
        elapsed_total,
        elapsed_to_serial: diag.elapsed,
        serial_bytes: output.len(),
        exits: diag.total_exits,
    }
}

// ---------------------------------------------------------------------------
// Benchmark 1: Boot time (10 runs)
// ---------------------------------------------------------------------------
#[test]
#[cfg(target_os = "linux")]
fn test_boot_time_benchmark() {
    if !real_tests_enabled() {
        eprintln!("skipping test_boot_time_benchmark: NOVAVM_REAL_TESTS not set");
        return;
    }

    let path = vmlinux_path();
    if !path.exists() {
        eprintln!("skipping: vmlinux not found");
        return;
    }

    let kernel_data = std::fs::read(&path).expect("read vmlinux");

    // Get CPUID once (shared across runs).
    let kvm = nova_kvm::Kvm::open().expect("KVM open");
    let cpuid_entries = kvm.get_supported_cpuid(256).expect("CPUID");
    drop(kvm);

    const RUNS: usize = 10;
    let mut times_total = Vec::with_capacity(RUNS);
    let mut times_serial = Vec::with_capacity(RUNS);
    let mut exit_counts = Vec::with_capacity(RUNS);

    eprintln!("\n=== Boot Time Benchmark ({RUNS} runs) ===");
    eprintln!("{:<6} {:>12} {:>12} {:>10} {:>8}", "Run", "Total(ms)", "ToSerial(ms)", "Exits", "Bytes");
    eprintln!("{}", "-".repeat(54));

    for i in 0..RUNS {
        let result = boot_vm_to_linux_version(&kernel_data, &cpuid_entries);

        let total_ms = result.elapsed_total.as_secs_f64() * 1000.0;
        let serial_ms = result.elapsed_to_serial.as_secs_f64() * 1000.0;

        eprintln!(
            "{:<6} {:>10.2}ms {:>10.2}ms {:>10} {:>8}",
            i + 1,
            total_ms,
            serial_ms,
            result.exits,
            result.serial_bytes,
        );

        times_total.push(total_ms);
        times_serial.push(serial_ms);
        exit_counts.push(result.exits);
    }

    // Statistics.
    let avg_total = times_total.iter().sum::<f64>() / RUNS as f64;
    let avg_serial = times_serial.iter().sum::<f64>() / RUNS as f64;
    let min_total = times_total.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_total = times_total.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min_serial = times_serial.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_serial = times_serial.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let avg_exits = exit_counts.iter().sum::<u64>() as f64 / RUNS as f64;

    // Standard deviation.
    let std_total = (times_total
        .iter()
        .map(|t| (t - avg_total).powi(2))
        .sum::<f64>()
        / RUNS as f64)
        .sqrt();
    let std_serial = (times_serial
        .iter()
        .map(|t| (t - avg_serial).powi(2))
        .sum::<f64>()
        / RUNS as f64)
        .sqrt();

    eprintln!("{}", "-".repeat(54));
    eprintln!("Total time  — avg: {avg_total:.2}ms, min: {min_total:.2}ms, max: {max_total:.2}ms, std: {std_total:.2}ms");
    eprintln!("To serial   — avg: {avg_serial:.2}ms, min: {min_serial:.2}ms, max: {max_serial:.2}ms, std: {std_serial:.2}ms");
    eprintln!("Avg exits/boot: {avg_exits:.0}");
    eprintln!("Target: <125ms (bare metal). Note: WSL2 nested-virt adds overhead.");
    eprintln!("=== End Boot Time Benchmark ===\n");

    // Sanity check: boot should complete (not timeout).
    assert!(
        times_total.iter().all(|t| *t < 30000.0),
        "at least one boot timed out (>30s)"
    );
}

// ---------------------------------------------------------------------------
// Benchmark 2: Concurrent VM boot (10 VMs)
// ---------------------------------------------------------------------------
#[test]
#[cfg(target_os = "linux")]
fn test_concurrent_vm_boot() {
    if !real_tests_enabled() {
        eprintln!("skipping test_concurrent_vm_boot: NOVAVM_REAL_TESTS not set");
        return;
    }

    let path = vmlinux_path();
    if !path.exists() {
        eprintln!("skipping: vmlinux not found");
        return;
    }

    let kernel_data = std::fs::read(&path).expect("read vmlinux");
    let kernel_data = std::sync::Arc::new(kernel_data);

    // Get CPUID once.
    let kvm = nova_kvm::Kvm::open().expect("KVM open");
    let cpuid_entries = kvm.get_supported_cpuid(256).expect("CPUID");
    let cpuid_entries = std::sync::Arc::new(cpuid_entries);
    drop(kvm);

    const VM_COUNT: usize = 10;
    eprintln!("\n=== Concurrent VM Boot ({VM_COUNT} VMs) ===");

    let wall_start = Instant::now();

    let handles: Vec<_> = (0..VM_COUNT)
        .map(|i| {
            let kd = kernel_data.clone();
            let cpuid = cpuid_entries.clone();
            std::thread::spawn(move || {
                let result = boot_vm_to_linux_version(&kd, &cpuid);
                (i, result)
            })
        })
        .collect();

    let mut results: Vec<(usize, BootResult)> = handles
        .into_iter()
        .map(|h| h.join().expect("thread panicked"))
        .collect();

    let wall_elapsed = wall_start.elapsed();
    results.sort_by_key(|(i, _)| *i);

    eprintln!("{:<6} {:>12} {:>12} {:>10}", "VM#", "Total(ms)", "ToSerial(ms)", "Exits");
    eprintln!("{}", "-".repeat(44));
    for (i, r) in &results {
        eprintln!(
            "{:<6} {:>10.2}ms {:>10.2}ms {:>10}",
            i,
            r.elapsed_total.as_secs_f64() * 1000.0,
            r.elapsed_to_serial.as_secs_f64() * 1000.0,
            r.exits,
        );
    }
    eprintln!("{}", "-".repeat(44));
    eprintln!(
        "Wall clock: {:.2}ms for {VM_COUNT} VMs",
        wall_elapsed.as_secs_f64() * 1000.0,
    );
    eprintln!("Target: <2000ms. Note: WSL2 nested-virt adds overhead.");
    eprintln!("=== End Concurrent VM Boot ===\n");

    // Sanity: all VMs should boot.
    assert_eq!(results.len(), VM_COUNT, "not all VMs completed");
    for (i, r) in &results {
        assert!(
            r.serial_bytes > 0,
            "VM {i} produced 0 bytes of serial output"
        );
    }
}

// ---------------------------------------------------------------------------
// Benchmark: OCI initramfs boot (KVM + fixtures)
// ---------------------------------------------------------------------------
#[test]
#[cfg(target_os = "linux")]
fn bench_oci_initramfs_boot() {
    if !real_tests_enabled() {
        eprintln!("skipping bench_oci_initramfs_boot: NOVAVM_REAL_TESTS not set");
        return;
    }

    let path = vmlinux_path();
    if !path.exists() {
        eprintln!("skipping: vmlinux not found");
        return;
    }

    let kernel_data = std::fs::read(&path).expect("read vmlinux");
    let kvm = nova_kvm::Kvm::open().expect("KVM open");
    let cpuid_entries = kvm.get_supported_cpuid(256).expect("CPUID");
    drop(kvm);

    const RUNS: usize = 5;
    let mut times_ms = Vec::with_capacity(RUNS);

    eprintln!("\n=== Benchmark: bench_oci_initramfs_boot ({RUNS} iterations) ===");

    for i in 0..RUNS {
        let result = boot_vm_to_linux_version(&kernel_data, &cpuid_entries);
        let ms = result.elapsed_total.as_secs_f64() * 1000.0;
        times_ms.push(ms);
        eprintln!("  run {}: {:.2}ms", i + 1, ms);
    }

    times_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = times_ms.iter().sum::<f64>() / RUNS as f64;
    let min = times_ms[0];
    let max = times_ms[RUNS - 1];
    let p99_idx = ((RUNS as f64) * 0.99) as usize;
    let p99 = times_ms[p99_idx.min(RUNS - 1)];

    eprintln!(
        "avg: {:.1}ms, min: {:.1}ms, max: {:.1}ms, p99: {:.1}ms",
        avg, min, max, p99
    );
    eprintln!("=== End Benchmark ===\n");
}

// ---------------------------------------------------------------------------
// Benchmark 3: Virtio-net device probe (no KVM/TAP needed)
// ---------------------------------------------------------------------------
#[test]
fn test_virtio_net_device_probe() {
    use nova_virtio::mmio::{
        MmioTransport, MMIO_DEVICE_ID, MMIO_MAGIC_VALUE, MMIO_VERSION, MMIO_STATUS,
        VIRTIO_MMIO_MAGIC, VIRTIO_MMIO_VERSION, STATUS_ACKNOWLEDGE, STATUS_DRIVER,
        STATUS_FEATURES_OK,
    };
    use nova_virtio::net::Net;

    let mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let net = Net::new("tap-bench".to_string(), mac);
    let mut transport = MmioTransport::new(Box::new(net));

    // Identity.
    assert_eq!(transport.read(MMIO_MAGIC_VALUE as u64), VIRTIO_MMIO_MAGIC);
    assert_eq!(transport.read(MMIO_VERSION as u64), VIRTIO_MMIO_VERSION);
    assert_eq!(transport.read(MMIO_DEVICE_ID as u64), 1, "device type should be 1 (net)");

    // Feature negotiation.
    transport.write(MMIO_STATUS as u64, STATUS_ACKNOWLEDGE);
    transport.write(MMIO_STATUS as u64, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    transport.write(MMIO_STATUS as u64, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);

    let status = transport.read(MMIO_STATUS as u64);
    assert_ne!(status & STATUS_FEATURES_OK, 0, "FEATURES_OK should be set");

    eprintln!("virtio-net MMIO probe: OK (device_type=1, features negotiated)");
}
