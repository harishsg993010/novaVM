//! Stage 11b integration tests — Guest eBPF injection, TOML config, GuestEventSource.

use nova_api::config::{DaemonConfig, GuestSensorConfig, SensorConfig};
use nova_eye::guest_source::GuestEventSource;
use nova_eye::source::SensorSource;
use nova_eye::{ChannelSink, JsondSink, SensorPipeline, SimulatedSource};
use nova_eye_common::EventHeader;

// ── Test helpers ───────────────────────────────────────────────────────

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

fn header_to_bytes(h: &EventHeader) -> Vec<u8> {
    let ptr = h as *const EventHeader as *const u8;
    let len = core::mem::size_of::<EventHeader>();
    unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
}

// ── 1. Config parsing ──────────────────────────────────────────────────

#[test]
fn test_daemon_config_parse_toml() {
    let toml = r#"
[daemon]
socket = "/tmp/test.sock"
image_dir = "/tmp/images"
kernel = "/boot/vmlinux"

[sensor]
events_log = "/tmp/events.jsonl"
ebpf_dir = "/tmp/ebpf"

[sensor.guest]
enabled = true
agent_path = "/tmp/agent"
event_port = 1234

[[sensor.probes]]
hook_type = "tracepoint"
target = "sched/sched_process_exec"
bytecode = "nova-eye-process"

[[sensor.probes]]
hook_type = "kprobe"
target = "vfs_open"
bytecode = "nova-eye-file"

[[sensor.probes]]
hook_type = "uprobe"
target = "SSL_write"
bytecode = "nova-eye-http"
binary = "/usr/lib/libssl.so"
"#;
    let cfg = DaemonConfig::from_toml(toml).unwrap();
    assert_eq!(cfg.daemon.socket, "/tmp/test.sock");
    assert_eq!(cfg.daemon.image_dir, "/tmp/images");
    assert_eq!(cfg.daemon.kernel.as_deref(), Some("/boot/vmlinux"));
    assert_eq!(cfg.sensor.events_log, "/tmp/events.jsonl");
    assert_eq!(cfg.sensor.ebpf_dir, "/tmp/ebpf");
    assert!(cfg.sensor.guest.enabled);
    assert_eq!(cfg.sensor.guest.agent_path, "/tmp/agent");
    assert_eq!(cfg.sensor.guest.event_port, 1234);
    assert_eq!(cfg.sensor.probes.len(), 3);
    assert_eq!(cfg.sensor.probes[0].hook_type, "tracepoint");
    assert_eq!(cfg.sensor.probes[1].target, "vfs_open");
    assert_eq!(cfg.sensor.probes[2].binary.as_deref(), Some("/usr/lib/libssl.so"));
}

#[test]
fn test_daemon_config_defaults() {
    let cfg = DaemonConfig::from_toml("").unwrap();
    assert_eq!(cfg.daemon.socket, "/run/nova/nova.sock");
    assert_eq!(cfg.daemon.image_dir, "/var/lib/nova/images");
    assert!(cfg.daemon.kernel.is_none());
    assert_eq!(cfg.sensor.events_log, "/var/run/nova/events.jsonl");
    assert_eq!(cfg.sensor.ebpf_dir, "/opt/nova/ebpf");
    assert!(!cfg.sensor.guest.enabled);
    assert_eq!(cfg.sensor.guest.event_port, 9876);
    assert!(cfg.sensor.probes.is_empty());
}

#[test]
fn test_daemon_config_probes_list() {
    let toml = r#"
[[sensor.probes]]
hook_type = "tracepoint"
target = "sched/sched_process_exec"
bytecode = "nova-eye-process"

[[sensor.probes]]
hook_type = "kprobe"
target = "vfs_open"

[[sensor.probes]]
hook_type = "kprobe"
target = "tcp_v4_connect"
bytecode = "nova-eye-network"

[[sensor.probes]]
hook_type = "uprobe"
target = "SSL_write"
bytecode = "nova-eye-http"
binary = "/usr/lib/libssl.so"

[[sensor.probes]]
hook_type = "uprobe"
target = "SSL_read"
bytecode = "nova-eye-http-read"
binary = "/usr/lib/libssl.so"
enabled = false
"#;
    let cfg = DaemonConfig::from_toml(toml).unwrap();
    assert_eq!(cfg.sensor.probes.len(), 5);
    // First 4 enabled by default, last one explicitly disabled.
    assert!(cfg.sensor.probes[0].enabled);
    assert!(cfg.sensor.probes[3].enabled);
    assert!(!cfg.sensor.probes[4].enabled);
    // Second probe has no bytecode.
    assert!(cfg.sensor.probes[1].bytecode.is_none());
    assert_eq!(cfg.sensor.probes[4].target, "SSL_read");
}

// ── 4. GuestEventSource UDP ────────────────────────────────────────────

