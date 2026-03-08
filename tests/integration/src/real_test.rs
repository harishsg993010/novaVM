//! Real-world integration tests for NovaVM.
//!
//! These tests require actual artifacts (vmlinux kernel, KVM access) and are
//! gated behind the `NOVAVM_REAL_TESTS=1` environment variable.
//!
//! Run on Linux with KVM:
//!   NOVAVM_REAL_TESTS=1 cargo test -p nova-integration-tests real_test

use std::path::PathBuf;

/// Path to the test fixtures directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

/// Check whether real tests are enabled.
fn real_tests_enabled() -> bool {
    std::env::var("NOVAVM_REAL_TESTS").map_or(false, |v| v == "1")
}

/// Path to the downloaded vmlinux kernel.
fn vmlinux_path() -> PathBuf {
    fixtures_dir().join("vmlinux-5.10")
}

// ---------------------------------------------------------------------------
// Test 1: ELF kernel validation (no KVM needed)
// ---------------------------------------------------------------------------
#[test]
fn test_elf_kernel_validation() {
    if !real_tests_enabled() {
        eprintln!("skipping test_elf_kernel_validation: NOVAVM_REAL_TESTS not set");
        return;
    }

    let path = vmlinux_path();
    if !path.exists() {
        eprintln!(
            "skipping test_elf_kernel_validation: {} not found (run tests/fixtures/download.sh)",
            path.display()
        );
        return;
    }

    let file = std::fs::File::open(&path).expect("failed to open vmlinux");
    let elf = nova_boot::ElfKernel::parse(file).expect("failed to parse vmlinux as ELF");

    // Verify ELF basics.
    assert!(
        elf.entry_point > 0,
        "ELF entry point should be non-zero, got {:#x}",
        elf.entry_point
    );
    assert!(
        !elf.segments.is_empty(),
        "ELF should have at least one PT_LOAD segment"
    );

    // vmlinux typically loads at or above 1 MiB.
    for seg in &elf.segments {
        assert!(
            seg.file_size > 0 || seg.mem_size > 0,
            "PT_LOAD segment should have non-zero size"
        );
    }

    eprintln!(
        "vmlinux: entry={:#x}, segments={}, pvh={:?}",
        elf.entry_point,
        elf.segments.len(),
        elf.pvh_entry
    );
}

