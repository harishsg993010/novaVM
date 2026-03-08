//! Stage 3: OCI image handling tests.
//!
//! Tests 1-4 run on any platform (no KVM needed).
//! Tests 5-8 require Linux with KVM and `NOVAVM_REAL_TESTS=1`.
//!
//! Run non-KVM tests:
//!   cargo test -p nova-integration-tests oci_test
//!
//! Run all OCI tests (Linux with KVM):
//!   NOVAVM_REAL_TESTS=1 cargo test -p nova-integration-tests oci_test -- --nocapture

use std::io::Write;
use std::path::PathBuf;

use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

/// Compute the end of kernel memory from ELF segments.
/// Returns the first address past the kernel, aligned to 2 MiB.
#[cfg(target_os = "linux")]
fn kernel_end_addr(elf: &nova_boot::ElfKernel) -> u64 {
    let mut max_end: u64 = 0;
    for seg in &elf.segments {
        let end = seg.guest_addr + seg.mem_size as u64;
        if end > max_end {
            max_end = end;
        }
    }
    // Align up to 2 MiB boundary for safety.
    (max_end + 0x1FFFFF) & !0x1FFFFF
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn real_tests_enabled() -> bool {
    std::env::var("NOVAVM_REAL_TESTS").map_or(false, |v| v == "1")
}

// ---------------------------------------------------------------------------
// Helper: create a synthetic OCI layout in a tempdir
// ---------------------------------------------------------------------------

/// Create a tar.gz layer containing the given files.
/// Each entry is (path, contents).
fn create_tar_gz_layer(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).expect("set path");
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &data[..]).expect("append entry");
        }
        builder.finish().expect("finish tar");
    }

    let mut gz_data = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut gz_data, Compression::fast());
        encoder.write_all(&tar_data).expect("compress");
        encoder.finish().expect("finish gz");
    }
    gz_data
}

/// Create a tar.gz layer that includes a whiteout entry.
fn create_tar_gz_layer_with_whiteout(
    files: &[(&str, &[u8])],
    whiteouts: &[&str],
) -> Vec<u8> {
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).expect("set path");
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &data[..]).expect("append entry");
        }
        // Add whiteout entries (zero-length files).
        for wh in whiteouts {
            let mut header = tar::Header::new_gnu();
            header.set_path(wh).expect("set whiteout path");
            header.set_size(0);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append(&header, std::io::empty())
                .expect("append whiteout");
        }
        builder.finish().expect("finish tar");
    }

    let mut gz_data = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut gz_data, Compression::fast());
        encoder.write_all(&tar_data).expect("compress");
        encoder.finish().expect("finish gz");
    }
    gz_data
}

/// Compute SHA-256 hex digest of data.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Write a blob to the OCI layout's blobs/sha256/ directory.
fn write_blob(layout_dir: &std::path::Path, data: &[u8]) -> String {
    let digest = sha256_hex(data);
    let blob_dir = layout_dir.join("blobs").join("sha256");
    std::fs::create_dir_all(&blob_dir).expect("create blob dir");
    std::fs::write(blob_dir.join(&digest), data).expect("write blob");
    format!("sha256:{digest}")
}

