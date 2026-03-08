//! NovaVM userspace eBPF sensor.
//!
//! This crate provides the userspace half of the nova-eye observability
//! stack. It loads eBPF programs via [`loader::SensorLoader`], aggregates
//! events through [`aggregator::EventAggregator`], and exports them via
//! pluggable [`pipeline::TelemetrySink`] implementations.

pub mod aggregator;
pub mod bpf_source;
pub mod error;
pub mod filter;
pub mod guest_source;
pub mod loader;
pub mod pipeline;
pub mod policy;
pub mod source;

pub use aggregator::{EventAggregator, EventHandler};
pub use error::{EyeError, Result};
pub use filter::EventFilter;
pub use loader::SensorLoader;
pub use pipeline::{ChannelSink, FileSink, GrpcSink, JsondSink, OtelSink, SensorPipeline, StdoutSink, TelemetrySink};
pub use policy::TracingPolicy;
pub use guest_source::GuestEventSource;
pub use source::{ReplaySource, SensorSource, SimulatedSource};

#[cfg(feature = "ebpf")]
pub use bpf_source::AyaBpfSource;

#[cfg(test)]
mod tests {
    use super::*;
    use nova_eye_common::EventHeader;

    /// Helper to build a dummy event header for testing.
    fn make_header(event_type: u32, pid: u32) -> EventHeader {
        let mut h = EventHeader {
            event_type,
            pid,
            tid: pid,
            uid: 1000,
            gid: 1000,
            comm: [0u8; 16],
            timestamp_ns: 123_456_789,
        };
        let name = b"test-cmd";
        h.comm[..name.len()].copy_from_slice(name);
        h
    }

    /// Helper to serialize a header to raw bytes (as it would come from eBPF).
    fn header_to_bytes(h: &EventHeader) -> Vec<u8> {
        let ptr = h as *const EventHeader as *const u8;
        let len = core::mem::size_of::<EventHeader>();
        unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
    }

    // -- SensorLoader tests --------------------------------------------------

    #[test]
    fn test_loader_load_and_attach() {
        let mut loader = SensorLoader::new();
        assert_eq!(loader.attached_count(), 0);

        loader
            .load_program("test_prog", &[0xEF, 0xBE])
            .expect("load should succeed");

        loader
            .attach_tracepoint("syscalls", "sys_enter_execve")
            .expect("attach tracepoint");
        loader
            .attach_kprobe("tcp_v4_connect")
            .expect("attach kprobe");
        loader
            .attach_uprobe("/usr/lib/libssl.so", "SSL_write")
            .expect("attach uprobe");

        assert_eq!(loader.attached_count(), 3);
    }

    #[test]
    fn test_loader_empty_bytecode_error() {
        let mut loader = SensorLoader::new();
        let err = loader.load_program("bad_prog", &[]);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("bad_prog"));
        assert!(msg.contains("empty"));
    }

    // -- EventAggregator tests -----------------------------------------------

    /// A simple handler that records events it receives.
    struct RecordingHandler {
        events: Vec<(u32, u32)>, // (event_type, pid)
    }

    impl RecordingHandler {
        fn new() -> Self {
            Self { events: Vec::new() }
        }
    }

    impl EventHandler for RecordingHandler {
        fn handle_event(&mut self, header: &EventHeader, _raw: &[u8], _source_name: &str) {
            self.events.push((header.event_type, header.pid));
        }
    }

    #[test]
    fn test_aggregator_dispatch_events() {
        let mut agg = EventAggregator::new();
        assert_eq!(agg.pending_count(), 0);

        let h1 = make_header(1, 100);
        let h2 = make_header(2, 200);

        agg.push_event(h1.clone(), header_to_bytes(&h1), String::new());
        agg.push_event(h2.clone(), header_to_bytes(&h2), String::new());
        assert_eq!(agg.pending_count(), 2);

        let mut handler = RecordingHandler::new();
        agg.process_events(&mut handler).expect("process ok");

        assert_eq!(agg.pending_count(), 0);
        assert_eq!(handler.events.len(), 2);
        assert_eq!(handler.events[0], (1, 100));
        assert_eq!(handler.events[1], (2, 200));
    }

    #[test]
    fn test_aggregator_empty_process() {
        let mut agg = EventAggregator::new();
        let mut handler = RecordingHandler::new();
        agg.process_events(&mut handler).expect("process ok");
        assert_eq!(handler.events.len(), 0);
    }

    // -- StdoutSink tests ----------------------------------------------------

    #[test]
    fn test_stdout_sink() {
        let mut sink = StdoutSink::new();
        assert_eq!(sink.event_count(), 0);

        let h = make_header(1, 42);
        let raw = header_to_bytes(&h);

        sink.send(&h, &raw, "").expect("send ok");
        sink.send(&h, &raw, "").expect("send ok");

        assert_eq!(sink.event_count(), 2);
    }

    // -- FileSink tests ------------------------------------------------------

    #[test]
    fn test_file_sink_writes_json_lines() {
        let dir = std::env::temp_dir().join("nova-eye-test");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("test_events.jsonl");

        // Clean up from prior runs.
        let _ = std::fs::remove_file(&path);

        {
            let mut sink = FileSink::new(&path).expect("open file sink");
            assert_eq!(sink.event_count(), 0);

            let h = make_header(1, 99);
            let raw = header_to_bytes(&h);
            sink.send(&h, &raw, "").expect("send ok");
            sink.send(&h, &raw, "").expect("send ok");

            assert_eq!(sink.event_count(), 2);
        }

        let contents = std::fs::read_to_string(&path).expect("read output");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line should be valid JSON.
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            assert_eq!(v["pid"], 99);
            assert_eq!(v["type"], 1);
        }

        // Clean up.
        let _ = std::fs::remove_file(&path);
    }

    // -- GrpcSink tests ------------------------------------------------------

    #[test]
    fn test_grpc_sink_placeholder() {
        let mut sink = GrpcSink::new("http://localhost:50051");
        let h = make_header(3, 77);
        let raw = header_to_bytes(&h);
        sink.send(&h, &raw, "").expect("send ok");
        // Placeholder: just verifies it doesn't panic.
    }

    // -- OtelSink tests ------------------------------------------------------

    #[test]
    fn test_otel_sink_placeholder() {
        let mut sink = OtelSink::new("http://localhost:4317", "nova-eye-test");
        let h = make_header(5, 88);
        let raw = header_to_bytes(&h);
        sink.send(&h, &raw, "").expect("send ok");
        // Placeholder: just verifies it doesn't panic.
    }
}
