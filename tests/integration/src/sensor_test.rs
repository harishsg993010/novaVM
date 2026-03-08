//! Sensor pipeline integration tests.
//!
//! Tests the observability pipeline: event types -> aggregation -> sink output.

use nova_eye::{EventAggregator, EventHandler, FileSink, StdoutSink, TelemetrySink};
use nova_eye_common::{
    EventHeader, EventType, FileOpenEvent, NetConnectEvent, ProcessExecEvent, MAX_COMM_LEN,
    MAX_PATH_LEN,
};

/// Helper: build a ProcessExecEvent with the given command name.
fn make_exec_event(comm: &str, pid: u32, ppid: u32) -> ProcessExecEvent {
    let mut header = EventHeader {
        event_type: EventType::ProcessExec as u32,
        timestamp_ns: 1_000_000_000,
        pid,
        tid: pid,
        uid: 1000,
        gid: 1000,
        comm: [0u8; MAX_COMM_LEN],
    };
    let bytes = comm.as_bytes();
    let len = bytes.len().min(MAX_COMM_LEN - 1);
    header.comm[..len].copy_from_slice(&bytes[..len]);

    ProcessExecEvent {
        header,
        filename: [0u8; MAX_PATH_LEN],
        ppid,
        _pad: 0,
    }
}

/// Helper: build a FileOpenEvent.
fn make_file_open_event(pid: u32, file_path: &str) -> FileOpenEvent {
    let header = EventHeader {
        event_type: EventType::FileOpen as u32,
        timestamp_ns: 2_000_000_000,
        pid,
        tid: pid,
        uid: 1000,
        gid: 1000,
        comm: [0u8; MAX_COMM_LEN],
    };

    let mut path = [0u8; MAX_PATH_LEN];
    let bytes = file_path.as_bytes();
    let len = bytes.len().min(MAX_PATH_LEN - 1);
    path[..len].copy_from_slice(&bytes[..len]);

    FileOpenEvent {
        header,
        path,
        flags: 0,
        mode: 0o644,
    }
}

/// Simple event handler that counts events by type.
struct CountingHandler {
    exec_count: usize,
    file_count: usize,
    other_count: usize,
}

impl CountingHandler {
    fn new() -> Self {
        Self {
            exec_count: 0,
            file_count: 0,
            other_count: 0,
        }
    }
}

impl EventHandler for CountingHandler {
    fn handle_event(&mut self, header: &EventHeader, _raw: &[u8], _source_name: &str) {
        match header.event_type {
            x if x == EventType::ProcessExec as u32 => self.exec_count += 1,
            x if x == EventType::FileOpen as u32 => self.file_count += 1,
            _ => self.other_count += 1,
        }
    }
}

/// Test that the event aggregator dispatches ProcessExec events to a handler.
#[test]
fn test_aggregator_dispatches_process_events() {
    let mut aggregator = EventAggregator::new();

    let event = make_exec_event("nginx", 100, 1);
    let raw_bytes = unsafe {
        std::slice::from_raw_parts(
            &event as *const _ as *const u8,
            std::mem::size_of::<ProcessExecEvent>(),
        )
    };

    aggregator.push_event(event.header, raw_bytes.to_vec(), String::new());
    assert_eq!(aggregator.pending_count(), 1);

    let mut handler = CountingHandler::new();
    aggregator.process_events(&mut handler).unwrap();

    assert_eq!(handler.exec_count, 1);
    assert_eq!(aggregator.pending_count(), 0);
}

/// Test that the aggregator dispatches FileOpen events.
#[test]
fn test_aggregator_dispatches_file_events() {
    let mut aggregator = EventAggregator::new();

    let event = make_file_open_event(200, "/etc/passwd");
    let raw_bytes = unsafe {
        std::slice::from_raw_parts(
            &event as *const _ as *const u8,
            std::mem::size_of::<FileOpenEvent>(),
        )
    };

    aggregator.push_event(event.header, raw_bytes.to_vec(), String::new());

    let mut handler = CountingHandler::new();
    aggregator.process_events(&mut handler).unwrap();

    assert_eq!(handler.file_count, 1);
}

