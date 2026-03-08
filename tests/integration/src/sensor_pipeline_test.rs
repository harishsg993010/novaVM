//! Stage 5: Sensor pipeline integration tests.
//!
//! Tests the full sensor pipeline: sources, filters, aggregator, sinks.

use nova_eye::{
    EventFilter, FileSink, SensorPipeline, SimulatedSource, StdoutSink,
};
use nova_eye_common::{EventHeader, EventType};

/// Helper to build a dummy EventHeader.
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

/// 1. SimulatedSource emits events.
#[test]
fn test_simulated_source_emits_events() {
    use nova_eye::SensorSource;

    let mut source = SimulatedSource::new("test-src");
    source.add_process_exec(100, "bash");
    source.add_file_open(101, "cat");
    source.add_net_connect(102, "curl");
    source.add_process_exec(103, "ls");
    source.add_process_exec(104, "ps");

    let events = source.poll_events().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0].0.event_type, EventType::ProcessExec as u32);
    assert_eq!(events[1].0.event_type, EventType::FileOpen as u32);
    assert_eq!(events[2].0.event_type, EventType::NetConnect as u32);

    // After poll, source is drained.
    let events2 = source.poll_events().unwrap();
    assert!(events2.is_empty());
}

/// 2. EventFilter by type.
#[test]
fn test_event_filter_by_type() {
    let mut filter = EventFilter::new();
    filter.allow_type(EventType::ProcessExec);

    let exec_h = make_header(EventType::ProcessExec as u32, 1, "a");
    let file_h = make_header(EventType::FileOpen as u32, 2, "b");

    assert!(filter.matches(&exec_h));
    assert!(!filter.matches(&file_h));
}

/// 3. EventFilter by PID.
#[test]
fn test_event_filter_by_pid() {
    let mut filter = EventFilter::new();
    filter.allow_pid(100);

    let h100 = make_header(EventType::ProcessExec as u32, 100, "a");
    let h200 = make_header(EventType::ProcessExec as u32, 200, "b");

    assert!(filter.matches(&h100));
    assert!(!filter.matches(&h200));
}

/// 4. Full pipeline: source -> file sink, verify JSON lines.
#[test]
fn test_pipeline_source_to_file_sink() {
    let dir = std::env::temp_dir().join("nova-sensor-test-pipeline");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("pipeline_events.jsonl");
    let _ = std::fs::remove_file(&path);

    let mut pipeline = SensorPipeline::new();

    let mut src = SimulatedSource::new("src-1");
    src.add_process_exec(42, "bash");
    src.add_file_open(43, "cat");
    src.add_net_connect(44, "curl");

    pipeline.add_source(Box::new(src));
    pipeline.add_sink(Box::new(FileSink::new(&path).unwrap()));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 3);

    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3);

    // Each line is valid JSON.
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["pid"].as_u64().unwrap() > 0);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 5. Multiple sources.
#[test]
fn test_pipeline_multiple_sources() {
    let dir = std::env::temp_dir().join("nova-sensor-test-multi-src");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("multi_src.jsonl");
    let _ = std::fs::remove_file(&path);

    let mut pipeline = SensorPipeline::new();

    let mut src1 = SimulatedSource::new("alpha");
    src1.add_process_exec(10, "a");
    src1.add_process_exec(11, "b");

    let mut src2 = SimulatedSource::new("beta");
    src2.add_net_connect(20, "c");
    src2.add_net_connect(21, "d");
    src2.add_net_connect(22, "e");

    pipeline.add_source(Box::new(src1));
    pipeline.add_source(Box::new(src2));
    pipeline.add_sink(Box::new(FileSink::new(&path).unwrap()));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 5);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 6. Pipeline with filter.
