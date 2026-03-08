//! E2E eBPF observability tests: Alpine/Python container scenarios.
//!
//! These tests simulate realistic container workloads generating eBPF events
//! that flow through the full pipeline (source → filter → sink → JSOND file).
//! Each scenario mirrors what real eBPF probes would capture when an Alpine
//! container runs Python workloads.

use nova_eye::filter::EventFilter;
use nova_eye::{JsondSink, SensorPipeline, SimulatedSource};
use nova_eye_common::{
    DnsQueryEvent, EventHeader, EventType, FileOpenEvent, HttpRequestEvent, NetConnectEvent,
    ProcessExecEvent, ProcessExitEvent, ProcessForkEvent, MAX_ADDR_LEN, MAX_HTTP_DATA_LEN,
    MAX_PATH_LEN,
};

/// Helper to build a test EventHeader with a given timestamp offset.
fn make_header(event_type: u32, pid: u32, comm: &str, ts_offset: u64) -> EventHeader {
    let mut h = EventHeader {
        event_type,
        pid,
        tid: pid,
        uid: 0,
        gid: 0,
        comm: [0u8; 16],
        timestamp_ns: 1_000_000_000 + ts_offset,
    };
    let bytes = comm.as_bytes();
    let len = bytes.len().min(16);
    h.comm[..len].copy_from_slice(&bytes[..len]);
    h
}