#[test]
fn test_guest_event_source_receives_udp() {
    // Bind to a random port.
    let source = GuestEventSource::new("127.0.0.1:0", "test-vm-1").unwrap();
    let local_addr = source.socket_local_addr();

    // Send a fake event via UDP.
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let header = make_header(1, 42, "test-cmd");
    let raw = header_to_bytes(&header);
    sender.send_to(&raw, local_addr).unwrap();

    // Small delay for OS to deliver.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut source = source;
    let events = source.poll_events().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0.pid, 42);
    assert_eq!(events[0].0.event_type, 1);
}

#[test]
fn test_guest_event_source_sandbox_id() {
    let source = GuestEventSource::new("127.0.0.1:0", "my-sandbox").unwrap();
    assert_eq!(source.sandbox_id(), "my-sandbox");
    assert_eq!(source.name(), "guest:my-sandbox");
}

// ── 6. Initrd injection ────────────────────────────────────────────────

#[test]
fn test_initrd_inject_agent_and_bytecode() {
    let mut cpio = Vec::new();

    // Inject agent binary.
    let fake_agent = b"#!/bin/sh\necho agent\n";
    nova_boot::initrd::inject_file(&mut cpio, "sbin/nova-eye-agent", fake_agent, 0o100755);

    // Inject eBPF bytecode.
    let fake_bytecode = vec![0xEF, 0xBE, 0xAD, 0xDE];
    nova_boot::initrd::inject_file(&mut cpio, "opt/nova/ebpf/nova-eye-process", &fake_bytecode, 0o100644);

    // Inject probe config.
    let probes_json = b"{\"probes\":[]}";
    nova_boot::initrd::inject_file(&mut cpio, "etc/nova/probes.json", probes_json, 0o100644);

    // Verify cpio contains expected paths (look for the strings in the raw cpio).
    let cpio_str = String::from_utf8_lossy(&cpio);
    assert!(cpio_str.contains("sbin/nova-eye-agent"));
    assert!(cpio_str.contains("opt/nova/ebpf/nova-eye-process"));
    assert!(cpio_str.contains("etc/nova/probes.json"));
}

// ── 7. Init script with agent ──────────────────────────────────────────

#[test]
fn test_init_script_with_eye_agent() {
    let setup = nova_vmm::network::NetworkSetup::default_for_tap("nova-tap0");

    // Without agent.
    let script = setup.guest_init_script("/sbin/init");
    assert!(!script.contains("nova-eye-agent"));

    // With agent.
    let script_with = setup.guest_init_script_with_agent("/sbin/init", true);
    assert!(script_with.contains("nova-eye-agent"));
    assert!(script_with.contains("if [ -x /sbin/nova-eye-agent ]"));
}

// ── 8. Dynamic source addition ─────────────────────────────────────────

#[test]
fn test_dynamic_source_addition() {
    let mut pipeline = SensorPipeline::new();

    // Start with one source.
    let mut src1 = SimulatedSource::new("src1");
    src1.add_process_exec(100, "init");
    pipeline.add_source(Box::new(src1));

    let (tx, rx) = crossbeam_channel::bounded(16);
    pipeline.add_sink(Box::new(ChannelSink::new(tx)));

    // Tick to drain source1.
    let n = pipeline.tick().unwrap();
    assert_eq!(n, 1);

    // Dynamically add another source.
    let mut src2 = SimulatedSource::new("src2");
    src2.add_file_open(200, "cat");
    pipeline.add_source(Box::new(src2));

    let n = pipeline.tick().unwrap();
    assert_eq!(n, 1);

    // Verify both events came through.
    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0.pid, 100);
    assert_eq!(events[1].0.pid, 200);
}

// ── 9. Pipeline guest → channel ─────────────────────────────────────────

