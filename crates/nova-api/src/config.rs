//! Daemon-level TOML configuration.
//!
//! Loaded from `--config /etc/nova/nova.toml` (or the default path).
//! Replaces all `NOVA_*` env-var knobs with a single config file.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use nova_eye::policy::ProbeSpec;

// ── defaults ───────────────────────────────────────────────────────────

fn default_socket() -> String {
    "/run/nova/nova.sock".into()
}
fn default_image_dir() -> String {
    "/var/lib/nova/images".into()
}
fn default_events_log() -> String {
    "/var/run/nova/events.jsonl".into()
}
fn default_ebpf_dir() -> String {
    "/opt/nova/ebpf".into()
}
fn default_agent_path() -> String {
    "/opt/nova/bin/nova-eye-agent".into()
}
fn default_api_port() -> u16 {
    9800
}
fn default_event_port() -> u16 {
    9876
}
fn default_true() -> bool {
    true
}
fn default_max_vcpus() -> u32 {
    8
}
fn default_max_memory() -> u32 {
    8192
}
fn default_max_sandboxes() -> u32 {
    100
}
fn default_bundle_dir() -> String {
    "/var/lib/nova/policy/bundles".into()
}
fn default_ruleset() -> String {
    "default".into()
}

// ── structs ────────────────────────────────────────────────────────────

/// Top-level daemon configuration.
#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub daemon: DaemonSettings,
    #[serde(default)]
    pub sensor: SensorConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
}

/// Policy engine configuration.
#[derive(Debug, Deserialize)]
pub struct PolicyConfig {
    /// Whether to enable admission control on sandbox creation.
    #[serde(default = "default_true")]
    pub admission_enabled: bool,
    /// Whether to enable runtime enforcement in the sensor pipeline.
    #[serde(default)]
    pub enforcement_enabled: bool,
    /// Max vCPUs per sandbox (admission check).
    #[serde(default = "default_max_vcpus")]
    pub max_vcpus: u32,
    /// Max memory MiB per sandbox (admission check).
    #[serde(default = "default_max_memory")]
    pub max_memory_mib: u32,
    /// Max concurrent sandboxes.
    #[serde(default = "default_max_sandboxes")]
    pub max_sandboxes: u32,
    /// Image allowlist prefixes. Empty = allow all.
    #[serde(default)]
    pub allowed_images: Vec<String>,
    /// Directory to store Wasm policy bundles.
    #[serde(default = "default_bundle_dir")]
    pub bundle_dir: String,
    /// Enforcement rule set: "default", "strict", or "none".
    #[serde(default = "default_ruleset")]
    pub enforcement_rules: String,
    /// Custom enforcement rules (appended after the builtin ruleset).
    #[serde(default)]
    pub rules: Vec<nova_policy::EnforcementRule>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            admission_enabled: default_true(),
            enforcement_enabled: false,
            max_vcpus: default_max_vcpus(),
            max_memory_mib: default_max_memory(),
            max_sandboxes: default_max_sandboxes(),
            allowed_images: Vec::new(),
            bundle_dir: default_bundle_dir(),
            enforcement_rules: default_ruleset(),
            rules: Vec::new(),
        }
    }
}

/// General daemon settings.
#[derive(Debug, Deserialize)]
pub struct DaemonSettings {
    /// Unix socket path for gRPC.
    #[serde(default = "default_socket")]
    pub socket: String,
    /// Directory for OCI images / caches.
    #[serde(default = "default_image_dir")]
    pub image_dir: String,
    /// Path to the guest kernel (vmlinux). `None` → simulated mode.
    pub kernel: Option<String>,
    /// TAP device name for guest networking. `None` → no networking.
    pub tap_device: Option<String>,
    /// REST API TCP port. Default: 9800. Set to 0 to disable.
    #[serde(default = "default_api_port")]
    pub api_port: u16,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            socket: default_socket(),
            image_dir: default_image_dir(),
            kernel: None,
            tap_device: None,
            api_port: default_api_port(),
        }
    }
}

