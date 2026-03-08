//! eBPF program loader.
//!
//! `SensorLoader` is responsible for loading compiled eBPF bytecode and
//! attaching programs to kernel tracepoints, kprobes, and uprobes.
//!
//! The real implementation would use the [`aya`] crate. This module provides
//! the structural skeleton with placeholder logic so that the rest of the
//! sensor pipeline can be developed and tested independently.

use crate::error::{EyeError, Result};

/// Represents a loaded and attached eBPF program.
#[allow(dead_code)]
#[derive(Debug)]
pub struct AttachedProgram {
    /// Human-readable name of the program.
    name: String,
    /// The kind of attachment (tracepoint, kprobe, uprobe).
    kind: AttachKind,
}

/// The kind of eBPF attachment.
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum AttachKind {
    Tracepoint { category: String, name: String },
    Kprobe { fn_name: String },
    Uprobe { binary: String, fn_name: String },
}

/// Loads eBPF programs from compiled bytecode and attaches them to hooks.
///
/// In production this would wrap `aya::Bpf` and manage the lifecycle of
/// loaded programs and their associated maps. The current implementation
/// is a structural placeholder.
pub struct SensorLoader {
    /// Programs that have been loaded but not yet attached.
    #[allow(dead_code)]
    bytecode_cache: Vec<(String, Vec<u8>)>,
    /// Programs that have been successfully attached.
    #[allow(dead_code)]
    attached: Vec<AttachedProgram>,
}

impl SensorLoader {
    /// Create a new, empty `SensorLoader`.
    pub fn new() -> Self {
        tracing::debug!("creating new SensorLoader");
        Self {
            bytecode_cache: Vec::new(),
            attached: Vec::new(),
        }
    }

    /// Load an eBPF program from raw bytecode.
    ///
    /// The `name` is used for logging and error messages. The `bytecode` is
    /// the ELF object produced by `llvm` / `clang` for the `bpfel` target.
    ///
    /// In a real implementation this would call `aya::Bpf::load()`.
    pub fn load_program(&mut self, name: &str, bytecode: &[u8]) -> Result<()> {
        if bytecode.is_empty() {
            return Err(EyeError::LoadError {
                name: name.to_string(),
                reason: "bytecode is empty".to_string(),
            });
        }

        tracing::info!(
            program = name,
            size = bytecode.len(),
            "loading eBPF program"
        );
        self.bytecode_cache
            .push((name.to_string(), bytecode.to_vec()));
        Ok(())
    }

    /// Attach a loaded program to a kernel tracepoint.
    ///
    /// For example: `attach_tracepoint("syscalls", "sys_enter_execve")`.
    ///
    /// In a real implementation this would call
    /// `program.attach_tracepoint(category, name)`.
    pub fn attach_tracepoint(&mut self, category: &str, name: &str) -> Result<()> {
        tracing::info!(category, name, "attaching tracepoint");

        self.attached.push(AttachedProgram {
            name: format!("{category}/{name}"),
            kind: AttachKind::Tracepoint {
                category: category.to_string(),
                name: name.to_string(),
            },
        });

        Ok(())
    }

    /// Attach a loaded program to a kernel function via kprobe.
    ///
    /// For example: `attach_kprobe("tcp_v4_connect")`.
    pub fn attach_kprobe(&mut self, fn_name: &str) -> Result<()> {
        tracing::info!(fn_name, "attaching kprobe");

        self.attached.push(AttachedProgram {
            name: format!("kprobe:{fn_name}"),
            kind: AttachKind::Kprobe {
                fn_name: fn_name.to_string(),
            },
        });

        Ok(())
    }

    /// Attach a loaded program to a userspace function via uprobe.
    ///
    /// For example: `attach_uprobe("/usr/lib/libssl.so", "SSL_write")`.
    pub fn attach_uprobe(&mut self, binary: &str, fn_name: &str) -> Result<()> {
        tracing::info!(binary, fn_name, "attaching uprobe");

        self.attached.push(AttachedProgram {
            name: format!("uprobe:{binary}:{fn_name}"),
            kind: AttachKind::Uprobe {
                binary: binary.to_string(),
                fn_name: fn_name.to_string(),
            },
        });

        Ok(())
    }

    /// Returns the number of currently attached programs.
    pub fn attached_count(&self) -> usize {
        self.attached.len()
    }
}

impl Default for SensorLoader {
    fn default() -> Self {
        Self::new()
    }
}