/// Serialize a repr(C) struct to raw bytes.
fn to_raw<T>(val: &T) -> Vec<u8> {
    let ptr = val as *const T as *const u8;
    let len = core::mem::size_of::<T>();
    unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Build a ProcessExecEvent.
fn make_process_exec(pid: u32, ppid: u32, comm: &str, filename: &str, ts: u64) -> (EventHeader, Vec<u8>) {
    let mut fname = [0u8; MAX_PATH_LEN];
    let fb = filename.as_bytes();
    fname[..fb.len().min(MAX_PATH_LEN)].copy_from_slice(&fb[..fb.len().min(MAX_PATH_LEN)]);

    let event = ProcessExecEvent {
        header: make_header(EventType::ProcessExec as u32, pid, comm, ts),
        filename: fname,
        ppid,
        _pad: 0,
    };
    (event.header, to_raw(&event))
}

/// Build a ProcessExitEvent.
fn make_process_exit(pid: u32, comm: &str, exit_code: i32, duration_ns: u64, ts: u64) -> (EventHeader, Vec<u8>) {
    let event = ProcessExitEvent {
        header: make_header(EventType::ProcessExit as u32, pid, comm, ts),
        exit_code,
        signal: 0,
        duration_ns,
    };
    (event.header, to_raw(&event))
}

/// Build a ProcessForkEvent.
fn make_process_fork(pid: u32, comm: &str, child_pid: u32, ts: u64) -> (EventHeader, Vec<u8>) {
    let event = ProcessForkEvent {
        header: make_header(EventType::ProcessFork as u32, pid, comm, ts),
        child_pid,
        child_tid: child_pid,
    };
    (event.header, to_raw(&event))
}

/// Build a FileOpenEvent.
fn make_file_open(pid: u32, comm: &str, path: &str, flags: u32, ts: u64) -> (EventHeader, Vec<u8>) {
    let mut p = [0u8; MAX_PATH_LEN];
    let pb = path.as_bytes();
    p[..pb.len().min(MAX_PATH_LEN)].copy_from_slice(&pb[..pb.len().min(MAX_PATH_LEN)]);

    let event = FileOpenEvent {
        header: make_header(EventType::FileOpen as u32, pid, comm, ts),
        path: p,
        flags,
        mode: 0o644,
    };
    (event.header, to_raw(&event))
}

/// Build a NetConnectEvent.
fn make_net_connect(
    pid: u32,
    comm: &str,
    dst_port: u16,
    ts: u64,
) -> (EventHeader, Vec<u8>) {
    let mut src = [0u8; 16];
    src[..4].copy_from_slice(&[172, 16, 0, 2]); // 172.16.0.2
    let mut dst = [0u8; 16];
    dst[..4].copy_from_slice(&[93, 184, 216, 34]); // 93.184.216.34

    let event = NetConnectEvent {
        header: make_header(EventType::NetConnect as u32, pid, comm, ts),
        src_addr: src,
        dst_addr: dst,
        src_port: 45678,
        dst_port,
        family: 2,  // AF_INET
        protocol: 6, // TCP
    };
    (event.header, to_raw(&event))
}

/// Build an HttpRequestEvent.
fn make_http_request(pid: u32, comm: &str, data: &str, ts: u64) -> (EventHeader, Vec<u8>) {
    let mut d = [0u8; MAX_HTTP_DATA_LEN];
    let db = data.as_bytes();
    let len = db.len().min(MAX_HTTP_DATA_LEN);
    d[..len].copy_from_slice(&db[..len]);

    let event = HttpRequestEvent {
        header: make_header(EventType::HttpRequest as u32, pid, comm, ts),
        data: d,
        data_len: len as u32,
        _pad: 0,
    };
    (event.header, to_raw(&event))
}

/// Build a DnsQueryEvent.
fn make_dns_query(pid: u32, comm: &str, domain: &str, ts: u64) -> (EventHeader, Vec<u8>) {
    let mut d = [0u8; MAX_ADDR_LEN];
    let db = domain.as_bytes();
    d[..db.len().min(MAX_ADDR_LEN)].copy_from_slice(&db[..db.len().min(MAX_ADDR_LEN)]);

    let event = DnsQueryEvent {
        header: make_header(EventType::DnsQuery as u32, pid, comm, ts),
        domain: d,
        query_type: 1, // A record
        _pad: [0u8; 6],
    };
    (event.header, to_raw(&event))
}

/// Parse JSONL file and return a vec of serde_json::Value.
fn read_jsonl(path: &std::path::Path) -> Vec<serde_json::Value> {
    let contents = std::fs::read_to_string(path).expect("read JSONL file");
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON"))
        .collect()
}

// --------------------------------------------------------------------------
// Test 1: Python interpreter startup — process exec + library file opens
// --------------------------------------------------------------------------

#[test]
fn test_e2e_python_startup_events() {
    let dir = std::env::temp_dir().join("nova-e2e-python-startup");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("python-startup");

        // PID 1000: Alpine init spawns Python
        let (h, r) = make_process_exec(1000, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);

        // Python opens its stdlib
        let (h, r) = make_file_open(1000, "python3", "/usr/lib/python3.11/os.py", 0, 100);
        source.add_raw(h, r);
        let (h, r) = make_file_open(1000, "python3", "/usr/lib/python3.11/site.py", 0, 200);
        source.add_raw(h, r);
        let (h, r) = make_file_open(1000, "python3", "/usr/lib/python3.11/json/__init__.py", 0, 300);
        source.add_raw(h, r);

        // Python opens shared libraries
        let (h, r) = make_file_open(1000, "python3", "/usr/lib/libpython3.11.so.1.0", 0, 400);
        source.add_raw(h, r);
        let (h, r) = make_file_open(1000, "python3", "/lib/ld-musl-x86_64.so.1", 0, 500);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 6);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 6);

    // First event: process exec
    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[0]["pid"], 1000);
    assert_eq!(events[0]["comm"], "python3");

    // Remaining: file opens
    for ev in &events[1..] {
        assert_eq!(ev["event_type"], "file_open");
        assert_eq!(ev["pid"], 1000);
        assert_eq!(ev["comm"], "python3");
    }

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 2: Python HTTP client — DNS + connect + HTTP request
// --------------------------------------------------------------------------

#[test]
fn test_e2e_python_http_client() {
    let dir = std::env::temp_dir().join("nova-e2e-python-http");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("python-http");

        // Python process already running
        let (h, r) = make_process_exec(2000, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);

        // DNS lookup for api.example.com
        let (h, r) = make_dns_query(2000, "python3", "api.example.com", 1000);
        source.add_raw(h, r);

        // TCP connect to 93.184.216.34:443
        let (h, r) = make_net_connect(2000, "python3", 443, 2000);
        source.add_raw(h, r);

        // SSL_write → HTTP request
        let (h, r) = make_http_request(2000, "python3", "GET /api/v1/data HTTP/1.1", 3000);
        source.add_raw(h, r);

        // Second request (keep-alive)
        let (h, r) = make_http_request(2000, "python3", "POST /api/v1/submit HTTP/1.1", 4000);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 5);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 5);

    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[1]["event_type"], "dns_query");
    assert_eq!(events[2]["event_type"], "net_connect");
    assert_eq!(events[3]["event_type"], "http_request");
    assert_eq!(events[4]["event_type"], "http_request");

    // All from same PID
    for ev in &events {
        assert_eq!(ev["pid"], 2000);
        assert_eq!(ev["comm"], "python3");
    }

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 3: Python subprocess management — fork + exec + exit
// --------------------------------------------------------------------------

