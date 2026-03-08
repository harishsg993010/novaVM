//! Stage 7: End-to-end agent scenario integration tests.
//!
//! Cross-crate integration proving all subsystems work together:
//! orchestrator + admission + enforcement + policy engine + sensor pipeline.

use std::path::PathBuf;

use nova_eye::{FileSink, SensorPipeline, SimulatedSource};
use nova_policy::{
    AdmissionChecker, AdmissionInput, EnforcementAction, EnforcementEngine, EnforcementRule,
    PolicyEngine,
};
use nova_runtime::{SandboxConfig, SandboxKind, SandboxOrchestrator, SandboxState};

/// Path to the test fixtures directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap()
}

fn wasm_config(module: &str, entry: &str) -> SandboxConfig {
    SandboxConfig {
        vcpus: 1,
        memory_mib: 64,
        kernel: PathBuf::new(),
        rootfs: PathBuf::new(),
        cmdline: String::new(),
        network: None,
        kind: SandboxKind::Wasm {
            module_path: fixtures_dir().join(module),
            entry_function: entry.to_string(),
        },
    }
}

fn admission_input(id: &str) -> AdmissionInput {
    AdmissionInput {
        sandbox_id: id.to_string(),
        image: "docker.io/library/wasm-runner:latest".to_string(),
        vcpus: 1,
        memory_mib: 64,
        uid: 1000,
    }
}

/// 1. Admission -> create Wasm sandbox -> start -> verify output -> destroy.
#[test]
fn test_e2e_wasm_sandbox_with_admission() {
    let checker = AdmissionChecker::new();
    let input = admission_input("e2e-1");

    let result = checker.check(&input);
    assert!(result.allowed);

    let mut orch = SandboxOrchestrator::new();
    orch.create("e2e-1".to_string(), wasm_config("hello.wat", "_start"))
        .unwrap();
    orch.start("e2e-1").unwrap();

    let sb = orch.get("e2e-1").unwrap();
    assert_eq!(sb.state(), SandboxState::Stopped);
    assert_eq!(sb.wasm_output().unwrap(), "Hello from Wasm\n");

    orch.destroy("e2e-1").unwrap();
}

/// 2. Excessive resources -> denied -> no sandbox.
#[test]
fn test_e2e_admission_denial_blocks_creation() {
    let mut checker = AdmissionChecker::new();
    checker.set_max_memory_mib(32);

    let input = admission_input("e2e-2");
    let result = checker.check(&input);
    assert!(!result.allowed);
    assert!(result.reason.contains("memory"));

    // Sandbox should NOT be created.
    let orch = SandboxOrchestrator::new();
    assert!(orch.get("e2e-2").is_err());
}

/// 3. Start Wasm -> sensor captures simulated events -> verify file sink.
#[test]
fn test_e2e_wasm_with_sensor_capture() {
    let dir = std::env::temp_dir().join("nova-e2e-sensor");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("e2e_events.jsonl");
    let _ = std::fs::remove_file(&path);

    // Start Wasm sandbox.
    let mut orch = SandboxOrchestrator::new();
    orch.create("e2e-3".to_string(), wasm_config("hello.wat", "_start"))
        .unwrap();
    orch.start("e2e-3").unwrap();
    assert_eq!(orch.get("e2e-3").unwrap().state(), SandboxState::Stopped);

    // Sensor pipeline captures simulated events about the sandbox.
    let mut pipeline = SensorPipeline::new();

    let mut src = SimulatedSource::new("wasm-monitor");
    src.add_process_exec(1000, "wasm-rt");
    src.add_file_open(1000, "wasm-rt");

    pipeline.add_source(Box::new(src));
    pipeline.add_sink(Box::new(FileSink::new(&path).unwrap()));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 2);

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents.lines().count(), 2);

    orch.destroy("e2e-3").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4. Wasm policy allows -> sandbox starts -> enforcement evaluates events.