/// Build a complete synthetic OCI layout directory with given layers.
///
/// Returns the path and the number of layers.
fn build_synthetic_oci_layout(
    dir: &std::path::Path,
    layer_blobs: &[Vec<u8>],
) -> PathBuf {
    let layout_dir = dir.join("oci-layout-test");
    std::fs::create_dir_all(&layout_dir).expect("create layout dir");

    // oci-layout file.
    std::fs::write(
        layout_dir.join("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .expect("write oci-layout");

    // Write layer blobs and collect descriptors.
    let mut layer_descriptors = Vec::new();
    for blob in layer_blobs {
        let digest = write_blob(&layout_dir, blob);
        layer_descriptors.push(serde_json::json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": digest,
            "size": blob.len()
        }));
    }

    // Config.
    let config_json = serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Cmd": ["/bin/sh"],
            "Env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": layer_descriptors.iter().map(|d| d["digest"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    });
    let config_bytes = serde_json::to_vec(&config_json).expect("serialize config");
    let config_digest = write_blob(&layout_dir, &config_bytes);

    // Manifest.
    let manifest_json = serde_json::json!({
        "schemaVersion": 2,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_bytes.len()
        },
        "layers": layer_descriptors
    });
    let manifest_bytes = serde_json::to_vec(&manifest_json).expect("serialize manifest");
    let manifest_digest = write_blob(&layout_dir, &manifest_bytes);

    // index.json.
    let index_json = serde_json::json!({
        "schemaVersion": 2,
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": manifest_digest,
            "size": manifest_bytes.len(),
            "platform": {
                "architecture": "amd64",
                "os": "linux"
            }
        }]
    });
    std::fs::write(
        layout_dir.join("index.json"),
        serde_json::to_string_pretty(&index_json).expect("serialize index"),
    )
    .expect("write index.json");

    layout_dir
}

// ===========================================================================
// Test 1: OCI layout parsing (no KVM)
// ===========================================================================
#[test]
fn test_oci_layout_parsing() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Single layer with one file.
    let layer = create_tar_gz_layer(&[("hello.txt", b"Hello, OCI!")]);
    let layout_dir = build_synthetic_oci_layout(tmp.path(), &[layer]);

    let layout =
        nova_runtime::image::OciImageLayout::open(&layout_dir).expect("failed to parse OCI layout");

    // Verify manifest.
    assert_eq!(layout.layers().len(), 1, "should have 1 layer");

    // Verify config.
    assert_eq!(layout.config.architecture, "amd64");
    assert_eq!(layout.config.os, "linux");
    assert_eq!(
        layout.config.config.cmd.as_deref(),
        Some(&["/bin/sh".to_string()][..])
    );

    // Verify rootfs.
    assert_eq!(layout.config.rootfs.diff_ids.len(), 1);

    eprintln!("test_oci_layout_parsing: PASSED");
}

// ===========================================================================
// Test 2: Layer extraction with whiteouts (no KVM)
// ===========================================================================
#[test]
fn test_oci_layer_extraction() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Layer 1: creates /hello.txt.
    let layer1 = create_tar_gz_layer(&[("hello.txt", b"hello world")]);

    // Layer 2: creates /world.txt + whiteout for /hello.txt.
    let layer2 = create_tar_gz_layer_with_whiteout(
        &[("world.txt", b"world file")],
        &[".wh.hello.txt"],
    );

    let layout_dir = build_synthetic_oci_layout(tmp.path(), &[layer1, layer2]);
    let layout =
        nova_runtime::image::OciImageLayout::open(&layout_dir).expect("parse OCI layout");

    // Extract layers.
    let output_dir = tmp.path().join("rootfs");
    nova_runtime::image::extract::extract_layers(
        layout.layout_dir(),
        layout.layers(),
        &output_dir,
    )
    .expect("extract layers");

    // hello.txt should be deleted by whiteout.
    assert!(
        !output_dir.join("hello.txt").exists(),
        "hello.txt should be deleted by whiteout"
    );

    // world.txt should exist.
    assert!(
        output_dir.join("world.txt").exists(),
        "world.txt should exist"
    );
    let contents = std::fs::read_to_string(output_dir.join("world.txt")).expect("read world.txt");
    assert_eq!(contents, "world file");

    eprintln!("test_oci_layer_extraction: PASSED");
}

