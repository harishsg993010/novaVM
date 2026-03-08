//! Host-side network configuration for VM TAP devices.
//!
//! Sets up IP addresses, routing, and NAT (masquerade) so the guest can
//! communicate with the host and the internet.

use std::net::Ipv4Addr;
use std::process::Command;

/// Host-side network configuration for a VM's TAP interface.
pub struct NetworkSetup {
    /// Name of the TAP interface.
    pub tap_name: String,
    /// Host IP address on the TAP interface.
    pub host_ip: Ipv4Addr,
    /// Guest IP address (for documentation/init script generation).
    pub guest_ip: Ipv4Addr,
    /// Subnet mask prefix length.
    pub netmask: u8,
    /// Host interface for NAT masquerade (e.g., "eth0").
    pub host_iface: String,
    /// Whether setup has been applied (for teardown tracking).
    applied: bool,
}

impl NetworkSetup {
    /// Create a new network configuration.
    pub fn new(
        tap_name: String,
        host_ip: Ipv4Addr,
        guest_ip: Ipv4Addr,
        netmask: u8,
        host_iface: String,
    ) -> Self {
        Self {
            tap_name,
            host_ip,
            guest_ip,
            netmask,
            host_iface,
            applied: false,
        }
    }

    /// Create a default configuration with standard addresses.
    pub fn default_for_tap(tap_name: &str) -> Self {
        Self::new(
            tap_name.to_string(),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(172, 16, 0, 2),
            30,
            "eth0".to_string(),
        )
    }

