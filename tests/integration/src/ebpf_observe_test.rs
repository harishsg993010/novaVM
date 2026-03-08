//! Integration tests for Stage 11: eBPF Observability.
//!
//! Tests cover: TracingPolicy TOML parsing, ChannelSink, sensor pipeline
//! integration, sensor state tracking, event type mapping, event roundtrips,
//! and JSOND event log file output.

use nova_eye::policy::TracingPolicy;
use nova_eye::{ChannelSink, JsondSink, SensorPipeline, SimulatedSource, TelemetrySink};
use nova_eye_common::{EventHeader, EventType, FileOpenEvent, HttpRequestEvent, MAX_PATH_LEN, MAX_HTTP_DATA_LEN};

/// Helper to build a test EventHeader.
fn make_header(event_type: u32, pid: u32, comm: &str) -> EventHeader {
    let mut h = EventHeader {
        event_type,
        pid,
        tid: pid,
        uid: 1000,
        gid: 1000,
        comm: [0u8; 16],
        timestamp_ns: 123_456_789,
    };
    let bytes = comm.as_bytes();
    let len = bytes.len().min(16);
    h.comm[..len].copy_from_slice(&bytes[..len]);
    h
}

/// Serialize an EventHeader to raw bytes.
fn header_to_bytes(h: &EventHeader) -> Vec<u8> {
    let ptr = h as *const EventHeader as *const u8;
    let len = core::mem::size_of::<EventHeader>();
    unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
}

// --------------------------------------------------------------------------
// Test 1: Parse a basic TOML tracing policy
// --------------------------------------------------------------------------

#[test]
fn test_tracing_policy_parse_toml() {
    let toml_str = r#"
name = "file-monitor"

[[probes]]
hook_type = "kprobe"
target = "vfs_open"
bytecode = "/opt/nova/ebpf/nova-eye-file"
"#;

    let policy = TracingPolicy::from_toml(toml_str).expect("parse TOML");
    assert_eq!(policy.name, "file-monitor");
    assert_eq!(policy.probes.len(), 1);
    assert_eq!(policy.probes[0].hook_type, "kprobe");
    assert_eq!(policy.probes[0].target, "vfs_open");
    assert!(policy.probes[0].enabled); // default true
    assert!(policy.filters.is_none());
}

// --------------------------------------------------------------------------
// Test 2: Policy with multiple probes (kprobe + tracepoint)
// --------------------------------------------------------------------------

#[test]
fn test_tracing_policy_multiple_probes() {
    let toml_str = r#"
name = "multi-probe"

[[probes]]
hook_type = "kprobe"
target = "vfs_open"

[[probes]]
hook_type = "tracepoint"
target = "sched/sched_process_exec"

[[probes]]
hook_type = "uprobe"
target = "SSL_write"
binary = "/usr/lib/x86_64-linux-gnu/libssl.so"
enabled = false
"#;

    let policy = TracingPolicy::from_toml(toml_str).expect("parse TOML");
    assert_eq!(policy.probes.len(), 3);
    assert_eq!(policy.probes[0].hook_type, "kprobe");
    assert_eq!(policy.probes[1].hook_type, "tracepoint");
    assert_eq!(policy.probes[2].hook_type, "uprobe");
    assert!(policy.probes[0].enabled);
    assert!(policy.probes[1].enabled);
    assert!(!policy.probes[2].enabled);
    assert_eq!(
        policy.probes[2].binary.as_deref(),
        Some("/usr/lib/x86_64-linux-gnu/libssl.so")
    );
}

// --------------------------------------------------------------------------
// Test 3: Policy with PID/type/comm filters
// --------------------------------------------------------------------------

#[test]
fn test_tracing_policy_with_filters() {
    let toml_str = r#"
name = "filtered"

[[probes]]
hook_type = "kprobe"
target = "vfs_open"

[filters]
pids = [1234, 5678]
event_types = ["file_open", "net_connect"]
exclude_comms = ["systemd", "sshd"]
"#;

    let policy = TracingPolicy::from_toml(toml_str).expect("parse TOML");
    let filters = policy.filters.as_ref().expect("filters present");
    assert_eq!(filters.pids.as_ref().unwrap(), &[1234, 5678]);
    assert_eq!(
        filters.event_types.as_ref().unwrap(),
        &["file_open", "net_connect"]
    );
    assert_eq!(
        filters.exclude_comms.as_ref().unwrap(),
        &["systemd", "sshd"]
    );
}