#[test]
fn test_e2e_python_subprocess_lifecycle() {
    let dir = std::env::temp_dir().join("nova-e2e-python-subprocess");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("python-subprocess");

        // Parent python process
        let (h, r) = make_process_exec(3000, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);

        // Python forks to run subprocess
        let (h, r) = make_process_fork(3000, "python3", 3001, 1000);
        source.add_raw(h, r);

        // Child execs `ls -la /tmp`
        let (h, r) = make_process_exec(3001, 3000, "ls", "/bin/ls", 1100);
        source.add_raw(h, r);

        // ls opens /tmp directory
        let (h, r) = make_file_open(3001, "ls", "/tmp", 0, 1200);
        source.add_raw(h, r);

        // ls exits
        let (h, r) = make_process_exit(3001, "ls", 0, 5_000_000, 1300);
        source.add_raw(h, r);

        // Python forks again for `grep`
        let (h, r) = make_process_fork(3000, "python3", 3002, 2000);
        source.add_raw(h, r);

        // Child execs grep
        let (h, r) = make_process_exec(3002, 3000, "grep", "/bin/grep", 2100);
        source.add_raw(h, r);

        // grep exits with match found
        let (h, r) = make_process_exit(3002, "grep", 0, 3_000_000, 2200);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 8);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 8);

    // Verify process lifecycle sequence
    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[0]["pid"], 3000);

    assert_eq!(events[1]["event_type"], "process_fork");
    assert_eq!(events[1]["pid"], 3000);

    assert_eq!(events[2]["event_type"], "process_exec");
    assert_eq!(events[2]["pid"], 3001);

    assert_eq!(events[3]["event_type"], "file_open");
    assert_eq!(events[3]["pid"], 3001);

    assert_eq!(events[4]["event_type"], "process_exit");
    assert_eq!(events[4]["pid"], 3001);

    assert_eq!(events[5]["event_type"], "process_fork");
    assert_eq!(events[5]["pid"], 3000);

    assert_eq!(events[6]["event_type"], "process_exec");
    assert_eq!(events[6]["pid"], 3002);

    assert_eq!(events[7]["event_type"], "process_exit");
    assert_eq!(events[7]["pid"], 3002);

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 4: Python file I/O workload — reads config, writes output
// --------------------------------------------------------------------------

#[test]
fn test_e2e_python_file_io_workload() {
    let dir = std::env::temp_dir().join("nova-e2e-python-fileio");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("python-fileio");

        // Python starts
        let (h, r) = make_process_exec(4000, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);

        // Read config file
        let (h, r) = make_file_open(4000, "python3", "/app/config.yaml", 0, 100);
        source.add_raw(h, r);

        // Read input data
        let (h, r) = make_file_open(4000, "python3", "/data/input.csv", 0, 200);
        source.add_raw(h, r);

        // Open output file for writing (O_WRONLY|O_CREAT = 0x41)
        let (h, r) = make_file_open(4000, "python3", "/data/output.json", 0x41, 300);
        source.add_raw(h, r);

        // Open log file
        let (h, r) = make_file_open(4000, "python3", "/var/log/app.log", 0x41, 400);
        source.add_raw(h, r);

        // Open temp file
        let (h, r) = make_file_open(4000, "python3", "/tmp/processing.tmp", 0x42, 500);
        source.add_raw(h, r);

        // Python exits
        let (h, r) = make_process_exit(4000, "python3", 0, 2_000_000_000, 600);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 7);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 7);

    // Process lifecycle
    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[6]["event_type"], "process_exit");

    // File opens
    let file_events: Vec<_> = events.iter().filter(|e| e["event_type"] == "file_open").collect();
    assert_eq!(file_events.len(), 5);

    // All from python3
    for ev in &events {
        assert_eq!(ev["comm"], "python3");
        assert_eq!(ev["pid"], 4000);
    }

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 5: Multi-service Alpine container — nginx + python + curl
// --------------------------------------------------------------------------