#[test]
fn test_pipeline_guest_to_channel() {
    // Use a loopback UDP socket to simulate guest events.
    let source = GuestEventSource::new("127.0.0.1:0", "guest-vm").unwrap();
    let local_addr = source.socket_local_addr();

    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();

    // Send 3 events.
    for pid in [10, 20, 30] {
        let header = make_header(1, pid, "proc");
        let raw = header_to_bytes(&header);
        sender.send_to(&raw, &local_addr).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut pipeline = SensorPipeline::new();
    pipeline.add_source(Box::new(source));

    let (tx, rx) = crossbeam_channel::bounded(64);
    pipeline.add_sink(Box::new(ChannelSink::new(tx)));

    let n = pipeline.tick().unwrap();
    assert_eq!(n, 3);

    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(events.len(), 3);
    let pids: Vec<u32> = events.iter().map(|e| e.0.pid).collect();
    assert!(pids.contains(&10));
    assert!(pids.contains(&20));
    assert!(pids.contains(&30));
}

// ── 10. Sandbox ID propagation ──────────────────────────────────────────

#[test]
fn test_sandbox_id_propagation() {
    let source = GuestEventSource::new("127.0.0.1:0", "sb-alpha").unwrap();
    let local_addr = source.socket_local_addr();
    assert_eq!(source.sandbox_id(), "sb-alpha");
    assert_eq!(source.name(), "guest:sb-alpha");

    // Send a fake event.
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let header = make_header(1, 99, "test");
    let raw = header_to_bytes(&header);
    sender.send_to(&raw, local_addr).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Wire through pipeline → ChannelSink → verify source_name carries sandbox_id.
    let mut pipeline = SensorPipeline::new();
    pipeline.add_source(Box::new(source));
    let (tx, rx) = crossbeam_channel::bounded(16);
    pipeline.add_sink(Box::new(ChannelSink::new(tx)));

    let n = pipeline.tick().unwrap();
    assert_eq!(n, 1);

    let (hdr, _raw, source_name) = rx.recv().unwrap();
    assert_eq!(hdr.pid, 99);
    assert_eq!(source_name, "guest:sb-alpha");

    // Extract sandbox_id the same way the daemon does.
    let sandbox_id = source_name.strip_prefix("guest:").unwrap_or("");
    assert_eq!(sandbox_id, "sb-alpha");
}

// ── 11. HttpResponseEvent roundtrip ─────────────────────────────────────

#[test]
fn test_ssl_read_event_roundtrip() {
    // Simulate an HttpResponse event (type=31) flowing through the pipeline.
    let mut src = SimulatedSource::new("ssl-read-sim");
    let header = make_header(31, 42, "curl"); // 31 = HttpResponse
    let raw = header_to_bytes(&header);
    src.add_raw(header, raw);

    let mut pipeline = SensorPipeline::new();
    pipeline.add_source(Box::new(src));

    let (tx, rx) = crossbeam_channel::bounded(16);
    pipeline.add_sink(Box::new(ChannelSink::new(tx)));

    let n = pipeline.tick().unwrap();
    assert_eq!(n, 1);

    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0.event_type, 31); // HttpResponse
    assert_eq!(events[0].0.pid, 42);
}

// ── 12. Mixed sandbox events ────────────────────────────────────────────

#[test]
fn test_mixed_sandbox_events_filtered() {
    // Two sources simulating two different VMs.
    let mut src_a = SimulatedSource::new("guest:vm-a");
    src_a.add_process_exec(100, "nginx");
    src_a.add_file_open(100, "nginx");

    let mut src_b = SimulatedSource::new("guest:vm-b");
    src_b.add_process_exec(200, "redis");
    src_b.add_net_connect(200, "redis");
    src_b.add_file_open(200, "redis");

    let mut pipeline = SensorPipeline::new();
    pipeline.add_source(Box::new(src_a));
    pipeline.add_source(Box::new(src_b));

    let (tx, rx) = crossbeam_channel::bounded(64);
    pipeline.add_sink(Box::new(ChannelSink::new(tx)));

    let n = pipeline.tick().unwrap();
    assert_eq!(n, 5); // 2 from vm-a + 3 from vm-b

    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(events.len(), 5);

    // Count events by PID to verify both sources contributed.
    let vm_a_count = events.iter().filter(|e| e.0.pid == 100).count();
    let vm_b_count = events.iter().filter(|e| e.0.pid == 200).count();
    assert_eq!(vm_a_count, 2);
    assert_eq!(vm_b_count, 3);

    // Verify sandbox_id propagation via source_name (third element of tuple).
    for ev in &events {
        let source_name = &ev.2;
        if ev.0.pid == 100 {
            assert_eq!(source_name, "guest:vm-a");
        } else {
            assert_eq!(source_name, "guest:vm-b");
        }
    }
}

// ── 13. JsondSink includes sandbox_id for guest events ──────────────────

#[test]
fn test_jsond_sink_sandbox_id() {
    let dir = std::env::temp_dir().join("nova-guest-jsond-test");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("guest-events.jsonl");
    let _ = std::fs::remove_file(&path);

    {
        let mut pipeline = SensorPipeline::new();

        // Guest source with sandbox_id.
        let mut guest_src = SimulatedSource::new("guest:my-vm");
        guest_src.add_process_exec(100, "nginx");
        pipeline.add_source(Box::new(guest_src));

        // Host source (no sandbox_id).
        let mut host_src = SimulatedSource::new("ebpf:process");
        host_src.add_process_exec(200, "systemd");
        pipeline.add_source(Box::new(host_src));

        let jsond_sink = JsondSink::new(&path).expect("open jsond sink");
        pipeline.add_sink(Box::new(jsond_sink));

        let n = pipeline.tick().unwrap();
        assert_eq!(n, 2);
    }

    let contents = std::fs::read_to_string(&path).expect("read events");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2);

    // Guest event should have sandbox_id.
    let v0: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    assert_eq!(v0["pid"], 100);
    assert_eq!(v0["sandbox_id"], "my-vm");

    // Host event should NOT have sandbox_id.
    let v1: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON");
    assert_eq!(v1["pid"], 200);
    assert!(v1["sandbox_id"].is_null(), "host event should not have sandbox_id");

    let _ = std::fs::remove_file(&path);
}
