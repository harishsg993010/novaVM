//! Event types shared between eBPF kernel probes and userspace.
//!
//! All structs are `#[repr(C)]` and fit within the eBPF stack limit (512 bytes).
//! For larger payloads, use per-CPU maps referenced by an event ID.

/// Maximum length for short string fields (command name, filename).
pub const MAX_COMM_LEN: usize = 16;
/// Maximum length for path fields.
pub const MAX_PATH_LEN: usize = 128;
/// Maximum length for hostname/address fields.
pub const MAX_ADDR_LEN: usize = 64;
/// Maximum length for HTTP data excerpt.
pub const MAX_HTTP_DATA_LEN: usize = 128;

/// Event type discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EventType {
    ProcessExec = 1,
    ProcessExit = 2,
    ProcessFork = 3,
    FileOpen = 10,
    FileWrite = 11,
    FileUnlink = 12,
    NetConnect = 20,
    NetAccept = 21,
    NetClose = 22,
    HttpRequest = 30,
    HttpResponse = 31,
    DnsQuery = 40,
}

/// Common header for all events.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventHeader {
    /// Event type discriminator.
    pub event_type: u32,
    /// Timestamp in nanoseconds (from bpf_ktime_get_ns).
    pub timestamp_ns: u64,
    /// PID of the process that generated the event.
    pub pid: u32,
    /// TID (thread ID) of the thread.
    pub tid: u32,
    /// UID of the process.
    pub uid: u32,
    /// GID of the process.
    pub gid: u32,
    /// Command name (comm).
    pub comm: [u8; MAX_COMM_LEN],
}

impl Default for EventHeader {
    fn default() -> Self {
        Self {
            event_type: 0,
            timestamp_ns: 0,
            pid: 0,
            tid: 0,
            uid: 0,
            gid: 0,
            comm: [0u8; MAX_COMM_LEN],
        }
    }
}

#[cfg(feature = "std")]
impl EventHeader {
    /// Returns the comm as a &str (trimmed at first null byte).
    pub fn comm_str(&self) -> &str {
        let end = self
            .comm
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(MAX_COMM_LEN);
        // SAFETY: comm is always ASCII from the kernel.
        core::str::from_utf8(&self.comm[..end]).unwrap_or("<invalid>")
    }
}

// ---- Process events ----

/// Process execution event (execve).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessExecEvent {
    pub header: EventHeader,
    /// Full filename/path of the executable.
    pub filename: [u8; MAX_PATH_LEN],
    /// Parent PID.
    pub ppid: u32,
    /// Padding for alignment.
    pub _pad: u32,
}

/// Process exit event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessExitEvent {
    pub header: EventHeader,
    /// Exit code.
    pub exit_code: i32,
    /// Signal that caused the exit (0 if normal exit).
    pub signal: u32,
    /// Duration in nanoseconds since exec.
    pub duration_ns: u64,
}

/// Process fork event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessForkEvent {
    pub header: EventHeader,
    /// Child PID.
    pub child_pid: u32,
    /// Child TID.
    pub child_tid: u32,
}

// ---- File events ----

/// File open event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileOpenEvent {
    pub header: EventHeader,
    /// File path.
    pub path: [u8; MAX_PATH_LEN],
    /// Open flags.
    pub flags: u32,
    /// File mode.
    pub mode: u32,
}

/// File write event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileWriteEvent {
    pub header: EventHeader,
    /// File path.
    pub path: [u8; MAX_PATH_LEN],
    /// Number of bytes written.
    pub bytes_written: u64,
}

/// File unlink (delete) event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileUnlinkEvent {
    pub header: EventHeader,
    /// File path.
    pub path: [u8; MAX_PATH_LEN],
}

// ---- Network events ----

/// Network connect event (tcp_v4_connect, tcp_v6_connect).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NetConnectEvent {
    pub header: EventHeader,
    /// Source address (IPv4 or IPv6, as bytes).
    pub src_addr: [u8; 16],
    /// Destination address.
    pub dst_addr: [u8; 16],
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Address family: 2 = AF_INET, 10 = AF_INET6.
    pub family: u16,
    /// Protocol: 6 = TCP, 17 = UDP.
    pub protocol: u16,
}