// ---------------------------------------------------------------------------
// Test 2: Real kernel boot (Stage 2.1 — THE critical test)
// ---------------------------------------------------------------------------
#[test]
#[cfg(target_os = "linux")]
fn test_real_kernel_boot() {
    if !real_tests_enabled() {
        eprintln!("skipping test_real_kernel_boot: NOVAVM_REAL_TESTS not set");
        return;
    }

    let path = vmlinux_path();
    if !path.exists() {
        eprintln!(
            "skipping test_real_kernel_boot: {} not found (run tests/fixtures/download.sh)",
            path.display()
        );
        return;
    }

    use nova_boot::boot_params::E820Entry;
    use nova_boot::{cpu_setup, layout, BootParams, CmdlineBuilder, ElfKernel};
    use nova_kvm::cap::KvmCap;
    use nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion;
    use nova_kvm::Kvm;
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_vmm::device_mgr::MmioBus;
    use nova_vmm::exit_handler;
    use std::time::Duration;

    // 1. Open KVM.
    let kvm = Kvm::open().expect("failed to open /dev/kvm");
    kvm.require_capability(KvmCap::UserMemory)
        .expect("KVM_CAP_USER_MEMORY required");

    // 2. Create VM with irqchip and PIT.
    let vm_fd = kvm.create_vm().expect("failed to create VM");
    vm_fd.set_tss_addr(0xFFFB_D000).expect("failed to set TSS");
    vm_fd.create_irqchip().expect("failed to create irqchip");
    vm_fd.create_pit2().expect("failed to create PIT2");

    // 3. Allocate 128 MiB guest memory.
    let mem_size: usize = 128 * 1024 * 1024;
    let guest_memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false)
        .expect("failed to allocate guest memory");

    let host_addr = guest_memory.region_host_addr(0).expect("no memory region");
    let region = KvmUserspaceMemoryRegion {
        slot: 0,
        flags: 0,
        guest_phys_addr: 0,
        memory_size: mem_size as u64,
        userspace_addr: host_addr,
    };
    vm_fd
        .set_user_memory_region(&region)
        .expect("failed to set memory region");

    // 4. Load vmlinux ELF.
    let file = std::fs::File::open(&path).expect("failed to open vmlinux");
    let elf = ElfKernel::parse(file).expect("failed to parse ELF");
    elf.load_into_memory(&guest_memory)
        .expect("failed to load ELF into guest memory");

    // 5. Set up 64-bit page tables and GDT.
    cpu_setup::setup_long_mode_page_tables(&guest_memory).expect("failed to set up page tables");
    cpu_setup::setup_gdt(&guest_memory).expect("failed to set up GDT");

    // 6. Build boot_params with E820 and command line.
    let mut boot_params = BootParams::new();

    // Set critical setup header fields for 64-bit direct boot.
    {
        let hdr = boot_params.setup_header_mut();
        hdr.header = 0x5372_6448; // "HdrS" magic
        hdr.version = 0x020F; // Boot protocol 2.15
        hdr.type_of_loader = 0xFF; // Unknown bootloader
        hdr.loadflags = 1; // LOADED_HIGH
        hdr.code32_start = 0x100000; // Default protected-mode entry
        hdr.kernel_alignment = 0x100_0000; // 16 MiB alignment
        hdr.cmdline_size = layout::CMDLINE_MAX_SIZE as u32;
        hdr.init_size = mem_size as u32;
    }

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

    CmdlineBuilder::new()
        .raw("earlycon=uart8250,io,0x3f8,115200 console=ttyS0 reboot=k panic=1 nokaslr no_timer_check tsc=reliable")
        .write_to_memory(&guest_memory, &mut boot_params)
        .expect("failed to write cmdline");

    guest_memory
        .write_slice(
            GuestAddress::new(layout::ZERO_PAGE_ADDR),
            boot_params.as_bytes(),
        )
        .expect("failed to write boot params");

    // 7. Create vCPU with 64-bit long mode state.
    let vcpu = vm_fd.create_vcpu(0).expect("failed to create vCPU");

    // Set CPUID — the guest needs to see proper CPU features (NX, LM, etc.)
    // or it will triple-fault during early boot.
    let cpuid_entries = kvm
        .get_supported_cpuid(256)
        .expect("failed to get supported CPUID");
    vcpu.set_cpuid2(&cpuid_entries)
        .expect("failed to set CPUID");

    let mut sregs = vcpu.get_sregs().expect("failed to get sregs");
    cpu_setup::configure_64bit_sregs(&mut sregs);
    vcpu.set_sregs(&sregs).expect("failed to set sregs");

    let mut regs = vcpu.get_regs().expect("failed to get regs");
    cpu_setup::configure_64bit_regs(&mut regs, elf.entry_point);
    regs.rsi = layout::ZERO_PAGE_ADDR;
    vcpu.set_regs(&regs).expect("failed to set regs");

    // 8. Run with serial capture, 15-second timeout.
    let mut mmio_bus = MmioBus::new();
    let (output, reason, diag) = exit_handler::run_vcpu_with_capture(
        &vcpu,
        &mut mmio_bus,
        Duration::from_secs(15),
        1024 * 1024, // 1 MiB max
    )
    .expect("vCPU run failed");

    // Dump register state for diagnostics.
    if let Ok(regs) = vcpu.get_regs() {
        eprintln!(
            "--- Registers after exit ---\n\
             RIP={:#018x} RSP={:#018x} RSI={:#018x}\n\
             RAX={:#018x} RBX={:#018x} RCX={:#018x} RDX={:#018x}\n\
             RDI={:#018x} RBP={:#018x} RFLAGS={:#018x}",
            regs.rip, regs.rsp, regs.rsi, regs.rax, regs.rbx, regs.rcx, regs.rdx, regs.rdi,
            regs.rbp, regs.rflags
        );
    }
    if let Ok(sregs) = vcpu.get_sregs() {
        eprintln!(
            "CR0={:#018x} CR3={:#018x} CR4={:#018x} EFER={:#018x}",
            sregs.cr0, sregs.cr3, sregs.cr4, sregs.efer
        );
    }

    eprintln!(
        "--- Diagnostics: exits={}, io_in={}, io_out={}, io_out_serial={}, mmio={} ---",
        diag.total_exits, diag.io_in_count, diag.io_out_count, diag.io_out_serial_count, diag.mmio_count
    );

    let output_str = String::from_utf8_lossy(&output);
    eprintln!(
        "--- Serial output ({} bytes, stop={:?}) ---",
        output.len(),
        reason
    );
    eprintln!("{}", output_str);
    eprintln!("--- End serial output ---");

    // 9. Assert kernel booted.
    assert!(
        output_str.contains("Linux version"),
        "expected serial output to contain 'Linux version', got {} bytes of output",
        output.len()
    );
}