/// Sensor / eBPF configuration.
#[derive(Debug, Deserialize)]
pub struct SensorConfig {
    /// Path to the JSONL event log file.
    #[serde(default = "default_events_log")]
    pub events_log: String,
    /// Directory containing compiled eBPF bytecode files.
    #[serde(default = "default_ebpf_dir")]
    pub ebpf_dir: String,
    /// Guest-side eBPF agent injection settings.
    #[serde(default)]
    pub guest: GuestSensorConfig,
    /// Probe specifications (reuses `ProbeSpec` from `nova-eye`).
    #[serde(default)]
    pub probes: Vec<ProbeSpec>,
}

impl Default for SensorConfig {
    fn default() -> Self {
        Self {
            events_log: default_events_log(),
            ebpf_dir: default_ebpf_dir(),
            guest: GuestSensorConfig::default(),
            probes: Vec::new(),
        }
    }
}

/// Guest-side eBPF agent injection config.
#[derive(Debug, Clone, Deserialize)]
pub struct GuestSensorConfig {
    /// Whether to inject the eBPF agent into the guest initrd.
    #[serde(default)]
    pub enabled: bool,
    /// Path to the pre-built `nova-eye-agent` static binary on the host.
    #[serde(default = "default_agent_path")]
    pub agent_path: String,
    /// UDP port the agent sends events to on the host gateway.
    #[serde(default = "default_event_port")]
    pub event_port: u16,
}

impl Default for GuestSensorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            agent_path: default_agent_path(),
            event_port: default_event_port(),
        }
    }
}

impl DaemonConfig {
    /// Parse from a TOML string.
    pub fn from_toml(toml_str: &str) -> anyhow::Result<Self> {
        let config: DaemonConfig = toml::from_str(toml_str)?;
        Ok(config)
    }

    /// Parse from a TOML file on disk.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_toml(&content)
    }

    /// Build a config purely from defaults (no file needed).
    pub fn defaults() -> Self {
        Self {
            daemon: DaemonSettings::default(),
            sensor: SensorConfig::default(),
            policy: PolicyConfig::default(),
        }
    }

    /// Helper: kernel path as `Option<PathBuf>`.
    pub fn kernel_path(&self) -> Option<PathBuf> {
        self.daemon.kernel.as_deref().map(PathBuf::from)
    }

    /// Helper: socket as `PathBuf`.
    pub fn socket_path(&self) -> PathBuf {
        PathBuf::from(&self.daemon.socket)
    }

    /// Helper: image_dir as `PathBuf`.
    pub fn image_dir(&self) -> PathBuf {
        PathBuf::from(&self.daemon.image_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal() {
        let cfg = DaemonConfig::from_toml("").unwrap();
        assert_eq!(cfg.daemon.socket, "/run/nova/nova.sock");
        assert!(cfg.daemon.kernel.is_none());
        assert!(cfg.sensor.probes.is_empty());
    }

    #[test]
    fn test_parse_full() {
        let toml = r#"
[daemon]
socket = "/tmp/nova.sock"
image_dir = "/tmp/images"
kernel = "/boot/vmlinux"

[sensor]
events_log = "/tmp/events.jsonl"
ebpf_dir = "/tmp/ebpf"

[sensor.guest]
enabled = true
agent_path = "/tmp/agent"
event_port = 1234

[[sensor.probes]]
hook_type = "tracepoint"
target = "sched/sched_process_exec"
bytecode = "nova-eye-process"

[[sensor.probes]]
hook_type = "kprobe"
target = "vfs_open"
"#;
        let cfg = DaemonConfig::from_toml(toml).unwrap();
        assert_eq!(cfg.daemon.socket, "/tmp/nova.sock");
        assert_eq!(cfg.daemon.kernel.as_deref(), Some("/boot/vmlinux"));
        assert_eq!(cfg.sensor.guest.event_port, 1234);
        assert!(cfg.sensor.guest.enabled);
        assert_eq!(cfg.sensor.probes.len(), 2);
        assert_eq!(cfg.sensor.probes[0].hook_type, "tracepoint");
    }
}
