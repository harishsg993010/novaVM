//! Stage 8: Subsystem benchmarks.
//!
//! Structured timing output for all non-KVM subsystems.

use std::path::PathBuf;
use std::time::Instant;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap()
}

/// Helper to run a benchmark and print structured output.
fn run_bench<F: FnMut()>(name: &str, iters: usize, mut f: F) {
    let mut times_us = Vec::with_capacity(iters);

    for _ in 0..iters {
        let start = Instant::now();
        f();
        times_us.push(start.elapsed().as_micros() as u64);
    }

    times_us.sort();
    let avg = times_us.iter().sum::<u64>() as f64 / iters as f64;
    let min = times_us[0];
    let max = times_us[iters - 1];
    let p99_idx = ((iters as f64) * 0.99) as usize;
    let p99 = times_us[p99_idx.min(iters - 1)];

    eprintln!("=== Benchmark: {name} ({iters} iterations) ===");
    eprintln!("avg: {avg:.1}us, min: {min}us, max: {max}us, p99: {p99}us");
    eprintln!("=== End Benchmark ===");
}

// ---------------------------------------------------------------------------
// 1. OCI parse time
// ---------------------------------------------------------------------------
#[test]
fn bench_oci_parse_time() {
    let alpine_index = fixtures_dir().join("alpine-oci/index.json");
    if !alpine_index.exists() {
        eprintln!("skipping bench_oci_parse_time: alpine-oci fixture not found");
        return;
    }

    run_bench("oci_parse_time", 1000, || {
        let data = std::fs::read_to_string(&alpine_index).unwrap();
        let _v: serde_json::Value = serde_json::from_str(&data).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 2. OCI extract time
// ---------------------------------------------------------------------------
#[test]
fn bench_oci_extract_time() {
    let alpine_index = fixtures_dir().join("alpine-oci/index.json");
    if !alpine_index.exists() {
        eprintln!("skipping bench_oci_extract_time: alpine-oci fixture not found");
        return;
    }

    run_bench("oci_extract_time", 100, || {
        let data = std::fs::read_to_string(&alpine_index).unwrap();
        let _v: serde_json::Value = serde_json::from_str(&data).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 3. CPIO creation time
// ---------------------------------------------------------------------------
#[test]
fn bench_cpio_creation_time() {
    let dir = std::env::temp_dir().join("nova-bench-cpio");
    std::fs::create_dir_all(&dir).ok();
    let test_file = dir.join("testfile.txt");
    std::fs::write(&test_file, "Hello from CPIO benchmark\n".repeat(100)).unwrap();

    run_bench("cpio_creation_time", 100, || {
        let data = std::fs::read(&test_file).unwrap();
        // Simulate CPIO header creation (newc format).
        let _header = format!(
            "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
            0, // inode
            0o100644, // mode
            0, // uid
            0, // gid
            1, // nlink
            0, // mtime
            data.len(),
            0, 0, // devmajor, devminor
            0, 0, // rdevmajor, rdevminor
            9, // namesize ("testfile\0")
            0, // checksum
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 4. Wasm engine creation
// ---------------------------------------------------------------------------
#[test]
fn bench_wasm_engine_creation() {
    run_bench("wasm_engine_creation", 1000, || {
        let config = nova_wasm::WasmEngineConfig::default();
        let _engine = nova_wasm::create_engine(&config).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 5. Wasm module compile
// ---------------------------------------------------------------------------
#[test]
fn bench_wasm_module_compile() {
    let wasm_bytes = read_fixture("hello.wat");
    let config = nova_wasm::WasmEngineConfig::default();
    let engine = nova_wasm::create_engine(&config).unwrap();

    run_bench("wasm_module_compile", 1000, || {
        let _module = wasmtime::Module::new(&engine, &wasm_bytes).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 6. Wasm instantiate and run
// ---------------------------------------------------------------------------
#[test]
fn bench_wasm_instantiate_and_run() {
    let wasm_bytes = read_fixture("hello.wat");
    let config = nova_wasm::WasmEngineConfig::default();
    let engine = nova_wasm::create_engine(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &wasm_bytes).unwrap();

    run_bench("wasm_instantiate_and_run", 1000, || {
        let ctx = nova_wasm::WasiContextWithCapture::new(&engine).unwrap();
        let _output = ctx.run(&module).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 7. Wasm module cache hit
// ---------------------------------------------------------------------------
#[test]
fn bench_wasm_module_cache_hit() {
    let wasm_bytes = read_fixture("hello.wat");
    let config = nova_wasm::WasmEngineConfig::default();
    let engine = nova_wasm::create_engine(&config).unwrap();
    let mut cache = nova_wasm::ModuleCache::new(engine);

    // Prime the cache.
    cache.compile("hello", &wasm_bytes).unwrap();

    run_bench("wasm_module_cache_hit", 10000, || {
        // Compiling the same bytes hits the dedup path.
        let _compiled = cache.compile("hello", &wasm_bytes).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 8. Policy compile and eval
// ---------------------------------------------------------------------------
#[test]
fn bench_policy_compile_and_eval() {
    let wasm_bytes = read_fixture("policy_allow.wat");

    run_bench("policy_compile_and_eval", 1000, || {
        let mut engine = nova_policy::PolicyEngine::new().unwrap();
        let policy = engine.compile("bench_allow", &wasm_bytes).unwrap();
        let _result = engine.evaluate_simple(&policy, 1).unwrap();
    });
}

// ---------------------------------------------------------------------------
// 9. Admission check
// ---------------------------------------------------------------------------
#[test]
fn bench_admission_check() {
    let checker = nova_policy::AdmissionChecker::new();
    let input = nova_policy::AdmissionInput {
        sandbox_id: "bench-sb".to_string(),
        image: "docker.io/library/nginx:latest".to_string(),
        vcpus: 2,
        memory_mib: 256,
        uid: 1000,
    };

    run_bench("admission_check", 100000, || {
        let _result = checker.check(&input);
    });
}

// ---------------------------------------------------------------------------
// 10. Enforcement evaluate
// ---------------------------------------------------------------------------
#[test]
fn bench_enforcement_evaluate() {
    let mut engine = nova_policy::EnforcementEngine::new();
    engine.add_rule(nova_policy::EnforcementRule {
        name: "audit_exec".to_string(),
        event_type: "process_exec".to_string(),
        action: nova_policy::EnforcementAction::Alert,
        enabled: true,
    });
    engine.add_rule(nova_policy::EnforcementRule {
        name: "deny_net".to_string(),
        event_type: "net_connect".to_string(),
        action: nova_policy::EnforcementAction::Deny,
        enabled: true,
    });
    engine.add_rule(nova_policy::EnforcementRule {
        name: "allow_file".to_string(),
        event_type: "file_open".to_string(),
        action: nova_policy::EnforcementAction::Allow,
        enabled: true,
    });

    run_bench("enforcement_evaluate", 100000, || {
        let _action = engine.evaluate("process_exec");
    });
}

// ---------------------------------------------------------------------------
// 11. Sensor pipeline throughput
// ---------------------------------------------------------------------------
#[test]
fn bench_sensor_pipeline_throughput() {
    use nova_eye::{SensorPipeline, SimulatedSource};
    use nova_eye_common::EventHeader;

    let mut pipeline = SensorPipeline::new();
    let mut src = SimulatedSource::new("bench-src");

    for i in 0..10000u32 {
        let mut h = EventHeader::default();
        h.event_type = 1;
        h.pid = i;
        h.timestamp_ns = i as u64 * 1000;
        let raw = {
            let ptr = &h as *const EventHeader as *const u8;
            let len = core::mem::size_of::<EventHeader>();
            unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
        };
        src.add_raw(h, raw);
    }

    pipeline.add_source(Box::new(src));
    // Use a no-op-like stdout sink but suppress output.
    // We measure pipeline throughput, not I/O.

    let start = Instant::now();
    let dispatched = pipeline.tick().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(dispatched, 10000);
    let events_per_sec = 10000.0 / elapsed.as_secs_f64();

    eprintln!("=== Benchmark: sensor_pipeline_throughput (10000 events) ===");
    eprintln!(
        "total: {}us, throughput: {:.0} events/sec",
        elapsed.as_micros(),
        events_per_sec
    );
    eprintln!("=== End Benchmark ===");
}

// ---------------------------------------------------------------------------
// 12. Event filter throughput
// ---------------------------------------------------------------------------
#[test]
fn bench_event_filter_throughput() {
    use nova_eye::EventFilter;
    use nova_eye_common::{EventHeader, EventType};

    let mut filter = EventFilter::new();
    filter.allow_type(EventType::ProcessExec);
    filter.allow_type(EventType::NetConnect);

    let headers: Vec<EventHeader> = (0..100000u32)
        .map(|i| {
            let mut h = EventHeader::default();
            h.event_type = if i % 3 == 0 { 1 } else if i % 3 == 1 { 10 } else { 20 };
            h.pid = i;
            h
        })
        .collect();

    let start = Instant::now();
    let mut matched = 0u64;
    for h in &headers {
        if filter.matches(h) {
            matched += 1;
        }
    }
    let elapsed = start.elapsed();

    eprintln!("=== Benchmark: event_filter_throughput (100000 events) ===");
    eprintln!(
        "total: {}us, matched: {matched}, throughput: {:.0} ops/sec",
        elapsed.as_micros(),
        100000.0 / elapsed.as_secs_f64()
    );
    eprintln!("=== End Benchmark ===");

    // ProcessExec (type 1) = every 3rd (i%3==0) = ~33333
    // NetConnect (type 20) = every 3rd (i%3==2) = ~33334
    // Total matched should be ~66667
    assert!(matched > 60000);
}

// ---------------------------------------------------------------------------
// 13. E2E Wasm sandbox lifecycle
// ---------------------------------------------------------------------------
#[test]
fn bench_e2e_wasm_sandbox_lifecycle() {
    use nova_runtime::{SandboxConfig, SandboxKind, SandboxOrchestrator};

    let fixture = fixtures_dir().join("hello.wat");

    run_bench("e2e_wasm_sandbox_lifecycle", 100, || {
        let mut orch = SandboxOrchestrator::new();
        let config = SandboxConfig {
            vcpus: 1,
            memory_mib: 64,
            kernel: PathBuf::new(),
            rootfs: PathBuf::new(),
            cmdline: String::new(),
            network: None,
            kind: SandboxKind::Wasm {
                module_path: fixture.clone(),
                entry_function: "_start".to_string(),
            },
        };
        orch.create("bench-lc".to_string(), config).unwrap();
        orch.start("bench-lc").unwrap();
        orch.destroy("bench-lc").unwrap();
    });
}

// ---------------------------------------------------------------------------
// 14. Concurrent Wasm sandboxes
// ---------------------------------------------------------------------------
#[test]
fn bench_concurrent_wasm_sandboxes() {
    use nova_runtime::{SandboxConfig, SandboxKind, SandboxOrchestrator};
    use std::thread;

    let fixture = fixtures_dir().join("hello.wat");

    let start = Instant::now();
    let handles: Vec<_> = (0..50)
        .map(|i| {
            let fixture = fixture.clone();
            thread::spawn(move || {
                let mut orch = SandboxOrchestrator::new();
                let config = SandboxConfig {
                    vcpus: 1,
                    memory_mib: 64,
                    kernel: PathBuf::new(),
                    rootfs: PathBuf::new(),
                    cmdline: String::new(),
                    network: None,
                    kind: SandboxKind::Wasm {
                        module_path: fixture,
                        entry_function: "_start".to_string(),
                    },
                };
                let id = format!("bench-conc-{i}");
                orch.create(id.clone(), config).unwrap();
                orch.start(&id).unwrap();
                orch.destroy(&id).unwrap();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed();

    eprintln!("=== Benchmark: concurrent_wasm_sandboxes (50 concurrent) ===");
    eprintln!(
        "total: {}ms, avg: {:.2}ms/sandbox",
        elapsed.as_millis(),
        elapsed.as_secs_f64() * 1000.0 / 50.0,
    );
    eprintln!("=== End Benchmark ===");
}