// ===========================================================================
// Test 3: Directory to CPIO (no KVM)
// ===========================================================================
#[test]
fn test_rootfs_to_cpio() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Create a minimal rootfs.
    let root = tmp.path().join("rootfs");
    std::fs::create_dir_all(root.join("bin")).expect("mkdir bin");
    std::fs::create_dir_all(root.join("etc")).expect("mkdir etc");
    std::fs::write(root.join("etc/hostname"), b"test-host\n").expect("write hostname");
    std::fs::write(root.join("bin/sh"), b"#!/fake/shell\n").expect("write sh");

    let cpio = nova_boot::initrd::dir_to_cpio(&root).expect("dir_to_cpio");

    // Verify cpio magic.
    let magic = std::str::from_utf8(&cpio[..6]).expect("magic utf8");
    assert_eq!(magic, "070701", "cpio should start with newc magic");

    // Verify TRAILER is present.
    let cpio_str = String::from_utf8_lossy(&cpio);
    assert!(
        cpio_str.contains("TRAILER!!!"),
        "cpio should contain TRAILER"
    );

    // Verify some filenames are present.
    assert!(cpio_str.contains("etc"), "cpio should contain 'etc'");
    assert!(cpio_str.contains("bin"), "cpio should contain 'bin'");

    // Verify non-trivial size.
    assert!(cpio.len() > 200, "cpio should have content, got {} bytes", cpio.len());

    eprintln!("test_rootfs_to_cpio: PASSED ({} bytes)", cpio.len());
}

// ===========================================================================
// Test 4: End-to-end OCI → parse → extract → CPIO (no KVM)
// ===========================================================================
#[test]
fn test_oci_to_initramfs_pipeline() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Create a 2-layer OCI layout.
    let layer1 = create_tar_gz_layer(&[
        ("etc/hostname", b"nova-guest\n"),
        ("bin/sh", b"#!/bin/sh\necho hello\n"),
    ]);
    let layer2 = create_tar_gz_layer(&[
        ("etc/resolv.conf", b"nameserver 8.8.8.8\n"),
        ("usr/bin/app", b"#!/bin/sh\necho app\n"),
    ]);

    let layout_dir = build_synthetic_oci_layout(tmp.path(), &[layer1, layer2]);
    let layout =
        nova_runtime::image::OciImageLayout::open(&layout_dir).expect("parse OCI layout");

    // Extract layers.
    let rootfs_dir = tmp.path().join("rootfs");
    nova_runtime::image::extract::extract_layers(
        layout.layout_dir(),
        layout.layers(),
        &rootfs_dir,
    )
    .expect("extract layers");

    // Verify extracted files.
    assert!(rootfs_dir.join("etc/hostname").exists());
    assert!(rootfs_dir.join("etc/resolv.conf").exists());
    assert!(rootfs_dir.join("bin/sh").exists());
    assert!(rootfs_dir.join("usr/bin/app").exists());

    // Convert to cpio.
    let cpio = nova_boot::initrd::dir_to_cpio(&rootfs_dir).expect("dir_to_cpio");

    // Verify cpio format.
    let magic = std::str::from_utf8(&cpio[..6]).expect("magic utf8");
    assert_eq!(magic, "070701");

    let cpio_str = String::from_utf8_lossy(&cpio);
    assert!(cpio_str.contains("TRAILER!!!"));
    assert!(cpio_str.contains("etc/hostname"));
    assert!(cpio_str.contains("bin/sh"));

    eprintln!(
        "test_oci_to_initramfs_pipeline: PASSED ({} bytes cpio from {} files)",
        cpio.len(),
        4
    );
}

