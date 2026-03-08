//! Sandbox state machine and configuration.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Result, RuntimeError};

/// Sandbox lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxState {
    /// Created but not yet started.
    Created,
    /// Running with an active microVM.
    Running,
    /// Stopped (exited or killed).
    Stopped,
    /// In an error state.
    Error,
}

impl std::fmt::Display for SandboxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// The kind of sandbox execution environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxKind {
    /// A full microVM sandbox (default).
    Vm,
    /// A Wasm sandbox.
    Wasm {
        /// Path to the Wasm module.
        module_path: PathBuf,
        /// Entry function to call (e.g. "_start" or "add").
        entry_function: String,
    },
}

impl Default for SandboxKind {
    fn default() -> Self {
        Self::Vm
    }
}

/// Configuration for a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Memory in MiB.
    pub memory_mib: u32,
    /// Path to the kernel image.
    pub kernel: PathBuf,
    /// Path to the rootfs image.
    pub rootfs: PathBuf,
    /// Kernel command line.
    pub cmdline: String,
    /// Network configuration.
    pub network: Option<NetworkConfig>,
    /// Sandbox execution kind.
    #[serde(default)]
    pub kind: SandboxKind,
}

/// Network configuration for a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// TAP device name.
    pub tap_device: String,
    /// Guest IP address (CIDR).
    pub guest_ip: String,
    /// Host IP address (CIDR).
    pub host_ip: String,
    /// MAC address.
    pub mac_address: String,
}

/// A sandbox instance representing a microVM or Wasm execution.
pub struct Sandbox {
    /// Unique sandbox identifier.
    id: String,
    /// Current state.
    state: SandboxState,
    /// Sandbox configuration.
    config: SandboxConfig,
    /// PID of the VMM process (when running).
    pid: Option<u32>,
    /// When the sandbox was created.
    created_at: SystemTime,
    /// Captured stdout from Wasm execution (if applicable).
    wasm_output: Option<String>,
    /// Return values from Wasm function calls.
    wasm_result: Option<Vec<i64>>,
}

impl Sandbox {
    /// Create a new sandbox in the `Created` state.
    pub fn new(id: String, config: SandboxConfig) -> Self {
        tracing::info!(sandbox_id = %id, "creating sandbox");
        Self {
            id,
            state: SandboxState::Created,
            config,
            pid: None,
            created_at: SystemTime::now(),
            wasm_output: None,
            wasm_result: None,
        }
    }

    /// Start the sandbox (transition from Created -> Running).
    ///
    /// In production, this would spawn a VMM child process.
    pub fn start(&mut self) -> Result<()> {
        if self.state != SandboxState::Created {
            return Err(RuntimeError::InvalidState {
                id: self.id.clone(),
                from: self.state.to_string(),
                to: "running".to_string(),
            });
        }

        tracing::info!(
            sandbox_id = %self.id,
            vcpus = self.config.vcpus,
            memory_mib = self.config.memory_mib,
            "starting sandbox"
        );

        // In production: spawn nova-vmm as a child process.
        // For now, simulate with a fake PID.
        self.pid = Some(std::process::id() + 1000);
        self.state = SandboxState::Running;

        Ok(())
    }

    /// Stop the sandbox (transition from Running -> Stopped).
    pub fn stop(&mut self) -> Result<()> {
        if self.state != SandboxState::Running {
            return Err(RuntimeError::InvalidState {
                id: self.id.clone(),
                from: self.state.to_string(),
                to: "stopped".to_string(),
            });
        }

        tracing::info!(sandbox_id = %self.id, "stopping sandbox");

        // In production: send SIGTERM to the VMM process, wait, then SIGKILL.
        self.pid = None;
        self.state = SandboxState::Stopped;

        Ok(())
    }

    /// Get the sandbox ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the current state.
    pub fn state(&self) -> SandboxState {
        self.state
    }

    /// Get the VMM process PID (if running).
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Get the sandbox configuration.
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Get the creation time.
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// Get captured Wasm stdout output.
    pub fn wasm_output(&self) -> Option<&str> {
        self.wasm_output.as_deref()
    }

    /// Set captured Wasm stdout output.
    pub fn set_wasm_output(&mut self, output: String) {
        self.wasm_output = Some(output);
    }

    /// Get Wasm function return values.
    pub fn wasm_result(&self) -> Option<&[i64]> {
        self.wasm_result.as_deref()
    }

    /// Set Wasm function return values.
    pub fn set_wasm_result(&mut self, result: Vec<i64>) {
        self.wasm_result = Some(result);
    }

    /// Start the sandbox with a pre-built VM (L4 pool or L3 snapshot restore).
    pub fn start_with_vm(&mut self) -> Result<()> {
        if self.state != SandboxState::Created {
            return Err(RuntimeError::InvalidState {
                id: self.id.clone(),
                from: self.state.to_string(),
                to: "running".to_string(),
            });
        }

        tracing::info!(
            sandbox_id = %self.id,
            "starting sandbox with pre-built VM"
        );

        self.pid = Some(std::process::id() + 2000);
        self.state = SandboxState::Running;
        Ok(())
    }

    /// Set the state directly (used by orchestrator for Wasm execution).
    pub fn set_state(&mut self, state: SandboxState) {
        self.state = state;
    }

    /// Mark the sandbox as errored.
    pub fn set_error(&mut self) {
        self.state = SandboxState::Error;
        self.pid = None;
    }
}