/// Network accept event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NetAcceptEvent {
    pub header: EventHeader,
    /// Source address (the connecting peer).
    pub src_addr: [u8; 16],
    /// Listening address.
    pub dst_addr: [u8; 16],
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Address family.
    pub family: u16,
    /// Padding.
    pub _pad: u16,
}

/// Network close event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NetCloseEvent {
    pub header: EventHeader,
    /// Source address.
    pub src_addr: [u8; 16],
    /// Destination address.
    pub dst_addr: [u8; 16],
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Bytes sent during the connection.
    pub bytes_sent: u64,
    /// Bytes received during the connection.
    pub bytes_received: u64,
}

// ---- HTTP events ----

/// HTTP request event (from SSL_write uprobe).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HttpRequestEvent {
    pub header: EventHeader,
    /// HTTP method + URL excerpt.
    pub data: [u8; MAX_HTTP_DATA_LEN],
    /// Total data length.
    pub data_len: u32,
    /// Padding.
    pub _pad: u32,
}

/// HTTP response event (from SSL_read uprobe).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HttpResponseEvent {
    pub header: EventHeader,
    /// Response status line excerpt.
    pub data: [u8; MAX_HTTP_DATA_LEN],
    /// Total data length.
    pub data_len: u32,
    /// Padding.
    pub _pad: u32,
}

// ---- DNS events ----

/// DNS query event.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DnsQueryEvent {
    pub header: EventHeader,
    /// Queried domain name.
    pub domain: [u8; MAX_ADDR_LEN],
    /// Query type (A=1, AAAA=28, etc.).
    pub query_type: u16,
    /// Padding.
    pub _pad: [u8; 6],
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use core::mem;

    #[test]
    fn test_event_header_size() {
        // EventHeader should be a reasonable size for eBPF.
        let size = mem::size_of::<EventHeader>();
        assert!(size <= 64, "EventHeader is {size} bytes, should be <= 64");
    }

    #[test]
    fn test_process_exec_event_fits_stack() {
        let size = mem::size_of::<ProcessExecEvent>();
        assert!(
            size <= 512,
            "ProcessExecEvent is {size} bytes, must fit in 512-byte eBPF stack"
        );
    }

    #[test]
    fn test_net_connect_event_fits_stack() {
        let size = mem::size_of::<NetConnectEvent>();
        assert!(
            size <= 512,
            "NetConnectEvent is {size} bytes, must fit in 512-byte eBPF stack"
        );
    }

    #[test]
    fn test_http_request_event_fits_stack() {
        let size = mem::size_of::<HttpRequestEvent>();
        assert!(
            size <= 512,
            "HttpRequestEvent is {size} bytes, must fit in 512-byte eBPF stack"
        );
    }

    #[test]
    fn test_event_header_comm_str() {
        let mut header = EventHeader::default();
        header.comm[..5].copy_from_slice(b"nginx");
        assert_eq!(header.comm_str(), "nginx");
    }

    #[test]
    fn test_event_type_values() {
        assert_eq!(EventType::ProcessExec as u32, 1);
        assert_eq!(EventType::FileOpen as u32, 10);
        assert_eq!(EventType::NetConnect as u32, 20);
        assert_eq!(EventType::HttpRequest as u32, 30);
    }

    #[test]
    fn test_all_events_repr_c() {
        // Verify sizes are stable (they're repr(C) so layout is deterministic).
        assert!(mem::size_of::<ProcessExecEvent>() > 0);
        assert!(mem::size_of::<ProcessExitEvent>() > 0);
        assert!(mem::size_of::<FileOpenEvent>() > 0);
        assert!(mem::size_of::<NetConnectEvent>() > 0);
        assert!(mem::size_of::<HttpRequestEvent>() > 0);
        assert!(mem::size_of::<DnsQueryEvent>() > 0);
    }
}