/// Test aggregator with multiple events of different types.
#[test]
fn test_aggregator_multiple_event_types() {
    let mut aggregator = EventAggregator::new();

    let exec = make_exec_event("bash", 10, 1);
    let file = make_file_open_event(10, "/tmp/test");

    let exec_bytes = unsafe {
        std::slice::from_raw_parts(
            &exec as *const _ as *const u8,
            std::mem::size_of::<ProcessExecEvent>(),
        )
    };
    let file_bytes = unsafe {
        std::slice::from_raw_parts(
            &file as *const _ as *const u8,
            std::mem::size_of::<FileOpenEvent>(),
        )
    };

    aggregator.push_event(exec.header, exec_bytes.to_vec(), String::new());
    aggregator.push_event(file.header, file_bytes.to_vec(), String::new());
    assert_eq!(aggregator.pending_count(), 2);

    let mut handler = CountingHandler::new();
    aggregator.process_events(&mut handler).unwrap();

    assert_eq!(handler.exec_count, 1);
    assert_eq!(handler.file_count, 1);
    assert_eq!(handler.other_count, 0);
}

/// Test the StdoutSink accepts events.
#[test]
fn test_stdout_sink_send() {
    let mut sink = StdoutSink::new();
    let header = EventHeader {
        event_type: EventType::ProcessExec as u32,
        timestamp_ns: 1_000_000,
        pid: 42,
        tid: 42,
        uid: 0,
        gid: 0,
        comm: [0u8; MAX_COMM_LEN],
    };
    let raw = [0u8; 64];

    sink.send(&header, &raw, "").unwrap();
    assert_eq!(sink.event_count(), 1);
}

/// Test the FileSink writes event lines to a temp file.
#[test]
fn test_file_sink_writes_events() {
    let dir = std::env::temp_dir().join("nova-sensor-integ-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("events.jsonl");

    // Remove any leftover from previous runs.
    let _ = std::fs::remove_file(&path);

    let mut sink = FileSink::new(&path).unwrap();

    let header1 = EventHeader {
        event_type: EventType::ProcessExec as u32,
        timestamp_ns: 1_000_000,
        pid: 1,
        tid: 1,
        uid: 0,
        gid: 0,
        comm: [0u8; MAX_COMM_LEN],
    };
    let header2 = EventHeader {
        event_type: EventType::FileOpen as u32,
        timestamp_ns: 2_000_000,
        pid: 2,
        tid: 2,
        uid: 0,
        gid: 0,
        comm: [0u8; MAX_COMM_LEN],
    };

    sink.send(&header1, &[0u8; 32], "").unwrap();
    sink.send(&header2, &[0u8; 32], "").unwrap();
    assert_eq!(sink.event_count(), 2);

    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2);

    // Cleanup.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

/// Test event header comm_str extracts a null-terminated string.
#[test]
fn test_event_header_comm_str() {
    let mut header = EventHeader::default();
    let name = b"bash";
    header.comm[..name.len()].copy_from_slice(name);
    assert_eq!(header.comm_str(), "bash");
}

/// Test that EventType enum covers the expected variants.
#[test]
fn test_event_type_coverage() {
    let types = [
        EventType::ProcessExec,
        EventType::ProcessExit,
        EventType::ProcessFork,
        EventType::FileOpen,
        EventType::FileWrite,
        EventType::FileUnlink,
        EventType::NetConnect,
        EventType::NetAccept,
        EventType::NetClose,
        EventType::HttpRequest,
        EventType::HttpResponse,
        EventType::DnsQuery,
    ];
    assert_eq!(types.len(), 12);
}

/// Test that NetConnectEvent has expected size (larger than just EventHeader).
#[test]
fn test_net_connect_event_size() {
    let size = std::mem::size_of::<NetConnectEvent>();
    let header_size = std::mem::size_of::<EventHeader>();
    assert!(
        size > header_size,
        "NetConnectEvent ({size}) should be larger than EventHeader ({header_size})"
    );
    assert!(size <= 512, "must fit in eBPF stack");
}