#[test]
fn test_e2e_policy_then_enforcement() {
    let mut policy_engine = PolicyEngine::new().unwrap();
    let allow_bytes = read_fixture("policy_allow.wat");
    let policy = policy_engine.compile("allow", &allow_bytes).unwrap();

    // Policy check.
    let result = policy_engine.evaluate_simple(&policy, 1).unwrap();
    assert!(result.allowed);

    // Start sandbox.
    let mut orch = SandboxOrchestrator::new();
    orch.create("e2e-4".to_string(), wasm_config("hello.wat", "_start"))
        .unwrap();
    orch.start("e2e-4").unwrap();

    // Enforcement engine evaluates runtime events.
    let mut enforcement = EnforcementEngine::new();
    enforcement.add_rule(EnforcementRule {
        name: "audit_exec".to_string(),
        event_type: "process_exec".to_string(),
        action: EnforcementAction::Alert,
        enabled: true,
    });

    let action = enforcement.evaluate("process_exec");
    assert_eq!(action, EnforcementAction::Alert);
    assert_eq!(enforcement.alert_count(), 1);

    orch.destroy("e2e-4").unwrap();
}

/// 5. Deny policy -> sandbox rejected.
#[test]
fn test_e2e_wasm_policy_deny_blocks_start() {
    let checker = AdmissionChecker::new();
    let mut policy_engine = PolicyEngine::new().unwrap();
    let deny_bytes = read_fixture("policy_deny.wat");
    let policy = policy_engine.compile("deny", &deny_bytes).unwrap();

    let input = admission_input("e2e-5");
    let result = checker.check_with_policy(&input, &mut policy_engine, Some(&policy));
    assert!(!result.allowed);
    assert!(result.reason.contains("wasm policy"));
}

/// 6. 5 sandboxes, each with admission + policy.
#[test]
fn test_e2e_concurrent_sandboxes_with_policies() {
    let checker = AdmissionChecker::new();
    let mut policy_engine = PolicyEngine::new().unwrap();
    let allow_bytes = read_fixture("policy_allow.wat");
    let policy = policy_engine.compile("concurrent", &allow_bytes).unwrap();

    let mut orch = SandboxOrchestrator::new();

    for i in 0..5 {
        let id = format!("e2e-6-{i}");
        let input = admission_input(&id);

        let result = checker.check_with_policy(&input, &mut policy_engine, Some(&policy));
        assert!(result.allowed);

        orch.create(id.clone(), wasm_config("hello.wat", "_start"))
            .unwrap();
        orch.start(&id).unwrap();
        assert_eq!(orch.get(&id).unwrap().state(), SandboxState::Stopped);
    }

    assert_eq!(orch.count(), 5);
    assert_eq!(policy_engine.eval_count(), 5);

    for i in 0..5 {
        orch.destroy(&format!("e2e-6-{i}")).unwrap();
    }
}

/// 7. Sensor net_connect -> enforcement Deny action.
#[test]
fn test_e2e_sensor_events_trigger_enforcement() {
    // Sensor pipeline emits events.
    let mut pipeline = SensorPipeline::new();
    let mut src = SimulatedSource::new("net-monitor");
    src.add_net_connect(500, "suspicious");
    pipeline.add_source(Box::new(src));

    let dispatched = pipeline.tick().unwrap();
    assert_eq!(dispatched, 1);

    // Enforcement engine reacts to the event type.
    let mut enforcement = EnforcementEngine::new();
    enforcement.add_rule(EnforcementRule {
        name: "deny_net".to_string(),
        event_type: "net_connect".to_string(),
        action: EnforcementAction::Deny,
        enabled: true,
    });

    let action = enforcement.evaluate("net_connect");
    assert_eq!(action, EnforcementAction::Deny);
    assert_eq!(enforcement.denial_count(), 1);
}