#[test]
fn test_e2e_multi_service_container() {
    let dir = std::env::temp_dir().join("nova-e2e-multi-service");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("multi-service");

        // nginx starts (PID 100)
        let (h, r) = make_process_exec(100, 1, "nginx", "/usr/sbin/nginx", 0);
        source.add_raw(h, r);
        let (h, r) = make_file_open(100, "nginx", "/etc/nginx/nginx.conf", 0, 100);
        source.add_raw(h, r);

        // python3 starts (PID 200)
        let (h, r) = make_process_exec(200, 1, "python3", "/usr/bin/python3", 500);
        source.add_raw(h, r);

        // Python connects to an external API
        let (h, r) = make_dns_query(200, "python3", "pypi.org", 1000);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(200, "python3", 443, 1100);
        source.add_raw(h, r);
        let (h, r) = make_http_request(200, "python3", "GET /simple/requests/ HTTP/1.1", 1200);
        source.add_raw(h, r);

        // curl runs (PID 300) — health check
        let (h, r) = make_process_exec(300, 1, "curl", "/usr/bin/curl", 2000);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(300, "curl", 80, 2100);
        source.add_raw(h, r);
        let (h, r) = make_http_request(300, "curl", "GET /health HTTP/1.1", 2200);
        source.add_raw(h, r);
        let (h, r) = make_process_exit(300, "curl", 0, 500_000_000, 2300);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 10);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 10);

    // Verify distinct processes
    let nginx_events: Vec<_> = events.iter().filter(|e| e["comm"] == "nginx").collect();
    let python_events: Vec<_> = events.iter().filter(|e| e["comm"] == "python3").collect();
    let curl_events: Vec<_> = events.iter().filter(|e| e["comm"] == "curl").collect();

    assert_eq!(nginx_events.len(), 2);
    assert_eq!(python_events.len(), 4);
    assert_eq!(curl_events.len(), 4);

    // Verify event type distribution
    let exec_count = events.iter().filter(|e| e["event_type"] == "process_exec").count();
    let net_count = events.iter().filter(|e| e["event_type"] == "net_connect").count();
    let http_count = events.iter().filter(|e| e["event_type"] == "http_request").count();
    let dns_count = events.iter().filter(|e| e["event_type"] == "dns_query").count();

    assert_eq!(exec_count, 3);
    assert_eq!(net_count, 2);
    assert_eq!(http_count, 2);
    assert_eq!(dns_count, 1);

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 6: PID-filtered pipeline — only capture python events
// --------------------------------------------------------------------------

#[test]
fn test_e2e_pid_filtered_pipeline() {
    let dir = std::env::temp_dir().join("nova-e2e-pid-filter");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("pid-filtered");

        // nginx (PID 100) — should be filtered out
        let (h, r) = make_process_exec(100, 1, "nginx", "/usr/sbin/nginx", 0);
        source.add_raw(h, r);

        // python (PID 200) — should pass
        let (h, r) = make_process_exec(200, 1, "python3", "/usr/bin/python3", 100);
        source.add_raw(h, r);
        let (h, r) = make_file_open(200, "python3", "/app/main.py", 0, 200);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(200, "python3", 443, 300);
        source.add_raw(h, r);

        // curl (PID 300) — should be filtered out
        let (h, r) = make_process_exec(300, 1, "curl", "/usr/bin/curl", 400);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));

        // Filter to only PID 200
        let mut filter = EventFilter::new();
        filter.allow_pid(200);
        pipeline.set_filter(filter);

        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 3); // Only PID 200 events pass
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 3);

    for ev in &events {
        assert_eq!(ev["pid"], 200);
        assert_eq!(ev["comm"], "python3");
    }

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 7: Event-type filter — only net + HTTP events
// --------------------------------------------------------------------------