// --------------------------------------------------------------------------
// Test 4: ChannelSink delivers events through crossbeam channel
// --------------------------------------------------------------------------

#[test]
fn test_channel_sink_delivers_events() {
    let (tx, rx) = crossbeam_channel::bounded(64);
    let mut sink = ChannelSink::new(tx);

    let h1 = make_header(EventType::ProcessExec as u32, 100, "bash");
    let raw1 = header_to_bytes(&h1);
    sink.send(&h1, &raw1, "").expect("send ok");

    let h2 = make_header(EventType::FileOpen as u32, 200, "cat");
    let raw2 = header_to_bytes(&h2);
    sink.send(&h2, &raw2, "").expect("send ok");

    assert_eq!(sink.event_count(), 2);

    // Receive from channel.
    let (recv_h1, recv_raw1, _src1) = rx.recv().expect("receive event 1");
    assert_eq!(recv_h1.pid, 100);
    assert_eq!(recv_h1.event_type, EventType::ProcessExec as u32);
    assert_eq!(recv_raw1.len(), raw1.len());

    let (recv_h2, _, _) = rx.recv().expect("receive event 2");
    assert_eq!(recv_h2.pid, 200);
    assert_eq!(recv_h2.event_type, EventType::FileOpen as u32);
}

// --------------------------------------------------------------------------
// Test 5: Full pipeline tick → ChannelSink
// --------------------------------------------------------------------------

#[test]
fn test_sensor_pipeline_to_channel() {
    let (tx, rx) = crossbeam_channel::bounded(64);

    let mut pipeline = SensorPipeline::new();

    let mut source = SimulatedSource::new("test-src");
    source.add_process_exec(42, "nginx");
    source.add_file_open(43, "cat");
    source.add_net_connect(44, "curl");
    pipeline.add_source(Box::new(source));

    let sink = ChannelSink::new(tx);
    pipeline.add_sink(Box::new(sink));

    let count = pipeline.tick().expect("tick ok");
    assert_eq!(count, 3);

    // All three events should be in the channel.
    let mut received = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        received.push(ev);
    }
    assert_eq!(received.len(), 3);
    assert_eq!(received[0].0.pid, 42);
    assert_eq!(received[1].0.pid, 43);
    assert_eq!(received[2].0.pid, 44);
}

// --------------------------------------------------------------------------
// Test 6: Sensor state tracks programs (load/unload)
// --------------------------------------------------------------------------

#[test]
fn test_sensor_state_tracks_programs() {
    use nova_api::sensor_server::{ProgramInfo, SensorState};

    let mut state = SensorState::new();
    assert_eq!(state.programs.len(), 0);

    state.programs.push(ProgramInfo {
        name: "process_exec".into(),
        attached: true,
        attach_type: "tracepoint".into(),
        attach_target: "sched/sched_process_exec".into(),
        event_count: 0,
    });
    state.programs.push(ProgramInfo {
        name: "tcp_connect".into(),
        attached: true,
        attach_type: "kprobe".into(),
        attach_target: "tcp_v4_connect".into(),
        event_count: 100,
    });

    assert_eq!(state.programs.len(), 2);
    assert_eq!(state.programs[0].name, "process_exec");
    assert_eq!(state.programs[1].event_count, 100);

    // Unload one.
    state.programs.retain(|p| p.name != "process_exec");
    assert_eq!(state.programs.len(), 1);
    assert_eq!(state.programs[0].name, "tcp_connect");
}

// --------------------------------------------------------------------------
// Test 7: EventType mapping between nova-eye-common and proto
// --------------------------------------------------------------------------

