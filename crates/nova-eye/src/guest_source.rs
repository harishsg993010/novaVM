//! Guest VM event source.
//!
//! [`GuestEventSource`] implements [`SensorSource`] by listening for UDP
//! datagrams from the `nova-eye-agent` running inside a guest VM. Each
//! datagram contains a raw eBPF event (EventHeader + payload), sent from
//! the guest over the TAP interface to the host gateway.

use std::net::UdpSocket;
use std::os::unix::io::FromRawFd;

use nova_eye_common::EventHeader;

use crate::error::Result;
use crate::source::SensorSource;

/// A sensor source that receives eBPF events from a guest VM via UDP.
pub struct GuestEventSource {
    /// UDP socket bound to the host TAP IP (e.g. 172.16.0.1:9876).
    socket: UdpSocket,
    /// Sandbox ID to tag events with.
    sandbox_id: String,
    /// Human-readable name for this source.
    source_name: String,
}

impl GuestEventSource {
    /// Create a new `GuestEventSource` bound to the given address.
    ///
    /// `bind_addr` is typically `"172.16.0.1:9876"`.
    pub fn new(bind_addr: &str, sandbox_id: &str) -> Result<Self> {
        // Create socket with SO_REUSEADDR before binding (prevents EADDRINUSE
        // when the daemon restarts quickly).
        let addr: std::net::SocketAddr = bind_addr.parse()
            .map_err(|e| crate::error::EyeError::IoError(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, e)
            ))?;
        let socket = unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let optval: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &optval as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            let sockaddr = match addr {
                std::net::SocketAddr::V4(v4) => {
                    let mut sa: libc::sockaddr_in = std::mem::zeroed();
                    sa.sin_family = libc::AF_INET as libc::sa_family_t;
                    sa.sin_port = v4.port().to_be();
                    sa.sin_addr.s_addr = u32::from(*v4.ip()).to_be();
                    sa
                }
                _ => {
                    libc::close(fd);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "only IPv4 supported",
                    ).into());
                }
            };
            let ret = libc::bind(
                fd,
                &sockaddr as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            );
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err.into());
            }
            UdpSocket::from_raw_fd(fd)
        };
        socket.set_nonblocking(true)?;

        let source_name = format!("guest:{}", sandbox_id);
        tracing::info!(
            bind = %bind_addr,
            sandbox = %sandbox_id,
            "GuestEventSource listening for VM events"
        );

        Ok(Self {
            socket,
            sandbox_id: sandbox_id.to_string(),
            source_name,
        })
    }

    /// Create from an already-bound UDP socket.
    pub fn from_socket(socket: UdpSocket, sandbox_id: &str) -> Result<Self> {
        socket.set_nonblocking(true)?;
        let source_name = format!("guest:{}", sandbox_id);
        Ok(Self {
            socket,
            sandbox_id: sandbox_id.to_string(),
            source_name,
        })
    }

    /// Returns the sandbox ID this source is associated with.
    pub fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }

    /// Returns the local address the socket is bound to.
    pub fn socket_local_addr(&self) -> std::net::SocketAddr {
        self.socket.local_addr().expect("socket has local addr")
    }
}

impl SensorSource for GuestEventSource {
    fn poll_events(&mut self) -> Result<Vec<(EventHeader, Vec<u8>)>> {
        let mut events = Vec::new();
        let mut buf = [0u8; 4096];
        let header_size = core::mem::size_of::<EventHeader>();

        // Non-blocking recv loop — drain all available datagrams.
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((n, _addr)) => {
                    if n >= header_size {
                        let header: EventHeader =
                            unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const EventHeader) };
                        events.push((header, buf[..n].to_vec()));
                    }
                    // Smaller packets (heartbeats, hello) are silently dropped.
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        if !events.is_empty() {
            tracing::debug!(
                count = events.len(),
                sandbox = %self.sandbox_id,
                "GuestEventSource received events"
            );
        }

        Ok(events)
    }

    fn name(&self) -> &str {
        &self.source_name
    }
}
