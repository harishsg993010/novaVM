//! VM configuration deserialization from TOML.

use serde::Deserialize;
use std::path::PathBuf;

/// Top-level VM configuration.
#[derive(Debug, Deserialize)]
pub struct VmConfig {
    /// Number of vCPUs.
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,

    /// Memory size in MiB.
    #[serde(default = "default_memory_mib")]
    pub memory_mib: u64,

    /// Kernel configuration.
    pub kernel: KernelConfig,

    /// Block devices.
    #[serde(default)]
    pub block: Vec<BlockConfig>,

    /// Network configuration.
    pub network: Option<NetworkConfig>,

    /// Console configuration.
    #[serde(default)]
    pub console: ConsoleConfig,

    /// Vsock configuration.
    pub vsock: Option<VsockConfig>,
}

/// Kernel loading configuration.
#[derive(Debug, Deserialize)]
pub struct KernelConfig {
    /// Path to the kernel image (bzImage or vmlinux).
    pub path: PathBuf,

    /// Kernel command line.
    #[serde(default = "default_cmdline")]
    pub cmdline: String,

    /// Path to initrd/initramfs (optional).
    pub initrd: Option<PathBuf>,

    /// Boot method: "bzimage", "elf", or "pvh".
    #[serde(default = "default_boot_method")]
    pub boot_method: String,
}

/// Block device configuration.
#[derive(Debug, Deserialize)]
pub struct BlockConfig {
    /// Path to the disk image.
    pub path: PathBuf,

    /// Read-only flag.
    #[serde(default)]
    pub read_only: bool,
}

/// Network configuration.
#[derive(Debug, Deserialize)]
pub struct NetworkConfig {
    /// TAP device name.
    #[serde(default = "default_tap_name")]
    pub tap: String,

    /// MAC address (as string "xx:xx:xx:xx:xx:xx").
    pub mac: Option<String>,
}

/// Console configuration.
#[derive(Debug, Deserialize, Default)]
pub struct ConsoleConfig {
    /// Console mode: "serial" or "virtio".
    #[serde(default = "default_console_mode")]
    pub mode: String,
}

/// Vsock configuration.
#[derive(Debug, Deserialize)]
pub struct VsockConfig {
    /// Guest CID.
    pub guest_cid: u64,
}

fn default_vcpus() -> u32 {
    1
}

fn default_memory_mib() -> u64 {
    256
}

fn default_cmdline() -> String {
    "console=ttyS0 reboot=k panic=1 pci=off".to_string()
}

fn default_boot_method() -> String {
    "bzimage".to_string()
}

fn default_tap_name() -> String {
    "nova-tap0".to_string()
}

fn default_console_mode() -> String {
    "serial".to_string()
}

impl VmConfig {
    /// Parse a VmConfig from a TOML string.
    pub fn from_toml(toml_str: &str) -> anyhow::Result<Self> {
        let config: VmConfig = toml::from_str(toml_str)?;
        Ok(config)
    }

    /// Parse from a TOML file.
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_toml(&content)
    }

    /// Returns the memory size in bytes.
    pub fn memory_bytes(&self) -> u64 {
        self.memory_mib * 1024 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
            [kernel]
            path = "/boot/vmlinuz"
        "#;
        let config = VmConfig::from_toml(toml).unwrap();
        assert_eq!(config.vcpus, 1);
        assert_eq!(config.memory_mib, 256);
        assert_eq!(config.kernel.path, PathBuf::from("/boot/vmlinuz"));
        assert_eq!(config.kernel.boot_method, "bzimage");
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
            vcpus = 4
            memory_mib = 1024

            [kernel]
            path = "/boot/vmlinuz"
            cmdline = "console=ttyS0 root=/dev/vda ro"
            initrd = "/boot/initrd.img"
            boot_method = "bzimage"

            [[block]]
            path = "/var/images/rootfs.ext4"
            read_only = false

            [[block]]
            path = "/var/images/data.qcow2"
            read_only = true

            [network]
            tap = "nova-tap0"
            mac = "52:54:00:12:34:56"

            [vsock]
            guest_cid = 3
        "#;
        let config = VmConfig::from_toml(toml).unwrap();
        assert_eq!(config.vcpus, 4);
        assert_eq!(config.memory_mib, 1024);
        assert_eq!(config.block.len(), 2);
        assert!(config.network.is_some());
        assert!(config.vsock.is_some());
        assert_eq!(config.vsock.unwrap().guest_cid, 3);
    }
}
