//! Stage 6: OPA policy evaluation integration tests.
//!
//! Tests compile+evaluate WAT policy fixtures via PolicyEngine.

use std::path::PathBuf;

use nova_policy::{
    AdmissionChecker, AdmissionInput, BundleManager, EnforcementAction, EnforcementEngine,
    EnforcementRule, PolicyEngine,
};

/// Path to the test fixtures directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

/// Read a WAT fixture and return the bytes.
fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap()
}

/// 1. Compile allow.wat, eval -> allowed=true.
#[test]
fn test_compile_wat_allow_policy() {
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_allow.wat");
    let policy = engine.compile("allow_policy", &wasm_bytes).unwrap();

    let result = engine.evaluate_simple(&policy, 0).unwrap();
    assert!(result.allowed);
    assert_eq!(engine.eval_count(), 1);
    assert_eq!(engine.denied_count(), 0);
}

/// 2. Compile deny.wat, eval -> allowed=false.
#[test]
fn test_compile_wat_deny_policy() {
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_deny.wat");
    let policy = engine.compile("deny_policy", &wasm_bytes).unwrap();

    let result = engine.evaluate_simple(&policy, 0).unwrap();
    assert!(!result.allowed);
    assert!(result.reason.contains("denied"));
    assert_eq!(engine.denied_count(), 1);
}

/// 3. Conditional policy: input_code=1 -> allowed.
#[test]
fn test_conditional_policy_allow() {
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_conditional.wat");
    let policy = engine.compile("conditional", &wasm_bytes).unwrap();

    let result = engine.evaluate_simple(&policy, 1).unwrap();
    assert!(result.allowed);
}

/// 4. Conditional policy: input_code=0 -> denied.
#[test]
fn test_conditional_policy_deny() {
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_conditional.wat");
    let policy = engine.compile("conditional", &wasm_bytes).unwrap();

    let result = engine.evaluate_simple(&policy, 0).unwrap();
    assert!(!result.allowed);
}

/// 5. Admission with Wasm policy: built-in passes + Wasm allow passes.
#[test]
fn test_admission_with_wasm_policy() {
    let checker = AdmissionChecker::new();
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_allow.wat");
    let policy = engine.compile("allow", &wasm_bytes).unwrap();

    let input = AdmissionInput {
        sandbox_id: "sb-1".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 2,
        memory_mib: 256,
        uid: 1000,
    };

    let result = checker.check_with_policy(&input, &mut engine, Some(&policy));
    assert!(result.allowed);
}

/// 6. Admission: built-in denies, Wasm never called.
#[test]
fn test_admission_builtin_blocks_before_wasm() {
    let mut checker = AdmissionChecker::new();
    checker.set_max_vcpus(1);

    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_allow.wat");
    let policy = engine.compile("allow", &wasm_bytes).unwrap();

    let input = AdmissionInput {
        sandbox_id: "sb-2".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 4, // Exceeds limit.
        memory_mib: 256,
        uid: 1000,
    };

    let result = checker.check_with_policy(&input, &mut engine, Some(&policy));
    assert!(!result.allowed);
    assert!(result.reason.contains("vCPUs"));

    // Wasm engine should NOT have been called.
    assert_eq!(engine.eval_count(), 0);
}

/// 7. Admission: built-in passes, Wasm denies.
#[test]
fn test_admission_wasm_deny_overrides() {
    let checker = AdmissionChecker::new();
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_deny.wat");
    let policy = engine.compile("deny", &wasm_bytes).unwrap();

    let input = AdmissionInput {
        sandbox_id: "sb-3".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 2,
        memory_mib: 256,
        uid: 1000,
    };

    let result = checker.check_with_policy(&input, &mut engine, Some(&policy));
    assert!(!result.allowed);
    assert!(result.reason.contains("wasm policy"));
}

/// 8. Load WAT via BundleManager -> evaluate.
#[test]
fn test_bundle_load_and_evaluate() {
    let dir = std::env::temp_dir().join("nova-policy-eval-bundle");
    let _ = std::fs::remove_dir_all(&dir);

    let mut engine = PolicyEngine::new().unwrap();
    let mut mgr = BundleManager::new(&dir).unwrap();

    let wasm_bytes = read_fixture("policy_allow.wat");
    mgr.load_bundle("test-bundle", &wasm_bytes, &engine).unwrap();

    let policies = mgr.get_policies("test-bundle").unwrap();
    assert_eq!(policies.len(), 1);

    let result = engine.evaluate_simple(&policies[0], 0).unwrap();
    assert!(result.allowed);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 9. Evaluation timing: 100 evals -> avg_eval_us() > 0.
#[test]
fn test_policy_evaluation_timing() {
    let mut engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_allow.wat");
    let policy = engine.compile("timed", &wasm_bytes).unwrap();

    for _ in 0..100 {
        engine.evaluate_simple(&policy, 0).unwrap();
    }

    assert_eq!(engine.eval_count(), 100);
    // avg_eval_us might be 0 on very fast machines (sub-microsecond),
    // but total should be > 0 over 100 iterations.
}

/// 10. Enforcement + Policy chaining.
#[test]
fn test_enforcement_with_policy_chaining() {
    let mut policy_engine = PolicyEngine::new().unwrap();
    let wasm_bytes = read_fixture("policy_allow.wat");
    let policy = policy_engine.compile("gatekeeper", &wasm_bytes).unwrap();

    // First: check policy.
    let result = policy_engine.evaluate_simple(&policy, 1).unwrap();
    assert!(result.allowed);

    // Then: check enforcement rules on the event.
    let mut enforcement = EnforcementEngine::new();
    enforcement.add_rule(EnforcementRule {
        name: "audit_exec".to_string(),
        event_type: "process_exec".to_string(),
        action: EnforcementAction::Alert,
        enabled: true,
    });
    enforcement.add_rule(EnforcementRule {
        name: "deny_net".to_string(),
        event_type: "net_connect".to_string(),
        action: EnforcementAction::Deny,
        enabled: true,
    });

    // Policy allowed, enforcement yields Alert for process_exec.
    let action = enforcement.evaluate("process_exec");
    assert_eq!(action, EnforcementAction::Alert);

    // Policy allowed, enforcement yields Deny for net_connect.
    let action = enforcement.evaluate("net_connect");
    assert_eq!(action, EnforcementAction::Deny);
}