// ===========================================================================
// Test 5: Boot VM with synthetic initramfs (KVM required)
// ===========================================================================
#[test]
#[cfg(target_os = "linux")]
fn test_oci_initramfs_vm_boot() {
    if !real_tests_enabled() {
        eprintln!("skipping test_oci_initramfs_vm_boot: NOVAVM_REAL_TESTS not set");
        return;
    }

    let vmlinux = fixtures_dir().join("vmlinux-5.10");
    if !vmlinux.exists() {
        eprintln!("skipping: vmlinux not found");
        return;
    }

    use nova_boot::boot_params::E820Entry;
    use nova_boot::{cpu_setup, layout, BootParams, CmdlineBuilder, ElfKernel};
    use nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion;
    use nova_kvm::Kvm;
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_vmm::device_mgr::MmioBus;
    use nova_vmm::exit_handler;
    use std::time::Duration;

    // Create a minimal synthetic cpio initramfs.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("rootfs");
    std::fs::create_dir_all(root.join("dev")).expect("mkdir");
    std::fs::create_dir_all(root.join("proc")).expect("mkdir");
    let init_path = root.join("init");
    std::fs::write(&init_path, b"#!/bin/sh\necho NOVAVM_OCI_BOOT_OK\n").expect("write init");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&init_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod init");
    }

    let cpio = nova_boot::initrd::dir_to_cpio(&root).expect("dir_to_cpio");
    eprintln!("synthetic initramfs: {} bytes", cpio.len());

    // Boot VM with initramfs.
    let kvm = Kvm::open().expect("KVM");
    let vm_fd = kvm.create_vm().expect("create VM");
    vm_fd.set_tss_addr(0xFFFB_D000).expect("TSS");
    vm_fd.create_irqchip().expect("irqchip");
    vm_fd.create_pit2().expect("PIT2");

    let mem_size: usize = 128 * 1024 * 1024;
    let guest_memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false)
        .expect("guest memory");
    let host_addr = guest_memory.region_host_addr(0).expect("host addr");
    vm_fd
        .set_user_memory_region(&KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem_size as u64,
            userspace_addr: host_addr,
        })
        .expect("set mem");

    // Load kernel.
    let kernel_data = std::fs::read(&vmlinux).expect("read vmlinux");
    let elf = ElfKernel::parse(std::io::Cursor::new(&kernel_data)).expect("parse ELF");
    elf.load_into_memory(&guest_memory).expect("load ELF");

    // Compute initrd address AFTER the kernel to avoid overwriting it.
    let initrd_addr = kernel_end_addr(&elf);
    eprintln!("kernel end -> initrd at {:#x}", initrd_addr);

    cpu_setup::setup_long_mode_page_tables(&guest_memory).expect("page tables");
    cpu_setup::setup_gdt(&guest_memory).expect("GDT");

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

    // Load initramfs AFTER the kernel.
    nova_boot::initrd::load_initrd_at(
        &mut std::io::Cursor::new(&cpio),
        &guest_memory,
        &mut boot_params,
        initrd_addr,
    )
    .expect("load initrd");

    CmdlineBuilder::new()
        .raw("earlycon=uart8250,io,0x3f8,115200 console=ttyS0 reboot=k panic=1 nokaslr no_timer_check tsc=reliable rdinit=/init keep_bootcon i8042.noaux i8042.nomux i8042.dumbkbd")
        .write_to_memory(&guest_memory, &mut boot_params)
        .expect("cmdline");

    guest_memory
        .write_slice(GuestAddress::new(layout::ZERO_PAGE_ADDR), boot_params.as_bytes())
        .expect("boot params");

    let vcpu = vm_fd.create_vcpu(0).expect("create vCPU");
    let cpuid = kvm.get_supported_cpuid(256).expect("CPUID");
    vcpu.set_cpuid2(&cpuid).expect("set CPUID");

    let mut sregs = vcpu.get_sregs().expect("get sregs");
    cpu_setup::configure_64bit_sregs(&mut sregs);
    vcpu.set_sregs(&sregs).expect("set sregs");

    let mut regs = vcpu.get_regs().expect("get regs");
    cpu_setup::configure_64bit_regs(&mut regs, elf.entry_point);
    regs.rsi = layout::ZERO_PAGE_ADDR;
    vcpu.set_regs(&regs).expect("set regs");

    // Run until we see the kernel booting.
    let mut mmio_bus = MmioBus::new();
    let (output, reason, diag) = exit_handler::run_vcpu_with_capture(
        &vcpu,
        &mut mmio_bus,
        Duration::from_secs(30),
        1024 * 1024,
    )
    .expect("vCPU run");

    let output_str = String::from_utf8_lossy(&output);
    eprintln!("--- Stop reason: {:?} ---", reason);
    eprintln!(
        "--- Diagnostics: exits={}, io_in={}, io_out={}, io_out_serial={}, mmio={}, elapsed={:?} ---",
        diag.total_exits, diag.io_in_count, diag.io_out_count, diag.io_out_serial_count, diag.mmio_count, diag.elapsed
    );
    eprintln!("--- Serial output ({} bytes) ---", output.len());
    if output.len() > 2000 {
        eprintln!("{}", &output_str[..2000]);
        eprintln!("... (truncated)");
    } else {
        eprintln!("{output_str}");
    }
    eprintln!("--- End serial output ---");

    // The kernel should print about the initramfs.
    assert!(
        output_str.contains("initramfs")
            || output_str.contains("Unpacking initramfs")
            || output_str.contains("rootfs")
            || output_str.contains("Linux version"),
        "kernel should boot and produce serial output, got {} bytes, reason={:?}",
        output.len(),
        reason,
    );

    eprintln!("test_oci_initramfs_vm_boot: PASSED");
}

