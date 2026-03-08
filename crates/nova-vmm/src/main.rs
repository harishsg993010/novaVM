//! NovaVM VMM — Virtual Machine Monitor entry point.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // Initialize tracing.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nova=info".parse().unwrap()))
        .init();

    // Parse arguments: nova-vmm <config.toml>
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: nova-vmm <config.toml>");
        std::process::exit(1);
    }

    let config_path = PathBuf::from(&args[1]);
    let vm_config =
        nova_vmm::config::VmConfig::from_file(&config_path).context("failed to parse VM config")?;

    tracing::info!(
        config = %config_path.display(),
        vcpus = vm_config.vcpus,
        memory_mib = vm_config.memory_mib,
        kernel = %vm_config.kernel.path.display(),
        "starting NovaVM"
    );

    // Set up signal handling for graceful shutdown.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown_clone)
        .context("failed to register SIGINT handler")?;
    let shutdown_clone2 = Arc::clone(&shutdown);
    signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown_clone2)
        .context("failed to register SIGTERM handler")?;

    // Build the VM.
    let mut vm = nova_vmm::builder::build_vm(&vm_config).context("failed to build VM")?;

    tracing::info!("VM built, starting vCPU run loop");

    // Run the first vCPU (single-threaded for now).
    if let Some(vcpu) = vm.vcpus.first() {
        nova_vmm::exit_handler::run_vcpu_loop(vcpu, &mut vm.mmio_bus)?;
    }

    tracing::info!("NovaVM exited cleanly");
    Ok(())
}