#[test]
fn test_pipeline_with_filter() {
    let dir = std::env::temp_dir().join("nova-sensor-test-filter");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("filtered.jsonl");
    let _ = std::fs::remove_file(&path);

    let mut pipeline = SensorPipeline::new();

    let mut src = SimulatedSource::new("mixed");
    src.add_process_exec(1, "a");
    src.add_file_open(2, "b");
    src.add_net_connect(3, "c");
    src.add_process_exec(4, "d");

    let mut filter = EventFilter::new();
    filter.allow_type(EventType::NetConnect);

    pipeline.add_source(Box::new(src));
    pipeline.set_filter(filter);
    pipeline.add_sink(Box::new(FileSink::new(&path).unwrap()));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 1); // Only net_connect passes.

    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 1);
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["type"].as_u64().unwrap(), EventType::NetConnect as u64);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 7. Multiple sinks.
#[test]
fn test_pipeline_multiple_sinks() {
    let dir = std::env::temp_dir().join("nova-sensor-test-multi-sink");
    std::fs::create_dir_all(&dir).ok();
    let path1 = dir.join("sink1.jsonl");
    let path2 = dir.join("sink2.jsonl");
    let _ = std::fs::remove_file(&path1);
    let _ = std::fs::remove_file(&path2);

    let mut pipeline = SensorPipeline::new();

    let mut src = SimulatedSource::new("src");
    src.add_process_exec(50, "test");

    pipeline.add_source(Box::new(src));
    pipeline.add_sink(Box::new(FileSink::new(&path1).unwrap()));
    pipeline.add_sink(Box::new(FileSink::new(&path2).unwrap()));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 1);

    // Both sinks should have the event.
    assert_eq!(std::fs::read_to_string(&path1).unwrap().lines().count(), 1);
    assert_eq!(std::fs::read_to_string(&path2).unwrap().lines().count(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 8. Concurrent events pushed to aggregator.
#[test]
fn test_pipeline_concurrent_events() {
    use nova_eye::EventAggregator;
    use std::sync::{Arc, Mutex};
    use std::thread;

    let agg = Arc::new(Mutex::new(EventAggregator::new()));

    let handles: Vec<_> = (0..4)
        .map(|t| {
            let agg = agg.clone();
            thread::spawn(move || {
                for i in 0..25 {
                    let pid = (t * 100 + i) as u32;
                    let h = make_header(EventType::ProcessExec as u32, pid, "th");
                    let raw = header_to_bytes(&h);
                    agg.lock().unwrap().push_event(h, raw, String::new());
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let agg = agg.lock().unwrap();
    assert_eq!(agg.pending_count(), 100);
}

/// 9. ReplaySource from JSON lines file.
#[test]
fn test_replay_source_from_file() {
    use nova_eye::{ReplaySource, SensorSource};

    let dir = std::env::temp_dir().join("nova-sensor-test-replay");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("replay.jsonl");

    // Write some JSON lines.
    let lines = vec![
        r#"{"type":1,"pid":100,"tid":100,"uid":1000,"comm":"bash","ts":111,"raw_len":48}"#,
        r#"{"type":10,"pid":200,"tid":200,"uid":1000,"comm":"cat","ts":222,"raw_len":48}"#,
        r#"{"type":20,"pid":300,"tid":300,"uid":1000,"comm":"curl","ts":333,"raw_len":48}"#,
    ];
    std::fs::write(&path, lines.join("\n")).unwrap();

    let mut source = ReplaySource::from_file(&path).unwrap();
    let events = source.poll_events().unwrap();

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].0.event_type, 1);
    assert_eq!(events[0].0.pid, 100);
    assert_eq!(events[1].0.event_type, 10);
    assert_eq!(events[2].0.event_type, 20);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 10. High-throughput: 10,000 events through pipeline.
#[test]
fn test_pipeline_high_throughput() {
    let mut pipeline = SensorPipeline::new();

    let mut src = SimulatedSource::new("high-throughput");
    for i in 0..10_000 {
        let h = make_header(EventType::ProcessExec as u32, i, "ht");
        let raw = header_to_bytes(&h);
        src.add_raw(h, raw);
    }

    pipeline.add_source(Box::new(src));
    pipeline.add_sink(Box::new(StdoutSink::new()));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 10_000);
}