// ===========================================================================
// Test 6: Real alpine OCI boot (KVM + fixture required)
// ===========================================================================
#[test]
#[cfg(target_os = "linux")]
fn test_real_alpine_oci_boot() {
    if !real_tests_enabled() {
        eprintln!("skipping test_real_alpine_oci_boot: NOVAVM_REAL_TESTS not set");
        return;
    }

    let vmlinux = fixtures_dir().join("vmlinux-5.10");
    let alpine_dir = fixtures_dir().join("alpine-oci");

    if !vmlinux.exists() {
        eprintln!("skipping: vmlinux not found");
        return;
    }
    if !alpine_dir.exists() {
        eprintln!(
            "skipping test_real_alpine_oci_boot: {} not found (run tests/fixtures/download-oci.sh)",
            alpine_dir.display()
        );
        return;
    }

    use nova_boot::boot_params::E820Entry;
    use nova_boot::{cpu_setup, layout, BootParams, CmdlineBuilder, ElfKernel};
    use nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion;
    use nova_kvm::Kvm;
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_vmm::device_mgr::MmioBus;
    use nova_vmm::exit_handler;
    use std::time::Duration;

    // Parse OCI layout.
    let layout =
        nova_runtime::image::OciImageLayout::open(&alpine_dir).expect("parse alpine OCI layout");
    eprintln!(
        "alpine: {} layers, arch={}",
        layout.layers().len(),
        layout.config.architecture
    );

    // Extract layers.
    let tmp = tempfile::tempdir().expect("tempdir");
    let rootfs_dir = tmp.path().join("alpine-rootfs");
    nova_runtime::image::extract::extract_layers(
        layout.layout_dir(),
        layout.layers(),
        &rootfs_dir,
    )
    .expect("extract alpine layers");

    // Inject custom /init with execute permission.
    // Write to /dev/kmsg (kernel log buffer) because userspace writes to ttyS0
    // go through the IRQ-driven 8250 path which we don't support.
    // /dev/kmsg goes through printk → serial console (synchronous polling).
    let init_path = rootfs_dir.join("init");
    std::fs::write(&init_path, b"#!/bin/sh\nmknod /dev/kmsg c 1 11 2>/dev/null\necho ALPINE_BOOT_OK > /dev/kmsg\nexec /sbin/halt -f\n")
        .expect("write init");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&init_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod init");
    }

    // Convert to cpio.
    let cpio = nova_boot::initrd::dir_to_cpio(&rootfs_dir).expect("dir_to_cpio");
    eprintln!("alpine initramfs: {} bytes ({:.1} MiB)", cpio.len(), cpio.len() as f64 / 1048576.0);

    // Boot VM.
    let kvm = Kvm::open().expect("KVM");
    let vm_fd = kvm.create_vm().expect("create VM");
    vm_fd.set_tss_addr(0xFFFB_D000).expect("TSS");
    vm_fd.create_irqchip().expect("irqchip");
    vm_fd.create_pit2().expect("PIT2");

    let mem_size: usize = 256 * 1024 * 1024; // 256 MiB for real rootfs
    let guest_memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false)
        .expect("guest memory");
    let host_addr = guest_memory.region_host_addr(0).expect("host addr");
    vm_fd
        .set_user_memory_region(&KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem_size as u64,
            userspace_addr: host_addr,
        })
        .expect("set mem");

    let kernel_data = std::fs::read(&vmlinux).expect("read vmlinux");
    let elf = ElfKernel::parse(std::io::Cursor::new(&kernel_data)).expect("parse ELF");
    elf.load_into_memory(&guest_memory).expect("load ELF");

    let initrd_addr = kernel_end_addr(&elf);
    eprintln!("kernel end -> initrd at {:#x}", initrd_addr);

    cpu_setup::setup_long_mode_page_tables(&guest_memory).expect("page tables");
    cpu_setup::setup_gdt(&guest_memory).expect("GDT");

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

    nova_boot::initrd::load_initrd_at(
        &mut std::io::Cursor::new(&cpio),
        &guest_memory,
        &mut boot_params,
        initrd_addr,
    )
    .expect("load initrd");

    CmdlineBuilder::new()
        .raw("earlycon=uart8250,io,0x3f8,115200 console=ttyS0 reboot=k panic=1 nokaslr no_timer_check tsc=reliable rdinit=/init keep_bootcon i8042.noaux i8042.nomux i8042.dumbkbd")
        .write_to_memory(&guest_memory, &mut boot_params)
        .expect("cmdline");

    guest_memory
        .write_slice(GuestAddress::new(layout::ZERO_PAGE_ADDR), boot_params.as_bytes())
        .expect("boot params");

    let vcpu = vm_fd.create_vcpu(0).expect("create vCPU");
    let cpuid = kvm.get_supported_cpuid(256).expect("CPUID");
    vcpu.set_cpuid2(&cpuid).expect("set CPUID");

    let mut sregs = vcpu.get_sregs().expect("get sregs");
    cpu_setup::configure_64bit_sregs(&mut sregs);
    vcpu.set_sregs(&sregs).expect("set sregs");

    let mut regs = vcpu.get_regs().expect("get regs");
    cpu_setup::configure_64bit_regs(&mut regs, elf.entry_point);
    regs.rsi = layout::ZERO_PAGE_ADDR;
    vcpu.set_regs(&regs).expect("set regs");

    let mut mmio_bus = MmioBus::new();
    let (output, reason, diag) = exit_handler::run_vcpu_until_match(
        &vcpu,
        &mut mmio_bus,
        Duration::from_secs(60),
        "ALPINE_BOOT_OK",
    )
    .expect("vCPU run");

    let output_str = String::from_utf8_lossy(&output);
    eprintln!("--- Stop reason: {:?}, elapsed: {:?} ---", reason, diag.elapsed);
    eprintln!("--- Diagnostics: exits={}, io_in={}, io_out={}, serial={}, mmio={} ---",
        diag.total_exits, diag.io_in_count, diag.io_out_count, diag.io_out_serial_count, diag.mmio_count);
    eprintln!("--- Serial output ({} bytes) ---", output.len());
    // Search for key diagnostic strings.
    for keyword in &["Failed", "panic", "error", "Unpacking", "ALPINE_BOOT_OK", "Freeing", "Run /init"] {
        if let Some(pos) = output_str.find(keyword) {
            let ctx_start = pos.saturating_sub(50);
            let ctx_end = (pos + 150).min(output_str.len());
            eprintln!("[MATCH '{}' @{}]: {}", keyword, pos, &output_str[ctx_start..ctx_end]);
        }
    }

    assert!(
        output_str.contains("ALPINE_BOOT_OK"),
        "expected 'ALPINE_BOOT_OK' on serial, got {} bytes, reason={:?}",
        output.len(),
        reason,
    );

    eprintln!("test_real_alpine_oci_boot: PASSED");
}

