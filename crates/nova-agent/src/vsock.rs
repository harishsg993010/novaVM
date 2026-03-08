//! AF_VSOCK client for host-guest communication.
//!
//! The guest agent connects back to the host VMM over a vsock socket.
//! This module provides a client that can send and receive length-prefixed
//! JSON messages over the vsock transport.

use std::os::unix::io::{AsRawFd, RawFd};

use crate::error::{AgentError, Result};
use crate::protocol::{Request, Response};

/// The vsock CID for the host (always 2 per the vsock spec).
pub const HOST_CID: u32 = 2;

/// Default vsock port the agent connects to.
pub const DEFAULT_PORT: u32 = 1024;

/// A vsock connection to the host VMM.
pub struct VsockClient {
    /// The raw socket file descriptor.
    fd: RawFd,
    /// Whether the socket is connected.
    connected: bool,
}

impl VsockClient {
    /// Create a new vsock client.
    ///
    /// This creates the AF_VSOCK socket but does not connect yet.
    pub fn new() -> Result<Self> {
        // AF_VSOCK = 40, SOCK_STREAM = 1
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(AgentError::Vsock(format!(
                "failed to create vsock socket: {}",
                std::io::Error::last_os_error()
            )));
        }

        tracing::debug!(fd, "created vsock socket");
        Ok(Self {
            fd,
            connected: false,
        })
    }

    /// Connect to the host at the given CID and port.
    pub fn connect(&mut self, cid: u32, port: u32) -> Result<()> {
        let addr = libc::sockaddr_vm {
            svm_family: libc::AF_VSOCK as u16,
            svm_reserved1: 0,
            svm_port: port,
            svm_cid: cid,
            svm_zero: [0u8; 4],
        };

        let ret = unsafe {
            libc::connect(
                self.fd,
                &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };

        if ret < 0 {
            return Err(AgentError::Vsock(format!(
                "failed to connect to cid={} port={}: {}",
                cid,
                port,
                std::io::Error::last_os_error()
            )));
        }

        self.connected = true;
        tracing::info!(cid, port, "connected to host via vsock");
        Ok(())
    }

    /// Send a response message to the host.
    ///
    /// Messages are length-prefixed: 4 bytes big-endian length, then JSON.
    pub fn send_response(&self, response: &Response) -> Result<()> {
        let json = serde_json::to_vec(response)?;
        let len = (json.len() as u32).to_be_bytes();

        self.write_all(&len)?;
        self.write_all(&json)?;

        tracing::trace!(len = json.len(), "sent response");
        Ok(())
    }

    /// Receive a request message from the host.
    ///
    /// Blocks until a complete message is received.
    pub fn recv_request(&self) -> Result<Request> {
        let mut len_buf = [0u8; 4];
        self.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > 1024 * 1024 {
            return Err(AgentError::Vsock(format!("message too large: {len} bytes")));
        }

        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;

        let request: Request = serde_json::from_slice(&buf)?;
        tracing::trace!(len, "received request");
        Ok(request)
    }

    /// Returns whether the client is currently connected.
    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut written = 0;
        while written < data.len() {
            let n = unsafe {
                libc::write(
                    self.fd,
                    data[written..].as_ptr() as *const libc::c_void,
                    data.len() - written,
                )
            };
            if n < 0 {
                return Err(AgentError::Io(std::io::Error::last_os_error()));
            }
            written += n as usize;
        }
        Ok(())
    }

    fn read_exact(&self, buf: &mut [u8]) -> Result<()> {
        let mut read = 0;
        while read < buf.len() {
            let n = unsafe {
                libc::read(
                    self.fd,
                    buf[read..].as_mut_ptr() as *mut libc::c_void,
                    buf.len() - read,
                )
            };
            if n <= 0 {
                if n == 0 {
                    return Err(AgentError::Vsock("connection closed".to_string()));
                }
                return Err(AgentError::Io(std::io::Error::last_os_error()));
            }
            read += n as usize;
        }
        Ok(())
    }
}

impl AsRawFd for VsockClient {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for VsockClient {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
        tracing::debug!(fd = self.fd, "closed vsock socket");
    }
}

/// A mock vsock client for testing without actual AF_VSOCK support.
///
/// Uses an in-memory buffer pair to simulate the host<->guest channel.
#[cfg(test)]
pub struct MockVsockClient {
    /// Requests queued for the agent to receive.
    incoming: Vec<Request>,
    /// Responses sent by the agent.
    outgoing: Vec<Response>,
}

#[cfg(test)]
impl MockVsockClient {
    /// Create a new mock client with the given incoming requests.
    pub fn new(incoming: Vec<Request>) -> Self {
        Self {
            incoming,
            outgoing: Vec::new(),
        }
    }

    /// Receive the next request (pops from the front of the queue).
    pub fn recv_request(&mut self) -> Result<Request> {
        if self.incoming.is_empty() {
            return Err(AgentError::Vsock("no more requests".to_string()));
        }
        Ok(self.incoming.remove(0))
    }

    /// Send a response (pushes to the outgoing buffer).
    pub fn send_response(&mut self, response: Response) -> Result<()> {
        self.outgoing.push(response);
        Ok(())
    }

    /// Returns all responses sent by the agent.
    #[allow(dead_code)]
    pub fn responses(&self) -> &[Response] {
        &self.outgoing
    }
}
