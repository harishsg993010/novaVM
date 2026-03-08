//! Policy engine integration tests.
//!
//! Tests admission control, enforcement rules, and bundle management
//! working together as a complete policy pipeline.

use nova_policy::{
    AdmissionChecker, AdmissionInput, BundleManager, EnforcementAction, EnforcementEngine,
    EnforcementRule, PolicyEngine,
};

/// Test the full admission control pipeline with resource limits.
#[test]
fn test_admission_resource_limits() {
    let mut checker = AdmissionChecker::new();
    checker.set_max_vcpus(4);
    checker.set_max_memory_mib(1024);
    checker.set_max_sandboxes(10);

    // Request within limits should pass.
    let input = AdmissionInput {
        sandbox_id: "sb-1".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 2,
        memory_mib: 512,
        uid: 1000,
    };
    let result = checker.check(&input);
    assert!(result.allowed, "should allow request within limits");

    // Request exceeding vCPU limit should be denied.
    let input = AdmissionInput {
        sandbox_id: "sb-2".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 8,
        memory_mib: 512,
        uid: 1000,
    };
    let result = checker.check(&input);
    assert!(!result.allowed, "should deny excessive vCPUs");
    assert!(!result.reason.is_empty());
}

/// Test admission with image allowlist.
#[test]
fn test_admission_image_allowlist() {
    let mut checker = AdmissionChecker::new();
    checker.add_allowed_image("nginx:latest");
    checker.add_allowed_image("alpine:latest");

    // Allowed image.
    let input = AdmissionInput {
        sandbox_id: "sb-1".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    assert!(checker.check(&input).allowed);

    // Blocked image.
    let input = AdmissionInput {
        sandbox_id: "sb-2".to_string(),
        image: "malicious:latest".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    let result = checker.check(&input);
    assert!(!result.allowed, "should deny non-allowlisted image");
}

/// Test the enforcement engine with multiple rules and evaluation.
#[test]
fn test_enforcement_evaluate() {
    let mut engine = EnforcementEngine::new();

    engine.add_rule(EnforcementRule {
        name: "audit-exec".to_string(),
        event_type: "process_exec".to_string(),
        action: EnforcementAction::Alert,
        enabled: true,
    });

    engine.add_rule(EnforcementRule {
        name: "deny-net".to_string(),
        event_type: "net_connect".to_string(),
        action: EnforcementAction::Deny,
        enabled: true,
    });

    assert_eq!(engine.rules().len(), 2);

    // Evaluate a process_exec event — should get Alert.
    let action = engine.evaluate("process_exec");
    assert_eq!(action, EnforcementAction::Alert);

    // Evaluate a net_connect event — should get Deny.
    let action = engine.evaluate("net_connect");
    assert_eq!(action, EnforcementAction::Deny);

    // Evaluate an unmatched event — should get Allow.
    let action = engine.evaluate("file_open");
    assert_eq!(action, EnforcementAction::Allow);

    assert_eq!(engine.decision_count(), 3);
    assert_eq!(engine.denial_count(), 1);
    assert_eq!(engine.alert_count(), 1);
}

/// Test enforcement with wildcard rules.
#[test]
fn test_enforcement_wildcard_rule() {
    let mut engine = EnforcementEngine::new();

    engine.add_rule(EnforcementRule {
        name: "catch-all".to_string(),
        event_type: "*".to_string(),
        action: EnforcementAction::Alert,
        enabled: true,
    });

    // Any event should match the wildcard.
    assert_eq!(engine.evaluate("process_exec"), EnforcementAction::Alert);
    assert_eq!(engine.evaluate("net_connect"), EnforcementAction::Alert);
    assert_eq!(engine.evaluate("file_open"), EnforcementAction::Alert);
}

/// Test that disabled rules are skipped.
#[test]
fn test_enforcement_disabled_rules() {
    let mut engine = EnforcementEngine::new();

    engine.add_rule(EnforcementRule {
        name: "deny-all".to_string(),
        event_type: "*".to_string(),
        action: EnforcementAction::Kill,
        enabled: false,
    });

    // Disabled rule should not match.
    assert_eq!(engine.evaluate("process_exec"), EnforcementAction::Allow);
}

/// Test builtin rules are well-formed.
#[test]
fn test_builtin_rules() {
    let default_rules = nova_policy::builtin::default_rules();
    assert!(!default_rules.is_empty(), "should have default rules");

    for rule in &default_rules {
        assert!(!rule.name.is_empty(), "rule name should not be empty");
        assert!(
            !rule.event_type.is_empty(),
            "rule event_type should not be empty"
        );
    }

    let strict_rules = nova_policy::builtin::strict_rules();
    assert!(
        strict_rules.len() >= default_rules.len(),
        "strict should have at least as many rules"
    );
}

/// Test bundle manager lifecycle.
#[test]
fn test_bundle_manager_lifecycle() {
    let dir = std::env::temp_dir().join("nova-policy-integ-test");
    let _ = std::fs::remove_dir_all(&dir);

    let mut manager = BundleManager::new(&dir).unwrap();
    let engine = PolicyEngine::new().unwrap();

    // Minimal valid Wasm module.
    let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];

    // Load a bundle.
    manager
        .load_bundle("test-bundle", &wasm_bytes, &engine)
        .unwrap();

    // Query it.
    let info = manager.get_info("test-bundle").unwrap();
    assert_eq!(info.bundle_id, "test-bundle");
    assert_eq!(info.policy_count, 1);

    // List bundles.
    let bundles = manager.list_bundles();
    assert_eq!(bundles.len(), 1);

    // Remove it.
    manager.remove_bundle("test-bundle").unwrap();
    assert!(manager.get_info("test-bundle").is_none());
    assert!(manager.list_bundles().is_empty());

    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}

/// Test that admission + enforcement work together as a pipeline.
#[test]
fn test_admission_then_enforcement_pipeline() {
    // Step 1: Admission check.
    let mut checker = AdmissionChecker::new();
    checker.set_max_vcpus(8);
    checker.set_max_memory_mib(2048);

    let input = AdmissionInput {
        sandbox_id: "pipeline-sb".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 4,
        memory_mib: 1024,
        uid: 1000,
    };

    let admission_result = checker.check(&input);
    assert!(admission_result.allowed, "admission should pass");

    // Step 2: If admission passes, check enforcement rules.
    let mut enforcement = EnforcementEngine::new();
    for rule in nova_policy::builtin::default_rules() {
        enforcement.add_rule(rule);
    }

    // Evaluate against default rules.
    let action = enforcement.evaluate("process_exec");
    // Default rules have an Alert for process_exec.
    assert_eq!(action, EnforcementAction::Alert);
}

/// Test the policy engine creation and compilation.
#[test]
fn test_policy_engine_compile() {
    let engine = PolicyEngine::new().unwrap();

    // Minimal valid Wasm module.
    let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];

    let compiled = engine.compile("test-policy", &wasm_bytes);
    assert!(compiled.is_ok(), "should compile minimal wasm");
}

/// Test that sandbox count limit is enforced.
#[test]
fn test_admission_sandbox_count_limit() {
    let mut checker = AdmissionChecker::new();
    checker.set_max_sandboxes(5);
    checker.set_current_sandboxes(5);

    let input = AdmissionInput {
        sandbox_id: "sb-overflow".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    let result = checker.check(&input);
    assert!(!result.allowed, "should deny when at sandbox limit");
}
