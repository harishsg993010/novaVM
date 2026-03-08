//! TracingPolicy — TOML-based configuration for eBPF probe selection.
//!
//! Inspired by Tetragon's TracingPolicy CRD, this module defines a
//! declarative format for specifying which eBPF programs to load,
//! where to attach them, and what events to filter.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{EyeError, Result};

fn default_true() -> bool {
    true
}

/// Top-level tracing policy configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TracingPolicy {
    /// Policy name.
    pub name: String,
    /// Probe specifications.
    pub probes: Vec<ProbeSpec>,
    /// Optional event filters.
    pub filters: Option<FilterSpec>,
}

/// Specification for a single eBPF probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSpec {
    /// Hook type: kprobe, tracepoint, or uprobe.
    pub hook_type: String,
    /// Target function or tracepoint (e.g. "vfs_open", "sched/sched_process_exec").
    pub target: String,
    /// Optional path to compiled eBPF bytecode.
    pub bytecode: Option<String>,
    /// Optional binary path for uprobes (e.g. "/usr/lib/libssl.so").
    pub binary: Option<String>,
    /// Whether this probe is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Event filter specification.
#[derive(Debug, Clone, Deserialize)]
pub struct FilterSpec {
    /// Filter by PID (allow only these PIDs).
    pub pids: Option<Vec<u32>>,
    /// Filter by event type name (e.g. "process_exec", "file_open").
    pub event_types: Option<Vec<String>>,
    /// Exclude events from these command names.
    pub exclude_comms: Option<Vec<String>>,
}

impl TracingPolicy {
    /// Load a tracing policy from a TOML file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| EyeError::LoadError {
            name: "tracing_policy".into(),
            reason: format!("failed to read policy file: {}", e),
        })?;
        Self::from_toml(&content)
    }

    /// Parse a tracing policy from a TOML string.
    pub fn from_toml(content: &str) -> Result<Self> {
        toml::from_str(content).map_err(|e| EyeError::LoadError {
            name: "tracing_policy".into(),
            reason: format!("failed to parse TOML: {}", e),
        })
    }
}