#[test]
fn test_e2e_type_filtered_pipeline() {
    let dir = std::env::temp_dir().join("nova-e2e-type-filter");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("type-filtered");

        // Mix of all event types
        let (h, r) = make_process_exec(500, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);
        let (h, r) = make_file_open(500, "python3", "/app/main.py", 0, 100);
        source.add_raw(h, r);
        let (h, r) = make_dns_query(500, "python3", "api.example.com", 200);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(500, "python3", 443, 300);
        source.add_raw(h, r);
        let (h, r) = make_http_request(500, "python3", "GET /api HTTP/1.1", 400);
        source.add_raw(h, r);
        let (h, r) = make_process_exit(500, "python3", 0, 1_000_000, 500);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));

        // Only capture network and HTTP events
        let mut filter = EventFilter::new();
        filter.allow_type(EventType::NetConnect);
        filter.allow_type(EventType::HttpRequest);
        filter.allow_type(EventType::DnsQuery);
        pipeline.set_filter(filter);

        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 3); // dns + net_connect + http_request
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 3);

    assert_eq!(events[0]["event_type"], "dns_query");
    assert_eq!(events[1]["event_type"], "net_connect");
    assert_eq!(events[2]["event_type"], "http_request");

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 8: Full Alpine container lifecycle — init to shutdown
// --------------------------------------------------------------------------

#[test]
fn test_e2e_alpine_container_lifecycle() {
    let dir = std::env::temp_dir().join("nova-e2e-alpine-lifecycle");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("alpine-lifecycle");

        // Phase 1: Container init
        let (h, r) = make_process_exec(1, 0, "init", "/sbin/init", 0);
        source.add_raw(h, r);
        let (h, r) = make_file_open(1, "init", "/etc/inittab", 0, 100);
        source.add_raw(h, r);

        // Phase 2: Init spawns services
        let (h, r) = make_process_fork(1, "init", 10, 500);
        source.add_raw(h, r);
        let (h, r) = make_process_exec(10, 1, "python3", "/usr/bin/python3", 600);
        source.add_raw(h, r);

        // Phase 3: Python app starts
        let (h, r) = make_file_open(10, "python3", "/app/server.py", 0, 700);
        source.add_raw(h, r);
        let (h, r) = make_file_open(10, "python3", "/app/requirements.txt", 0, 800);
        source.add_raw(h, r);

        // Phase 4: Python makes outbound connections
        let (h, r) = make_dns_query(10, "python3", "db.internal", 1000);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(10, "python3", 5432, 1100);
        source.add_raw(h, r);

        // Phase 5: Python handles requests via SSL
        let (h, r) = make_http_request(10, "python3", "GET /health HTTP/1.1", 2000);
        source.add_raw(h, r);
        let (h, r) = make_http_request(10, "python3", "POST /api/data HTTP/1.1", 3000);
        source.add_raw(h, r);

        // Phase 6: Subprocess for cron job
        let (h, r) = make_process_fork(10, "python3", 11, 4000);
        source.add_raw(h, r);
        let (h, r) = make_process_exec(11, 10, "python3", "/usr/bin/python3", 4100);
        source.add_raw(h, r);
        let (h, r) = make_file_open(11, "python3", "/app/cron_job.py", 0, 4200);
        source.add_raw(h, r);
        let (h, r) = make_process_exit(11, "python3", 0, 500_000_000, 4500);
        source.add_raw(h, r);

        // Phase 7: Shutdown
        let (h, r) = make_process_exit(10, "python3", 0, 5_000_000_000, 5000);
        source.add_raw(h, r);
        let (h, r) = make_process_exit(1, "init", 0, 6_000_000_000, 6000);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 16);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 16);

    // Verify all event types present
    let types: Vec<&str> = events.iter().map(|e| e["event_type"].as_str().unwrap()).collect();
    assert!(types.contains(&"process_exec"));
    assert!(types.contains(&"process_exit"));
    assert!(types.contains(&"process_fork"));
    assert!(types.contains(&"file_open"));
    assert!(types.contains(&"net_connect"));
    assert!(types.contains(&"dns_query"));
    assert!(types.contains(&"http_request"));

    // Verify timestamps are monotonically increasing
    let timestamps: Vec<u64> = events.iter().map(|e| e["timestamp_ns"].as_u64().unwrap()).collect();
    for i in 1..timestamps.len() {
        assert!(
            timestamps[i] >= timestamps[i - 1],
            "timestamps not monotonic at index {}: {} < {}",
            i,
            timestamps[i],
            timestamps[i - 1]
        );
    }

    // First event is init, last is init exit
    assert_eq!(events[0]["comm"], "init");
    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[15]["comm"], "init");
    assert_eq!(events[15]["event_type"], "process_exit");

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 9: Dual-sink pipeline — ChannelSink + JsondSink simultaneously
// --------------------------------------------------------------------------