#[test]
fn test_event_type_proto_mapping() {
    use nova_api::sensor::EventType as ProtoEventType;

    // Verify the mapping table between common EventType values and proto enum.
    let mappings: Vec<(u32, i32)> = vec![
        (EventType::ProcessExec as u32, ProtoEventType::ProcessExec as i32),
        (EventType::FileOpen as u32, ProtoEventType::FileOpen as i32),
        (EventType::NetConnect as u32, ProtoEventType::NetConnect as i32),
        (EventType::HttpRequest as u32, ProtoEventType::HttpRequest as i32),
        (EventType::DnsQuery as u32, ProtoEventType::DnsQuery as i32),
    ];

    // Common values: ProcessExec=1, FileOpen=10, NetConnect=20, HttpRequest=30, DnsQuery=40
    // Proto values: ProcessExec=1, FileOpen=4, NetConnect=7, HttpRequest=10, DnsQuery=12
    assert_eq!(mappings[0].0, 1); // common ProcessExec
    assert_eq!(mappings[0].1, 1); // proto ProcessExec
    assert_eq!(mappings[1].0, 10); // common FileOpen
    assert_eq!(mappings[1].1, 4);  // proto FileOpen
    assert_eq!(mappings[2].0, 20); // common NetConnect
    assert_eq!(mappings[2].1, 7);  // proto NetConnect
    assert_eq!(mappings[3].0, 30); // common HttpRequest
    assert_eq!(mappings[3].1, 10); // proto HttpRequest
    assert_eq!(mappings[4].0, 40); // common DnsQuery
    assert_eq!(mappings[4].1, 12); // proto DnsQuery
}

// --------------------------------------------------------------------------
// Test 8: FileOpenEvent roundtrip through pipeline
// --------------------------------------------------------------------------

#[test]
fn test_file_open_event_roundtrip() {
    let (tx, rx) = crossbeam_channel::bounded(64);

    let mut pipeline = SensorPipeline::new();

    // Build a FileOpenEvent and feed it as raw bytes.
    let mut path = [0u8; MAX_PATH_LEN];
    let name = b"/etc/passwd";
    path[..name.len()].copy_from_slice(name);

    let event = FileOpenEvent {
        header: make_header(EventType::FileOpen as u32, 500, "cat"),
        path,
        flags: 0, // O_RDONLY
        mode: 0o644,
    };

    let raw = unsafe {
        let ptr = &event as *const FileOpenEvent as *const u8;
        core::slice::from_raw_parts(ptr, core::mem::size_of::<FileOpenEvent>()).to_vec()
    };

    let mut source = SimulatedSource::new("file-src");
    source.add_raw(event.header, raw);
    pipeline.add_source(Box::new(source));

    let sink = ChannelSink::new(tx);
    pipeline.add_sink(Box::new(sink));

    let count = pipeline.tick().expect("tick ok");
    assert_eq!(count, 1);

    let (header, received_raw, _src) = rx.recv().expect("receive event");
    assert_eq!(header.event_type, EventType::FileOpen as u32);
    assert_eq!(header.pid, 500);
    assert!(received_raw.len() >= core::mem::size_of::<FileOpenEvent>());
}

// --------------------------------------------------------------------------
// Test 9: HttpRequestEvent roundtrip through pipeline
// --------------------------------------------------------------------------

#[test]
fn test_http_request_event_roundtrip() {
    let (tx, rx) = crossbeam_channel::bounded(64);

    let mut pipeline = SensorPipeline::new();

    // Build an HttpRequestEvent.
    let mut data = [0u8; MAX_HTTP_DATA_LEN];
    let payload = b"GET /index.html HTTP/1.1";
    data[..payload.len()].copy_from_slice(payload);

    let event = HttpRequestEvent {
        header: make_header(EventType::HttpRequest as u32, 600, "curl"),
        data,
        data_len: payload.len() as u32,
        _pad: 0,
    };

    let raw = unsafe {
        let ptr = &event as *const HttpRequestEvent as *const u8;
        core::slice::from_raw_parts(ptr, core::mem::size_of::<HttpRequestEvent>()).to_vec()
    };

    let mut source = SimulatedSource::new("http-src");
    source.add_raw(event.header, raw);
    pipeline.add_source(Box::new(source));

    let sink = ChannelSink::new(tx);
    pipeline.add_sink(Box::new(sink));

    let count = pipeline.tick().expect("tick ok");
    assert_eq!(count, 1);

    let (header, received_raw, _src) = rx.recv().expect("receive event");
    assert_eq!(header.event_type, EventType::HttpRequest as u32);
    assert_eq!(header.pid, 600);
    assert!(received_raw.len() >= core::mem::size_of::<HttpRequestEvent>());
}

// --------------------------------------------------------------------------
// Test 10: JsondSink writes valid JSONL to file
// --------------------------------------------------------------------------