    /// Apply the host-side network configuration.
    ///
    /// Requires root/CAP_NET_ADMIN. Runs:
    /// 1. `ip addr add <host_ip>/<netmask> dev <tap>`
    /// 2. `ip link set <tap> up`
    /// 3. `sysctl net.ipv4.ip_forward=1`
    /// 4. `iptables -t nat -A POSTROUTING -s <subnet> -o <host_iface> -j MASQUERADE`
    pub fn setup(&mut self) -> std::io::Result<()> {
        let subnet = format!("{}/{}", self.host_ip, self.netmask);
        let nat_subnet = format!(
            "{}/{}",
            self.network_addr(),
            self.netmask
        );

        // Add IP address to TAP interface (ignore "already assigned" errors).
        let _ = run_cmd("ip", &["addr", "add", &subnet, "dev", &self.tap_name]);

        // Bring TAP interface up.
        run_cmd("ip", &["link", "set", &self.tap_name, "up"])?;

        // Enable IP forwarding.
        run_cmd("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;

        // Add NAT masquerade rule (check first to avoid duplicates).
        let check = run_cmd(
            "iptables",
            &[
                "-t", "nat",
                "-C", "POSTROUTING",
                "-s", &nat_subnet,
                "-o", &self.host_iface,
                "-j", "MASQUERADE",
            ],
        );
        if check.is_err() {
            run_cmd(
                "iptables",
                &[
                    "-t", "nat",
                    "-A", "POSTROUTING",
                    "-s", &nat_subnet,
                    "-o", &self.host_iface,
                    "-j", "MASQUERADE",
                ],
            )?;
        }

        self.applied = true;
        tracing::info!(
            tap = %self.tap_name,
            host_ip = %self.host_ip,
            guest_ip = %self.guest_ip,
            "host network configured"
        );
        Ok(())
    }

    /// Tear down the host-side network configuration.
    ///
    /// Reverses the setup: removes iptables rule and IP address.
    pub fn teardown(&mut self) -> std::io::Result<()> {
        if !self.applied {
            return Ok(());
        }

        let nat_subnet = format!(
            "{}/{}",
            self.network_addr(),
            self.netmask
        );

        // Remove NAT masquerade rule (ignore errors if rule doesn't exist).
        let _ = run_cmd(
            "iptables",
            &[
                "-t", "nat",
                "-D", "POSTROUTING",
                "-s", &nat_subnet,
                "-o", &self.host_iface,
                "-j", "MASQUERADE",
            ],
        );

        // Remove IP from TAP (ignore errors).
        let subnet = format!("{}/{}", self.host_ip, self.netmask);
        let _ = run_cmd("ip", &["addr", "del", &subnet, "dev", &self.tap_name]);

        self.applied = false;
        tracing::info!(tap = %self.tap_name, "host network torn down");
        Ok(())
    }

    /// Generate a guest init script that configures networking inside the VM.
    ///
    /// When `inject_eye_agent` is true, the script starts `/sbin/nova-eye-agent`
    /// in the background before running the entrypoint.
    pub fn guest_init_script(&self, entrypoint: &str) -> String {
        self.guest_init_script_with_agent(entrypoint, false)
    }

    /// Generate a guest init script with optional nova-eye-agent injection.
    pub fn guest_init_script_with_agent(&self, entrypoint: &str, inject_eye_agent: bool) -> String {
        let agent_block = if inject_eye_agent {
            r#"
# Verify UDP works from guest to host
echo "NOVA-INIT-UDP-TEST" > /dev/udp/172.16.0.1/9876 2>/dev/null || true
echo "NovaVM: UDP test sent" >&2

# Start nova-eye eBPF telemetry agent
if [ -x /sbin/nova-eye-agent ]; then
    /sbin/nova-eye-agent &
    sleep 1
    # Send another UDP test after agent starts
    echo "NOVA-INIT-UDP-TEST2" > /dev/udp/172.16.0.1/9876 2>/dev/null || true
fi
"#
        } else {
            ""
        };

        format!(
            r#"#!/bin/sh
# NovaVM network init
ip addr add {guest_ip}/{netmask} dev eth0
ip link set eth0 up
ip route add default via {host_ip}

# DNS (use Google DNS)
echo "nameserver 8.8.8.8" > /etc/resolv.conf
{agent_block}
# Execute the entrypoint
exec {entrypoint}
"#,
            guest_ip = self.guest_ip,
            netmask = self.netmask,
            host_ip = self.host_ip,
            agent_block = agent_block,
            entrypoint = entrypoint,
        )
    }

    /// Compute the network address from host_ip and netmask.
    fn network_addr(&self) -> Ipv4Addr {
        let ip = u32::from(self.host_ip);
        let mask = if self.netmask >= 32 {
            0xFFFF_FFFFu32
        } else {
            !((1u32 << (32 - self.netmask)) - 1)
        };
        Ipv4Addr::from(ip & mask)
    }
}

impl Drop for NetworkSetup {
    fn drop(&mut self) {
        if self.applied {
            let _ = self.teardown();
        }
    }
}

/// Run a command, returning Ok(()) on success or Err on failure.
fn run_cmd(cmd: &str, args: &[&str]) -> std::io::Result<()> {
    tracing::debug!(cmd = cmd, args = ?args, "running command");
    let output = Command::new(cmd).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(cmd = cmd, stderr = %stderr, "command failed");
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("{} failed: {}", cmd, stderr.trim()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_addr_calculation() {
        let setup = NetworkSetup::new(
            "nova-tap0".to_string(),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(172, 16, 0, 2),
            30,
            "eth0".to_string(),
        );
        assert_eq!(setup.network_addr(), Ipv4Addr::new(172, 16, 0, 0));
    }

    #[test]
    fn test_guest_init_script_generation() {
        let setup = NetworkSetup::default_for_tap("nova-tap0");
        let script = setup.guest_init_script("/sbin/init");
        assert!(script.contains("172.16.0.2/30"));
        assert!(script.contains("172.16.0.1"));
        assert!(script.contains("/sbin/init"));
        assert!(script.contains("ip link set eth0 up"));
    }

    #[test]
    fn test_default_for_tap() {
        let setup = NetworkSetup::default_for_tap("test-tap0");
        assert_eq!(setup.tap_name, "test-tap0");
        assert_eq!(setup.host_ip, Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(setup.guest_ip, Ipv4Addr::new(172, 16, 0, 2));
        assert_eq!(setup.netmask, 30);
    }
}