#[test]
fn test_e2e_dual_sink_pipeline() {
    let dir = std::env::temp_dir().join("nova-e2e-dual-sink");
    std::fs::create_dir_all(&dir).ok();
    let jsonl_path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&jsonl_path);

    let (tx, rx) = crossbeam_channel::bounded(64);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("dual-sink");

        let (h, r) = make_process_exec(7000, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);
        let (h, r) = make_dns_query(7000, "python3", "example.com", 100);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(7000, "python3", 443, 200);
        source.add_raw(h, r);
        let (h, r) = make_http_request(7000, "python3", "GET / HTTP/1.1", 300);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));

        // Two sinks: channel + JSOND
        let channel_sink = nova_eye::ChannelSink::new(tx);
        pipeline.add_sink(Box::new(channel_sink));

        let jsond_sink = JsondSink::new(&jsonl_path).expect("open jsond sink");
        pipeline.add_sink(Box::new(jsond_sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 4);
    }

    // Verify channel received all events
    let mut channel_events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        channel_events.push(ev);
    }
    assert_eq!(channel_events.len(), 4);

    // Verify JSOND file has all events
    let file_events = read_jsonl(&jsonl_path);
    assert_eq!(file_events.len(), 4);

    // Both sinks should see same PIDs
    for (i, (header, _, _)) in channel_events.iter().enumerate() {
        assert_eq!(header.pid, 7000);
        assert_eq!(file_events[i]["pid"], 7000);
    }

    let _ = std::fs::remove_file(&jsonl_path);
}

// --------------------------------------------------------------------------
// Test 10: Multiple pipeline ticks — continuous event flow
// --------------------------------------------------------------------------

#[test]
fn test_e2e_continuous_event_flow() {
    let dir = std::env::temp_dir().join("nova-e2e-continuous");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();

        // Tick 1: process startup
        let mut source1 = SimulatedSource::new("tick1");
        let (h, r) = make_process_exec(8000, 1, "python3", "/usr/bin/python3", 0);
        source1.add_raw(h, r);
        let (h, r) = make_file_open(8000, "python3", "/app/main.py", 0, 100);
        source1.add_raw(h, r);
        pipeline.add_source(Box::new(source1));

        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick 1 ok");
        assert_eq!(count, 2);

        // Tick 2: network activity (add new source for next tick)
        let mut source2 = SimulatedSource::new("tick2");
        let (h, r) = make_dns_query(8000, "python3", "redis.local", 1000);
        source2.add_raw(h, r);
        let (h, r) = make_net_connect(8000, "python3", 6379, 1100);
        source2.add_raw(h, r);
        pipeline.add_source(Box::new(source2));

        let count = pipeline.tick().expect("tick 2 ok");
        assert_eq!(count, 2);

        // Tick 3: HTTP + exit
        let mut source3 = SimulatedSource::new("tick3");
        let (h, r) = make_http_request(8000, "python3", "GET /status HTTP/1.1", 2000);
        source3.add_raw(h, r);
        let (h, r) = make_process_exit(8000, "python3", 0, 3_000_000_000, 3000);
        source3.add_raw(h, r);
        pipeline.add_source(Box::new(source3));

        let count = pipeline.tick().expect("tick 3 ok");
        assert_eq!(count, 2);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 6);

    // Verify complete timeline
    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[1]["event_type"], "file_open");
    assert_eq!(events[2]["event_type"], "dns_query");
    assert_eq!(events[3]["event_type"], "net_connect");
    assert_eq!(events[4]["event_type"], "http_request");
    assert_eq!(events[5]["event_type"], "process_exit");

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 11: ReplaySource — replay from JSONL, re-export through pipeline
// --------------------------------------------------------------------------