#[test]
fn test_jsond_sink_writes_events() {
    let dir = std::env::temp_dir().join("nova-eye-jsond-test");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut sink = JsondSink::new(&path).expect("open jsond sink");
        assert_eq!(sink.event_count(), 0);

        let h1 = make_header(EventType::ProcessExec as u32, 1234, "nginx");
        let raw1 = header_to_bytes(&h1);
        sink.send(&h1, &raw1, "").expect("send ok");

        let h2 = make_header(EventType::FileOpen as u32, 5678, "cat");
        let raw2 = header_to_bytes(&h2);
        sink.send(&h2, &raw2, "").expect("send ok");

        assert_eq!(sink.event_count(), 2);
    }

    // Read back and verify each line is valid JSON with expected fields.
    let contents = std::fs::read_to_string(&path).expect("read events file");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2);

    let v1: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON line 1");
    assert_eq!(v1["event_type"], "process_exec");
    assert_eq!(v1["pid"], 1234);
    assert_eq!(v1["comm"], "nginx");
    assert_eq!(v1["timestamp_ns"], 123_456_789u64);

    let v2: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON line 2");
    assert_eq!(v2["event_type"], "file_open");
    assert_eq!(v2["pid"], 5678);
    assert_eq!(v2["comm"], "cat");

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 11: JsondSink pipeline integration (source → pipeline → JSOND file)
// --------------------------------------------------------------------------

#[test]
fn test_jsond_pipeline_end_to_end() {
    let dir = std::env::temp_dir().join("nova-eye-jsond-pipeline-test");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("pipeline-events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();

        let mut source = SimulatedSource::new("jsond-test-src");
        source.add_process_exec(100, "bash");
        source.add_file_open(200, "vim");
        source.add_net_connect(300, "curl");
        pipeline.add_source(Box::new(source));

        let jsond_sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(jsond_sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 3);
    }

    // Read back and verify all three events.
    let contents = std::fs::read_to_string(&path).expect("read events file");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3);

    // Verify each line is valid JSON.
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        assert!(v["event_type"].is_string());
        assert!(v["pid"].is_number());
        assert!(v["comm"].is_string());
        assert!(v["timestamp_ns"].is_number());
    }

    // Verify specific events.
    let v0: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    assert_eq!(v0["pid"], 100);
    assert_eq!(v0["event_type"], "process_exec");

    let v1: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON");
    assert_eq!(v1["pid"], 200);
    assert_eq!(v1["event_type"], "file_open");

    let v2: serde_json::Value = serde_json::from_str(lines[2]).expect("valid JSON");
    assert_eq!(v2["pid"], 300);
    assert_eq!(v2["event_type"], "net_connect");

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 12: Real eBPF process exec (gated behind NOVAVM_REAL_TESTS)
// --------------------------------------------------------------------------

#[test]
fn test_real_ebpf_process_exec() {
    if std::env::var("NOVAVM_REAL_TESTS").unwrap_or_default() != "1" {
        eprintln!("skipping real eBPF test (set NOVAVM_REAL_TESTS=1)");
        return;
    }

    // This test requires:
    // 1. Root privileges
    // 2. Compiled eBPF bytecode at the expected path
    // 3. Linux with KVM/eBPF support

    // Try to load the process_exec eBPF program.
    #[cfg(feature = "ebpf")]
    {
        use nova_eye::AyaBpfSource;
        use nova_eye::SensorSource;

        let bytecode_path = std::env::var("NOVA_EBPF_PROCESS")
            .unwrap_or_else(|_| "/opt/nova/ebpf/nova-eye-process".to_string());

        let mut source = AyaBpfSource::load_process_exec(&bytecode_path)
            .expect("load process_exec eBPF");

        // Trigger an exec by running a simple command.
        std::process::Command::new("true").output().expect("run true");

        // Give the kernel a moment to deliver the event.
        std::thread::sleep(std::time::Duration::from_millis(100));

        let events = source.poll_events().expect("poll events");
        assert!(
            !events.is_empty(),
            "expected at least one process_exec event from running 'true'"
        );
    }

    #[cfg(not(feature = "ebpf"))]
    {
        eprintln!("skipping real eBPF test (ebpf feature not enabled)");
    }
}

