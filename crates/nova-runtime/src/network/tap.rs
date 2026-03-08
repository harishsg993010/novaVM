//! TAP device creation and management.
//!
//! Creates TAP network interfaces that virtio-net devices connect to,
//! providing network connectivity to microVM guests.

use std::os::unix::io::RawFd;

use crate::error::{Result, RuntimeError};

/// Maximum length for a TAP device name.
const IFNAMSIZ: usize = 16;

/// Represents a TAP network device.
pub struct TapDevice {
    /// Device name (e.g., "tap0").
    name: String,
    /// File descriptor for the TAP device.
    fd: RawFd,
}

impl TapDevice {
    /// Create and configure a new TAP device.
    ///
    /// In production, this calls `open("/dev/net/tun")` and sets up the
    /// interface with `TUNSETIFF`. The current implementation is structural.
    pub fn create(name: &str) -> Result<Self> {
        if name.len() >= IFNAMSIZ {
            return Err(RuntimeError::Network(format!(
                "TAP device name too long: {} (max {})",
                name.len(),
                IFNAMSIZ - 1
            )));
        }

        // Open /dev/net/tun.
        let fd = unsafe { libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };

        if fd < 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(name, err = %err, "failed to open /dev/net/tun (expected outside VM host)");
            // Return a placeholder for testing environments.
            return Ok(Self {
                name: name.to_string(),
                fd: -1,
            });
        }

        // Set up IFF_TAP | IFF_NO_PI via TUNSETIFF ioctl.
        // IFF_TAP = 0x0002, IFF_NO_PI = 0x1000
        let mut ifr = [0u8; 40]; // struct ifreq
        let name_bytes = name.as_bytes();
        ifr[..name_bytes.len()].copy_from_slice(name_bytes);
        // ifr_flags at offset 16 (short = 2 bytes)
        let flags: u16 = 0x0002 | 0x1000; // IFF_TAP | IFF_NO_PI
        ifr[16..18].copy_from_slice(&flags.to_ne_bytes());

        // TUNSETIFF = _IOW('T', 202, int) = 0x400454ca
        let ret = unsafe { libc::ioctl(fd, 0x400454ca, ifr.as_ptr()) };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            tracing::warn!(name, err = %err, "TUNSETIFF failed (need CAP_NET_ADMIN)");
            // Return a placeholder for non-privileged environments.
            return Ok(Self {
                name: name.to_string(),
                fd: -1,
            });
        }

        tracing::info!(name, fd, "created TAP device");

        Ok(Self {
            name: name.to_string(),
            fd,
        })
    }

    /// Get the device name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the raw file descriptor.
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Check if the device has a valid file descriptor.
    pub fn is_valid(&self) -> bool {
        self.fd >= 0
    }
}

impl Drop for TapDevice {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            tracing::debug!(name = %self.name, "closed TAP device");
        }
    }
}