#[test]
fn test_e2e_replay_source_roundtrip() {
    let dir = std::env::temp_dir().join("nova-e2e-replay");
    std::fs::create_dir_all(&dir).ok();
    let input_path = dir.join("input.jsonl");
    let output_path = dir.join("output.jsonl");
    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&output_path);

    // Write a synthetic event log to replay
    let input_data = r#"{"type":1,"pid":9000,"tid":9000,"uid":0,"comm":"python3","ts":100000}
{"type":10,"pid":9000,"tid":9000,"uid":0,"comm":"python3","ts":200000}
{"type":20,"pid":9000,"tid":9000,"uid":0,"comm":"python3","ts":300000}
"#;
    std::fs::write(&input_path, input_data).expect("write input");

    {
        let mut pipeline = SensorPipeline::new();

        let replay = nova_eye::ReplaySource::from_file(&input_path).expect("load replay");
        pipeline.add_source(Box::new(replay));

        let sink = JsondSink::new(&output_path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 3);
    }

    let events = read_jsonl(&output_path);
    assert_eq!(events.len(), 3);

    assert_eq!(events[0]["event_type"], "process_exec");
    assert_eq!(events[0]["pid"], 9000);
    assert_eq!(events[1]["event_type"], "file_open");
    assert_eq!(events[2]["event_type"], "net_connect");

    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&output_path);
}

// --------------------------------------------------------------------------
// Test 12: TracingPolicy-driven source selection
// --------------------------------------------------------------------------

#[test]
fn test_e2e_policy_driven_event_capture() {
    use nova_eye::policy::TracingPolicy;

    let dir = std::env::temp_dir().join("nova-e2e-policy-driven");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    // Define a policy that targets file + network probes
    let policy_toml = r#"
name = "python-security-audit"

[[probes]]
hook_type = "kprobe"
target = "vfs_open"

[[probes]]
hook_type = "kprobe"
target = "tcp_v4_connect"

[[probes]]
hook_type = "uprobe"
target = "SSL_write"
binary = "/usr/lib/libssl.so"

[filters]
pids = [5000]
event_types = ["file_open", "net_connect", "http_request"]
"#;

    let policy = TracingPolicy::from_toml(policy_toml).expect("parse policy");
    assert_eq!(policy.name, "python-security-audit");
    assert_eq!(policy.probes.len(), 3);

    // Build filter from policy
    let filters = policy.filters.as_ref().expect("has filters");
    let mut filter = EventFilter::new();
    for pid in filters.pids.as_ref().unwrap() {
        filter.allow_pid(*pid);
    }
    for et in filters.event_types.as_ref().unwrap() {
        match et.as_str() {
            "file_open" => filter.allow_type(EventType::FileOpen),
            "net_connect" => filter.allow_type(EventType::NetConnect),
            "http_request" => filter.allow_type(EventType::HttpRequest),
            _ => {}
        }
    }

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("policy-driven");

        // PID 5000 events (should pass)
        let (h, r) = make_process_exec(5000, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);
        let (h, r) = make_file_open(5000, "python3", "/etc/shadow", 0, 100);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(5000, "python3", 4444, 200);
        source.add_raw(h, r);
        let (h, r) = make_http_request(5000, "python3", "POST /exfil HTTP/1.1", 300);
        source.add_raw(h, r);

        // PID 6000 events (should be filtered out)
        let (h, r) = make_file_open(6000, "bash", "/etc/passwd", 0, 400);
        source.add_raw(h, r);
        let (h, r) = make_net_connect(6000, "bash", 80, 500);
        source.add_raw(h, r);

        // PID 5000 but process_exec type (should be filtered — not in event_types)
        // (already added above at ts=0, which will be filtered)

        pipeline.add_source(Box::new(source));
        pipeline.set_filter(filter);

        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        // Only PID 5000 + matching types: file_open, net_connect, http_request = 3
        assert_eq!(count, 3);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 3);

    assert_eq!(events[0]["event_type"], "file_open");
    assert_eq!(events[0]["pid"], 5000);
    assert_eq!(events[1]["event_type"], "net_connect");
    assert_eq!(events[2]["event_type"], "http_request");

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 13: High-throughput burst — 1000 events through pipeline
// --------------------------------------------------------------------------