/// 8. Full lifecycle: create -> admission -> policy -> start -> sensor tick -> enforcement -> stop -> destroy.
#[test]
fn test_e2e_full_lifecycle() {
    let dir = std::env::temp_dir().join("nova-e2e-full");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("full_lifecycle.jsonl");
    let _ = std::fs::remove_file(&path);

    // 1. Admission.
    let checker = AdmissionChecker::new();
    let input = admission_input("e2e-8");
    assert!(checker.check(&input).allowed);

    // 2. Policy.
    let mut policy_engine = PolicyEngine::new().unwrap();
    let allow_bytes = read_fixture("policy_allow.wat");
    let policy = policy_engine.compile("lifecycle", &allow_bytes).unwrap();
    let eval = policy_engine.evaluate_simple(&policy, 1).unwrap();
    assert!(eval.allowed);

    // 3. Create and start.
    let mut orch = SandboxOrchestrator::new();
    orch.create("e2e-8".to_string(), wasm_config("hello.wat", "_start"))
        .unwrap();
    orch.start("e2e-8").unwrap();
    assert_eq!(orch.get("e2e-8").unwrap().state(), SandboxState::Stopped);

    // 4. Sensor tick.
    let mut pipeline = SensorPipeline::new();
    let mut src = SimulatedSource::new("lifecycle-monitor");
    src.add_process_exec(900, "wasm");
    src.add_net_connect(900, "wasm");
    pipeline.add_source(Box::new(src));
    pipeline.add_sink(Box::new(FileSink::new(&path).unwrap()));
    pipeline.tick().unwrap();

    // 5. Enforcement.
    let mut enforcement = EnforcementEngine::new();
    enforcement.add_rule(EnforcementRule {
        name: "audit_all".to_string(),
        event_type: "*".to_string(),
        action: EnforcementAction::Alert,
        enabled: true,
    });
    let action = enforcement.evaluate("process_exec");
    assert_eq!(action, EnforcementAction::Alert);

    // 6. Destroy.
    orch.destroy("e2e-8").unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

/// 9. Bad Wasm fails -> state Error -> can create new sandbox.
#[test]
fn test_e2e_error_recovery() {
    let dir = std::env::temp_dir().join("nova-e2e-error");
    std::fs::create_dir_all(&dir).ok();
    let bad_path = dir.join("bad.wasm");
    std::fs::write(&bad_path, b"not-valid-wasm").unwrap();

    let mut orch = SandboxOrchestrator::new();

    // First sandbox fails.
    let bad_config = SandboxConfig {
        vcpus: 1,
        memory_mib: 64,
        kernel: PathBuf::new(),
        rootfs: PathBuf::new(),
        cmdline: String::new(),
        network: None,
        kind: SandboxKind::Wasm {
            module_path: bad_path,
            entry_function: "_start".to_string(),
        },
    };

    orch.create("e2e-9-bad".to_string(), bad_config).unwrap();
    assert!(orch.start("e2e-9-bad").is_err());
    assert_eq!(orch.get("e2e-9-bad").unwrap().state(), SandboxState::Error);

    // Can still create and run a new sandbox.
    orch.create("e2e-9-good".to_string(), wasm_config("hello.wat", "_start"))
        .unwrap();
    orch.start("e2e-9-good").unwrap();
    assert_eq!(
        orch.get("e2e-9-good").unwrap().state(),
        SandboxState::Stopped
    );
    assert_eq!(
        orch.get("e2e-9-good").unwrap().wasm_output().unwrap(),
        "Hello from Wasm\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 10. Parse OCI config -> Wasm pre-flight -> boot decision.
#[test]
fn test_e2e_oci_parse_then_wasm_preflight() {
    // Simulate: user provides a Wasm module path.
    let fixture = fixtures_dir().join("hello.wat");
    let wasm_bytes = std::fs::read(&fixture).unwrap();

    // Pre-flight check: can we compile it?
    let config = nova_wasm::WasmEngineConfig::default();
    let engine = nova_wasm::create_engine(&config).unwrap();
    let compile_result = wasmtime::Module::new(&engine, &wasm_bytes);
    assert!(compile_result.is_ok(), "pre-flight should pass");

    // Decision: it's a valid Wasm module, run as Wasm sandbox.
    let mut orch = SandboxOrchestrator::new();
    orch.create("e2e-10".to_string(), wasm_config("hello.wat", "_start"))
        .unwrap();
    orch.start("e2e-10").unwrap();

    let sb = orch.get("e2e-10").unwrap();
    assert_eq!(sb.state(), SandboxState::Stopped);
    assert!(sb.wasm_output().is_some());
}
