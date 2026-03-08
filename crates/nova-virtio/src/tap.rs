//! TAP device management for virtio-net.
//!
//! Opens and configures Linux TAP devices via /dev/net/tun.

use std::os::unix::io::RawFd;

/// A TAP device handle.
pub struct Tap {
    /// File descriptor for the TAP device.
    fd: RawFd,
    /// Name of the TAP interface.
    name: String,
}

/// IFF_TAP flag for tun/tap.
const IFF_TAP: libc::c_short = 0x0002;
/// IFF_NO_PI: don't prepend packet info header.
const IFF_NO_PI: libc::c_short = 0x1000;
/// TUNSETIFF ioctl number.
const TUNSETIFF: libc::c_ulong = 0x400454CA;

/// ifreq structure for TAP ioctl.
#[repr(C)]
struct Ifreq {
    ifr_name: [u8; libc::IFNAMSIZ],
    ifr_flags: libc::c_short,
    _pad: [u8; 22], // Padding to match struct size
}

impl Tap {
    /// Open a TAP device with the given name.
    ///
    /// Requires CAP_NET_ADMIN or root privileges.
    pub fn open(name: &str) -> std::io::Result<Self> {
        // Open /dev/net/tun.
        let fd = unsafe {
            libc::open(
                b"/dev/net/tun\0".as_ptr() as *const libc::c_char,
                libc::O_RDWR | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Prepare ifreq.
        let mut ifr = Ifreq {
            ifr_name: [0u8; libc::IFNAMSIZ],
            ifr_flags: IFF_TAP | IFF_NO_PI,
            _pad: [0u8; 22],
        };
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
        ifr.ifr_name[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

        // TUNSETIFF ioctl to create/attach the TAP device.
        let ret = unsafe { libc::ioctl(fd, TUNSETIFF as libc::c_ulong, &ifr as *const Ifreq) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }

        // Read back the actual interface name.
        let actual_name = {
            let end = ifr.ifr_name.iter().position(|&b| b == 0).unwrap_or(libc::IFNAMSIZ);
            String::from_utf8_lossy(&ifr.ifr_name[..end]).to_string()
        };

        Ok(Tap {
            fd,
            name: actual_name,
        })
    }

    /// Set the TAP fd to non-blocking mode.
    pub fn set_nonblocking(&self) -> std::io::Result<()> {
        let flags = unsafe { libc::fcntl(self.fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ret = unsafe { libc::fcntl(self.fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Returns the raw file descriptor.
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Returns the TAP interface name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
    }
}