#[test]
fn test_e2e_high_throughput_burst() {
    let dir = std::env::temp_dir().join("nova-e2e-throughput");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    let event_count = 1000;

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("throughput");

        for i in 0..event_count {
            let pid = 10000 + (i % 50); // 50 distinct PIDs
            let comm = match i % 4 {
                0 => "python3",
                1 => "nginx",
                2 => "curl",
                _ => "node",
            };

            match i % 5 {
                0 => {
                    let (h, r) = make_process_exec(pid, 1, comm, "/usr/bin/app", i as u64);
                    source.add_raw(h, r);
                }
                1 => {
                    let (h, r) = make_file_open(pid, comm, "/tmp/data.txt", 0, i as u64);
                    source.add_raw(h, r);
                }
                2 => {
                    let (h, r) = make_net_connect(pid, comm, 443, i as u64);
                    source.add_raw(h, r);
                }
                3 => {
                    let (h, r) = make_http_request(pid, comm, "GET /api HTTP/1.1", i as u64);
                    source.add_raw(h, r);
                }
                _ => {
                    let (h, r) = make_dns_query(pid, comm, "example.com", i as u64);
                    source.add_raw(h, r);
                }
            }
        }

        pipeline.add_source(Box::new(source));
        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, event_count as usize);
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), event_count as usize);

    // Every line is valid JSON with required fields
    for ev in &events {
        assert!(ev["event_type"].is_string());
        assert!(ev["pid"].is_number());
        assert!(ev["comm"].is_string());
        assert!(ev["timestamp_ns"].is_number());
    }

    // Verify event type distribution
    let types: std::collections::HashMap<&str, usize> = events.iter().fold(
        std::collections::HashMap::new(),
        |mut map, ev| {
            *map.entry(ev["event_type"].as_str().unwrap()).or_insert(0) += 1;
            map
        },
    );
    assert_eq!(types["process_exec"], 200);
    assert_eq!(types["file_open"], 200);
    assert_eq!(types["net_connect"], 200);
    assert_eq!(types["http_request"], 200);
    assert_eq!(types["dns_query"], 200);

    let _ = std::fs::remove_file(&path);
}

// --------------------------------------------------------------------------
// Test 14: Sensitive file access detection scenario
// --------------------------------------------------------------------------

#[test]
fn test_e2e_sensitive_file_access_detection() {
    let dir = std::env::temp_dir().join("nova-e2e-sensitive-files");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();
        let mut source = SimulatedSource::new("sensitive-files");

        // Python starts
        let (h, r) = make_process_exec(1500, 1, "python3", "/usr/bin/python3", 0);
        source.add_raw(h, r);

        // Normal file accesses
        let (h, r) = make_file_open(1500, "python3", "/app/main.py", 0, 100);
        source.add_raw(h, r);
        let (h, r) = make_file_open(1500, "python3", "/usr/lib/python3.11/os.py", 0, 200);
        source.add_raw(h, r);

        // Suspicious: reading /etc/shadow
        let (h, r) = make_file_open(1500, "python3", "/etc/shadow", 0, 300);
        source.add_raw(h, r);

        // Suspicious: reading SSH keys
        let (h, r) = make_file_open(1500, "python3", "/root/.ssh/id_rsa", 0, 400);
        source.add_raw(h, r);

        // Suspicious: reading kube secrets
        let (h, r) = make_file_open(1500, "python3", "/var/run/secrets/kubernetes.io/token", 0, 500);
        source.add_raw(h, r);

        // Suspicious: outbound connection after reading secrets
        let (h, r) = make_net_connect(1500, "python3", 4444, 600);
        source.add_raw(h, r);
        let (h, r) = make_http_request(1500, "python3", "POST /upload HTTP/1.1", 700);
        source.add_raw(h, r);

        pipeline.add_source(Box::new(source));

        // Only capture file_open events for analysis
        let mut filter = EventFilter::new();
        filter.allow_type(EventType::FileOpen);
        filter.allow_type(EventType::NetConnect);
        filter.allow_type(EventType::HttpRequest);
        pipeline.set_filter(filter);

        let sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(sink));

        let count = pipeline.tick().expect("tick ok");
        assert_eq!(count, 7); // 5 file_open + 1 net_connect + 1 http_request
    }

    let events = read_jsonl(&path);
    assert_eq!(events.len(), 7);

    // All file_open events captured
    let file_events: Vec<_> = events.iter().filter(|e| e["event_type"] == "file_open").collect();
    assert_eq!(file_events.len(), 5);

    // Net + HTTP events captured
    let net_events: Vec<_> = events.iter().filter(|e| e["event_type"] == "net_connect").collect();
    assert_eq!(net_events.len(), 1);

    let http_events: Vec<_> = events.iter().filter(|e| e["event_type"] == "http_request").collect();
    assert_eq!(http_events.len(), 1);

    // All from the suspicious process
    for ev in &events {
        assert_eq!(ev["pid"], 1500);
        assert_eq!(ev["comm"], "python3");
    }

    let _ = std::fs::remove_file(&path);
}