// ---------------------------------------------------------------------------
// Test 3: Wasm hello world with stdout capture
// ---------------------------------------------------------------------------
#[test]
fn test_wasm_hello_world() {
    let fixtures = fixtures_dir();
    let wat_path = fixtures.join("hello.wat");

    if !wat_path.exists() {
        panic!(
            "test fixture not found: {} — this should be checked in",
            wat_path.display()
        );
    }

    let wat_source = std::fs::read_to_string(&wat_path).expect("failed to read hello.wat");

    let config = nova_wasm::WasmEngineConfig::default();
    let engine = nova_wasm::create_engine(&config).expect("failed to create engine");

    // Compile WAT to Wasm.
    let module = wasmtime::Module::new(&engine, &wat_source).expect("failed to compile WAT module");

    // Run with captured stdout.
    let ctx =
        nova_wasm::WasiContextWithCapture::new(&engine).expect("failed to create WASI context");
    let stdout = ctx.run(&module).expect("failed to run module");

    assert_eq!(stdout.trim(), "Hello from Wasm");
}

// ---------------------------------------------------------------------------
// Test 4: 64-bit CPU state verification
// ---------------------------------------------------------------------------
#[test]
fn test_cpu_state_64bit() {
    use nova_boot::cpu_setup;
    use nova_kvm::kvm_bindings::{KvmRegs, KvmSregs};

    let mut sregs = KvmSregs::default();
    cpu_setup::configure_64bit_sregs(&mut sregs);

    // CR0: PE + PG
    assert_ne!(sregs.cr0 & 1, 0, "CR0.PE should be set");
    assert_ne!(sregs.cr0 & (1 << 31), 0, "CR0.PG should be set");

    // CR3 = page table root
    assert_eq!(sregs.cr3, nova_boot::layout::PAGE_TABLE_ADDR);

    // CR4: PAE
    assert_ne!(sregs.cr4 & (1 << 5), 0, "CR4.PAE should be set");

    // EFER: LME + LMA
    assert_ne!(sregs.efer & (1 << 8), 0, "EFER.LME should be set");
    assert_ne!(sregs.efer & (1 << 10), 0, "EFER.LMA should be set");

    // CS: 64-bit code segment
    assert_eq!(sregs.cs.selector, 0x08);
    assert_eq!(sregs.cs.l, 1, "CS.L should be 1 for 64-bit");
    assert_eq!(sregs.cs.db, 0, "CS.D should be 0 for 64-bit");
    assert_eq!(sregs.cs.present, 1);

    // DS: data segment
    assert_eq!(sregs.ds.selector, 0x10);
    assert_eq!(sregs.ds.present, 1);

    // GDT
    assert_eq!(sregs.gdt.base, nova_boot::layout::GDT_ADDR);

    // General registers
    let mut regs = KvmRegs::default();
    cpu_setup::configure_64bit_regs(&mut regs, 0xDEAD_BEEF);

    assert_eq!(regs.rip, 0xDEAD_BEEF);
    assert_eq!(regs.rsp, nova_boot::layout::BOOT_STACK_ADDR);
    assert_eq!(regs.rflags, 0x2);
}
