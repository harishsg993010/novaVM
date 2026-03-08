//! Stage 4: Wasm sandbox integration tests.
//!
//! Tests that `SandboxOrchestrator` can create, start, stop, and destroy
//! Wasm sandboxes via the `nova-wasm` engine.

use std::path::PathBuf;

use nova_runtime::{SandboxConfig, SandboxKind, SandboxOrchestrator, SandboxState};

/// Path to the test fixtures directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

/// Build a Wasm sandbox config.
fn wasm_config(module_path: PathBuf, entry: &str) -> SandboxConfig {
    SandboxConfig {
        vcpus: 1,
        memory_mib: 64,
        kernel: PathBuf::new(),
        rootfs: PathBuf::new(),
        cmdline: String::new(),
        network: None,
        kind: SandboxKind::Wasm {
            module_path,
            entry_function: entry.to_string(),
        },
    }
}

/// 1. Create sandbox with SandboxKind::Wasm, verify state=Created.
#[test]
fn test_wasm_sandbox_create() {
    let mut orch = SandboxOrchestrator::new();
    let config = wasm_config(fixtures_dir().join("hello.wat"), "_start");

    orch.create("wasm-1".to_string(), config).unwrap();
    let sb = orch.get("wasm-1").unwrap();
    assert_eq!(sb.state(), SandboxState::Created);

    match sb.config().kind {
        SandboxKind::Wasm { .. } => {}
        _ => panic!("expected Wasm kind"),
    }
}

/// 2. Start with hello.wat, verify captured stdout.
#[test]
fn test_wasm_sandbox_start_hello() {
    let mut orch = SandboxOrchestrator::new();
    let config = wasm_config(fixtures_dir().join("hello.wat"), "_start");

    orch.create("wasm-hello".to_string(), config).unwrap();
    orch.start("wasm-hello").unwrap();

    let sb = orch.get("wasm-hello").unwrap();
    // Wasm sandboxes execute synchronously and transition to Stopped.
    assert_eq!(sb.state(), SandboxState::Stopped);

    let output = sb.wasm_output().expect("should have captured stdout");
    assert_eq!(output, "Hello from Wasm\n");
}

/// 3. Full lifecycle: create -> start -> stop -> destroy.
#[test]
fn test_wasm_sandbox_lifecycle() {
    let mut orch = SandboxOrchestrator::new();
    let config = wasm_config(fixtures_dir().join("hello.wat"), "_start");

    orch.create("wasm-lc".to_string(), config).unwrap();
    assert_eq!(orch.get("wasm-lc").unwrap().state(), SandboxState::Created);

    orch.start("wasm-lc").unwrap();
    // Wasm runs synchronously, ends up Stopped.
    assert_eq!(orch.get("wasm-lc").unwrap().state(), SandboxState::Stopped);

    // Destroy (already stopped).
    orch.destroy("wasm-lc").unwrap();
    assert!(orch.get("wasm-lc").is_err());
}

/// 4. Bad Wasm bytes -> meaningful error.
#[test]
fn test_wasm_sandbox_invalid_module() {
    let dir = std::env::temp_dir().join("nova-wasm-test-invalid");
    std::fs::create_dir_all(&dir).ok();
    let bad_path = dir.join("bad.wasm");
    std::fs::write(&bad_path, b"not-valid-wasm").unwrap();

    let mut orch = SandboxOrchestrator::new();
    let config = wasm_config(bad_path, "_start");

    orch.create("wasm-bad".to_string(), config).unwrap();
    let err = orch.start("wasm-bad");
    assert!(err.is_err());

    let msg = format!("{}", err.unwrap_err());
    assert!(msg.contains("wasm"), "error should mention wasm: {msg}");

    // Sandbox should be in Error state.
    let sb = orch.get("wasm-bad").unwrap();
    assert_eq!(sb.state(), SandboxState::Error);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 5. Nonexistent path -> error.
#[test]
fn test_wasm_sandbox_missing_file() {
    let mut orch = SandboxOrchestrator::new();
    let config = wasm_config(PathBuf::from("/nonexistent/module.wasm"), "_start");

    orch.create("wasm-missing".to_string(), config).unwrap();
    let err = orch.start("wasm-missing");
    assert!(err.is_err());

    let sb = orch.get("wasm-missing").unwrap();
    assert_eq!(sb.state(), SandboxState::Error);
}

/// 6. Concurrent Wasm sandboxes.
#[test]
fn test_concurrent_wasm_sandboxes() {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let results: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let fixture = fixtures_dir().join("hello.wat");

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let results = results.clone();
            let fixture = fixture.clone();
            thread::spawn(move || {
                let mut orch = SandboxOrchestrator::new();
                let id = format!("wasm-concurrent-{i}");
                let config = wasm_config(fixture, "_start");

                orch.create(id.clone(), config).unwrap();
                let ok = orch.start(&id).is_ok();
                results.lock().unwrap().push((id, ok));
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let results = results.lock().unwrap();
    assert_eq!(results.len(), 10);
    assert!(results.iter().all(|(_, ok)| *ok));
}

/// 7. Pre-flight check: compile module to validate before VM decision.
#[test]
fn test_wasm_preflight_check() {
    let fixture = fixtures_dir().join("hello.wat");
    let wasm_bytes = std::fs::read(&fixture).unwrap();

    let config = nova_wasm::WasmEngineConfig::default();
    let engine = nova_wasm::create_engine(&config).unwrap();

    // Pre-flight: attempt to compile.
    let result = wasmtime::Module::new(&engine, &wasm_bytes);
    assert!(result.is_ok(), "pre-flight compile should succeed");

    // Bad bytes should fail pre-flight.
    let bad_result = wasmtime::Module::new(&engine, b"not-wasm");
    assert!(bad_result.is_err());
}

/// 8. Load add.wat, call "add" export.
#[test]
fn test_wasm_sandbox_with_add_fixture() {
    let mut orch = SandboxOrchestrator::new();
    let config = wasm_config(fixtures_dir().join("add.wat"), "add");

    orch.create("wasm-add".to_string(), config).unwrap();
    orch.start("wasm-add").unwrap();

    let sb = orch.get("wasm-add").unwrap();
    assert_eq!(sb.state(), SandboxState::Stopped);

    // The add function was called with default params (0, 0) -> result [0].
    let result = sb.wasm_result().expect("should have return values");
    assert_eq!(result, &[0i64]); // add(0, 0) = 0
}