// ===========================================================================
// Test 7: Real nginx OCI contents (no KVM, fixture required)
// ===========================================================================
#[test]
fn test_real_nginx_oci_contents() {
    let nginx_dir = fixtures_dir().join("nginx-oci");
    if !nginx_dir.exists() {
        eprintln!(
            "skipping test_real_nginx_oci_contents: {} not found (run tests/fixtures/download-oci.sh)",
            nginx_dir.display()
        );
        return;
    }

    // Parse OCI layout.
    let layout =
        nova_runtime::image::OciImageLayout::open(&nginx_dir).expect("parse nginx OCI layout");
    eprintln!(
        "nginx: {} layers, arch={}, os={}",
        layout.layers().len(),
        layout.config.architecture,
        layout.config.os,
    );

    // Extract all layers.
    let tmp = tempfile::tempdir().expect("tempdir");
    let rootfs_dir = tmp.path().join("nginx-rootfs");
    nova_runtime::image::extract::extract_layers(
        layout.layout_dir(),
        layout.layers(),
        &rootfs_dir,
    )
    .expect("extract nginx layers");

    // Verify nginx binary exists.
    assert!(
        rootfs_dir.join("usr/sbin/nginx").exists(),
        "/usr/sbin/nginx should exist"
    );

    // Verify nginx config exists.
    assert!(
        rootfs_dir.join("etc/nginx/nginx.conf").exists(),
        "/etc/nginx/nginx.conf should exist"
    );

    // Verify OCI config entrypoint or cmd references nginx.
    // nginx:1.25-alpine uses "/docker-entrypoint.sh" as entrypoint
    // and ["nginx", "-g", "daemon off;"] as cmd.
    let entrypoint = layout.config.config.entrypoint.as_deref().unwrap_or(&[]);
    let cmd = layout.config.config.cmd.as_deref().unwrap_or(&[]);
    let has_nginx = entrypoint.iter().chain(cmd.iter()).any(|e| e.contains("nginx"));
    assert!(
        has_nginx,
        "entrypoint or cmd should reference 'nginx', got entrypoint={entrypoint:?} cmd={cmd:?}"
    );

    eprintln!("test_real_nginx_oci_contents: PASSED");
}

// ===========================================================================
// Test 8: Nginx OCI initramfs boot (KVM + fixture required)
// ===========================================================================
#[test]
#[cfg(target_os = "linux")]
fn test_nginx_oci_initramfs_boot() {
    if !real_tests_enabled() {
        eprintln!("skipping test_nginx_oci_initramfs_boot: NOVAVM_REAL_TESTS not set");
        return;
    }

    let vmlinux = fixtures_dir().join("vmlinux-5.10");
    let nginx_dir = fixtures_dir().join("nginx-oci");

    if !vmlinux.exists() {
        eprintln!("skipping: vmlinux not found");
        return;
    }
    if !nginx_dir.exists() {
        eprintln!(
            "skipping test_nginx_oci_initramfs_boot: {} not found (run tests/fixtures/download-oci.sh)",
            nginx_dir.display()
        );
        return;
    }

    use nova_boot::boot_params::E820Entry;
    use nova_boot::{cpu_setup, layout, BootParams, CmdlineBuilder, ElfKernel};
    use nova_kvm::kvm_bindings::KvmUserspaceMemoryRegion;
    use nova_kvm::Kvm;
    use nova_mem::{GuestAddress, GuestMemoryMmap};
    use nova_vmm::device_mgr::MmioBus;
    use nova_vmm::exit_handler;
    use std::time::Duration;

    // Parse and extract nginx OCI layout.
    let layout =
        nova_runtime::image::OciImageLayout::open(&nginx_dir).expect("parse nginx OCI layout");

    let tmp = tempfile::tempdir().expect("tempdir");
    let rootfs_dir = tmp.path().join("nginx-rootfs");
    nova_runtime::image::extract::extract_layers(
        layout.layout_dir(),
        layout.layers(),
        &rootfs_dir,
    )
    .expect("extract nginx layers");

    // Inject custom /init that verifies nginx binary, with execute permission.
    // Write to /dev/kmsg because we don't support serial IRQ for userspace tty output.
    let init_path = rootfs_dir.join("init");
    std::fs::write(
        &init_path,
        b"#!/bin/sh\nmknod /dev/kmsg c 1 11 2>/dev/null\necho NGINX_ROOTFS_OK > /dev/kmsg\nif [ -f /usr/sbin/nginx ]; then echo NGINX_BINARY_FOUND > /dev/kmsg; fi\nexec /sbin/halt -f\n",
    )
    .expect("write init");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&init_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod init");
    }

    // Convert to cpio.
    let cpio = nova_boot::initrd::dir_to_cpio(&rootfs_dir).expect("dir_to_cpio");
    eprintln!("nginx initramfs: {} bytes ({:.1} MiB)", cpio.len(), cpio.len() as f64 / 1048576.0);

    // Boot VM (512 MiB for nginx rootfs).
    let kvm = Kvm::open().expect("KVM");
    let vm_fd = kvm.create_vm().expect("create VM");
    vm_fd.set_tss_addr(0xFFFB_D000).expect("TSS");
    vm_fd.create_irqchip().expect("irqchip");
    vm_fd.create_pit2().expect("PIT2");

    let mem_size: usize = 512 * 1024 * 1024;
    let guest_memory = GuestMemoryMmap::new(&[(GuestAddress::new(0), mem_size)], false)
        .expect("guest memory");
    let host_addr = guest_memory.region_host_addr(0).expect("host addr");
    vm_fd
        .set_user_memory_region(&KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem_size as u64,
            userspace_addr: host_addr,
        })
        .expect("set mem");

    let kernel_data = std::fs::read(&vmlinux).expect("read vmlinux");
    let elf = ElfKernel::parse(std::io::Cursor::new(&kernel_data)).expect("parse ELF");
    elf.load_into_memory(&guest_memory).expect("load ELF");

    let initrd_addr = kernel_end_addr(&elf);
    eprintln!("kernel end -> initrd at {:#x}", initrd_addr);

    cpu_setup::setup_long_mode_page_tables(&guest_memory).expect("page tables");
    cpu_setup::setup_gdt(&guest_memory).expect("GDT");

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

    nova_boot::initrd::load_initrd_at(
        &mut std::io::Cursor::new(&cpio),
        &guest_memory,
        &mut boot_params,
        initrd_addr,
    )
    .expect("load initrd");

    CmdlineBuilder::new()
        .raw("earlycon=uart8250,io,0x3f8,115200 console=ttyS0 reboot=k panic=1 nokaslr no_timer_check tsc=reliable rdinit=/init keep_bootcon i8042.noaux i8042.nomux i8042.dumbkbd")
        .write_to_memory(&guest_memory, &mut boot_params)
        .expect("cmdline");

    guest_memory
        .write_slice(GuestAddress::new(layout::ZERO_PAGE_ADDR), boot_params.as_bytes())
        .expect("boot params");

    let vcpu = vm_fd.create_vcpu(0).expect("create vCPU");
    let cpuid = kvm.get_supported_cpuid(256).expect("CPUID");
    vcpu.set_cpuid2(&cpuid).expect("set CPUID");

    let mut sregs = vcpu.get_sregs().expect("get sregs");
    cpu_setup::configure_64bit_sregs(&mut sregs);
    vcpu.set_sregs(&sregs).expect("set sregs");

    let mut regs = vcpu.get_regs().expect("get regs");
    cpu_setup::configure_64bit_regs(&mut regs, elf.entry_point);
    regs.rsi = layout::ZERO_PAGE_ADDR;
    vcpu.set_regs(&regs).expect("set regs");

    let mut mmio_bus = MmioBus::new();
    let (output, reason, diag) = exit_handler::run_vcpu_until_match(
        &vcpu,
        &mut mmio_bus,
        Duration::from_secs(300),
        "NGINX_BINARY_FOUND",
    )
    .expect("vCPU run");

    let output_str = String::from_utf8_lossy(&output);
    eprintln!("--- Stop reason: {:?}, elapsed: {:?} ---", reason, diag.elapsed);
    eprintln!("--- Serial output ({} bytes) ---", output.len());
    eprintln!("{output_str}");

    assert!(
        output_str.contains("NGINX_ROOTFS_OK"),
        "expected 'NGINX_ROOTFS_OK' on serial, got {} bytes, reason={:?}",
        output.len(),
        reason,
    );
    assert!(
        output_str.contains("NGINX_BINARY_FOUND"),
        "expected 'NGINX_BINARY_FOUND' on serial, got {} bytes, reason={:?}",
        output.len(),
        reason,
    );

    eprintln!("test_nginx_oci_initramfs_boot: PASSED");
}
