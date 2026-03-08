//! Stage 12 integration tests — OPA policy daemon integration.
//!
//! Verifies PolicyConfig parsing, PolicyState initialization, admission
//! control, enforcement rules, and bundle lifecycle.

use nova_api::config::{DaemonConfig, PolicyConfig};
use nova_api::policy_server::PolicyState;
use nova_policy::{
    AdmissionInput, BundleManager, EnforcementAction, EnforcementEngine, PolicyEngine,
};

// ── 1. Config parsing ────────────────────────────────────────────────

#[test]
fn test_policy_config_parse_toml() {
    let toml = r#"
[daemon]
socket = "/tmp/nova.sock"

[policy]
admission_enabled = true
enforcement_enabled = true
max_vcpus = 4
max_memory_mib = 2048
max_sandboxes = 50
allowed_images = ["docker.io/library/", "ghcr.io/novavm/"]
bundle_dir = "/tmp/nova-bundles"
enforcement_rules = "strict"
"#;
    let cfg = DaemonConfig::from_toml(toml).unwrap();
    assert!(cfg.policy.admission_enabled);
    assert!(cfg.policy.enforcement_enabled);
    assert_eq!(cfg.policy.max_vcpus, 4);
    assert_eq!(cfg.policy.max_memory_mib, 2048);
    assert_eq!(cfg.policy.max_sandboxes, 50);
    assert_eq!(cfg.policy.allowed_images.len(), 2);
    assert_eq!(cfg.policy.bundle_dir, "/tmp/nova-bundles");
    assert_eq!(cfg.policy.enforcement_rules, "strict");
}

#[test]
fn test_policy_config_defaults() {
    // Missing [policy] section should give defaults.
    let cfg = DaemonConfig::from_toml("").unwrap();
    assert!(cfg.policy.admission_enabled); // default true
    assert!(!cfg.policy.enforcement_enabled); // default false
    assert_eq!(cfg.policy.max_vcpus, 8);
    assert_eq!(cfg.policy.max_memory_mib, 8192);
    assert_eq!(cfg.policy.max_sandboxes, 100);
    assert!(cfg.policy.allowed_images.is_empty());
    assert_eq!(cfg.policy.bundle_dir, "/var/lib/nova/policy/bundles");
    assert_eq!(cfg.policy.enforcement_rules, "default");
}

// ── 2. PolicyState initialization ────────────────────────────────────

#[test]
fn test_policy_state_initialization() {
    let config = PolicyConfig {
        admission_enabled: true,
        enforcement_enabled: true,
        max_vcpus: 4,
        max_memory_mib: 1024,
        max_sandboxes: 10,
        allowed_images: vec!["docker.io/".to_string()],
        bundle_dir: std::env::temp_dir()
            .join("nova-test-policy-state-init")
            .to_string_lossy()
            .to_string(),
        enforcement_rules: "default".to_string(),
        rules: Vec::new(),
    };

    let state = PolicyState::from_config(&config);
    assert!(state.admission_enabled);
    assert!(state.enforcement_enabled);
    assert_eq!(state.enforcement_engine.rules().len(), 3); // default rules
    assert_eq!(state.bundle_mgr.bundle_count(), 0);

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

// ── 3. Admission checks ─────────────────────────────────────────────

#[test]
fn test_admission_denies_in_daemon_context() {
    let config = PolicyConfig {
        admission_enabled: true,
        enforcement_enabled: false,
        max_vcpus: 2,
        max_memory_mib: 512,
        max_sandboxes: 100,
        allowed_images: Vec::new(),
        bundle_dir: std::env::temp_dir()
            .join("nova-test-admission-deny")
            .to_string_lossy()
            .to_string(),
        enforcement_rules: "none".to_string(),
        rules: Vec::new(),
    };

    let state = PolicyState::from_config(&config);

    // Exceeds vCPU limit.
    let input = AdmissionInput {
        sandbox_id: "test-1".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 4,
        memory_mib: 256,
        uid: 1000,
    };
    let result = state.admission_checker.check(&input);
    assert!(!result.allowed);
    assert!(result.reason.contains("vCPUs"));

    // Exceeds memory limit.
    let input = AdmissionInput {
        sandbox_id: "test-2".to_string(),
        image: "nginx:latest".to_string(),
        vcpus: 1,
        memory_mib: 1024,
        uid: 1000,
    };
    let result = state.admission_checker.check(&input);
    assert!(!result.allowed);
    assert!(result.reason.contains("memory"));

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

#[test]
fn test_admission_image_allowlist_from_config() {
    let config = PolicyConfig {
        admission_enabled: true,
        enforcement_enabled: false,
        max_vcpus: 8,
        max_memory_mib: 8192,
        max_sandboxes: 100,
        allowed_images: vec!["docker.io/library/".to_string()],
        bundle_dir: std::env::temp_dir()
            .join("nova-test-admission-allowlist")
            .to_string_lossy()
            .to_string(),
        enforcement_rules: "none".to_string(),
        rules: Vec::new(),
    };

    let state = PolicyState::from_config(&config);

    // Allowed.
    let input = AdmissionInput {
        sandbox_id: "test-ok".to_string(),
        image: "docker.io/library/nginx:latest".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    assert!(state.admission_checker.check(&input).allowed);

    // Denied.
    let input = AdmissionInput {
        sandbox_id: "test-deny".to_string(),
        image: "evil.io/malware:latest".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    let result = state.admission_checker.check(&input);
    assert!(!result.allowed);
    assert!(result.reason.contains("allowlist"));

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

// ── 4. Enforcement rules ─────────────────────────────────────────────

#[test]
fn test_enforcement_default_rules_loaded() {
    let config = PolicyConfig {
        enforcement_rules: "default".to_string(),
        ..PolicyConfig::default()
    };
    // Override bundle_dir to temp.
    let config = PolicyConfig {
        bundle_dir: std::env::temp_dir()
            .join("nova-test-enf-default")
            .to_string_lossy()
            .to_string(),
        ..config
    };

    let state = PolicyState::from_config(&config);
    assert_eq!(state.enforcement_engine.rules().len(), 3);
    assert!(state
        .enforcement_engine
        .rules()
        .iter()
        .all(|r| r.enabled));

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

#[test]
fn test_enforcement_strict_rules_loaded() {
    let config = PolicyConfig {
        enforcement_rules: "strict".to_string(),
        bundle_dir: std::env::temp_dir()
            .join("nova-test-enf-strict")
            .to_string_lossy()
            .to_string(),
        ..PolicyConfig::default()
    };

    let state = PolicyState::from_config(&config);
    assert_eq!(state.enforcement_engine.rules().len(), 5);
    let enabled: Vec<_> = state
        .enforcement_engine
        .rules()
        .iter()
        .filter(|r| r.enabled)
        .collect();
    assert_eq!(enabled.len(), 3);

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

#[test]
fn test_enforcement_evaluates_events() {
    let config = PolicyConfig {
        enforcement_enabled: true,
        enforcement_rules: "default".to_string(),
        bundle_dir: std::env::temp_dir()
            .join("nova-test-enf-eval")
            .to_string_lossy()
            .to_string(),
        ..PolicyConfig::default()
    };

    let mut state = PolicyState::from_config(&config);
    // Default rules alert on process_exec.
    let action = state.enforcement_engine.evaluate("process_exec");
    assert_eq!(action, EnforcementAction::Alert);

    // file_open has no matching rule → Allow.
    let action = state.enforcement_engine.evaluate("file_open");
    assert_eq!(action, EnforcementAction::Allow);

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

// ── 5. Bundle management ─────────────────────────────────────────────

#[test]
fn test_bundle_load_and_evaluate() {
    let bundle_dir = std::env::temp_dir().join("nova-test-bundle-eval");
    let _ = std::fs::remove_dir_all(&bundle_dir);

    let mut engine = PolicyEngine::new().unwrap();
    let mut mgr = BundleManager::new(&bundle_dir).unwrap();

    // Minimal valid Wasm module.
    let wasm = b"\x00asm\x01\x00\x00\x00";
    mgr.load_bundle("test-bundle", wasm, &engine).unwrap();

    let policies = mgr.get_policies("test-bundle").unwrap();
    assert_eq!(policies.len(), 1);

    // Evaluate — no eval export → defaults to allow.
    let input = serde_json::json!({"image": "nginx"});
    let result = engine.evaluate(&policies[0], &input).unwrap();
    assert!(result.allowed);

    let _ = std::fs::remove_dir_all(&bundle_dir);
}

#[test]
fn test_bundle_lifecycle_via_manager() {
    let bundle_dir = std::env::temp_dir().join("nova-test-bundle-lifecycle");
    let _ = std::fs::remove_dir_all(&bundle_dir);

    let engine = PolicyEngine::new().unwrap();
    let mut mgr = BundleManager::new(&bundle_dir).unwrap();
    assert_eq!(mgr.bundle_count(), 0);

    let wasm = b"\x00asm\x01\x00\x00\x00";
    mgr.load_bundle("alpha", wasm, &engine).unwrap();
    assert_eq!(mgr.bundle_count(), 1);

    let info = mgr.get_info("alpha").unwrap();
    assert!(info.digest.starts_with("sha256:"));
    assert_eq!(info.policy_count, 1);

    let all = mgr.list_bundles();
    assert_eq!(all.len(), 1);

    mgr.remove_bundle("alpha").unwrap();
    assert_eq!(mgr.bundle_count(), 0);
    assert!(mgr.get_info("alpha").is_none());

    let _ = std::fs::remove_dir_all(&bundle_dir);
}

// ── 6. Enforcement deny blocks event ─────────────────────────────────

#[test]
fn test_enforcement_deny_blocks_event() {
    let mut engine = EnforcementEngine::new();
    engine.add_rule(nova_policy::EnforcementRule {
        name: "deny_exec".to_string(),
        event_type: "process_exec".to_string(),
        action: EnforcementAction::Deny,
        enabled: true,
    });

    let action = engine.evaluate("process_exec");
    assert_eq!(action, EnforcementAction::Deny);
    assert_eq!(engine.denial_count(), 1);

    // Non-matching event still allowed.
    let action = engine.evaluate("file_open");
    assert_eq!(action, EnforcementAction::Allow);
}

// ── 7. E2E: deny file_open blocks /etc/passwd reads ──────────────────

/// Simulates the full pipeline: sensor events flow through enforcement.
/// A Deny rule on `file_open` should block file-read events (e.g. /etc/passwd)
/// while process_exec events still flow through with Alert.
#[test]
fn test_e2e_deny_file_open_blocks_etc_passwd() {
    use crossbeam_channel::{bounded, TryRecvError};

    // 1. Build PolicyState with enforcement enabled + deny_file_open rule.
    let config = PolicyConfig {
        admission_enabled: true,
        enforcement_enabled: true,
        enforcement_rules: "none".to_string(), // start clean
        bundle_dir: std::env::temp_dir()
            .join("nova-test-e2e-deny-file")
            .to_string_lossy()
            .to_string(),
        ..PolicyConfig::default()
    };
    let mut state = PolicyState::from_config(&config);

    // Add custom rules: deny file_open, alert process_exec.
    state.enforcement_engine.add_rule(nova_policy::EnforcementRule {
        name: "deny_file_open".to_string(),
        event_type: "file_open".to_string(),
        action: EnforcementAction::Deny,
        enabled: true,
    });
    state.enforcement_engine.add_rule(nova_policy::EnforcementRule {
        name: "alert_process_exec".to_string(),
        event_type: "process_exec".to_string(),
        action: EnforcementAction::Alert,
        enabled: true,
    });

    // 2. Simulate the pipeline drain loop from server.rs.
    //    Produce synthetic events as the sensor pipeline would.
    let (event_tx, event_rx) = bounded::<(String, u32, String)>(64);

    // Simulate 3 events:
    //  - file_open (EventType 10) — e.g. reading /etc/passwd → should be DENIED
    //  - process_exec (EventType 1) — e.g. cat /etc/passwd → should be ALERTED
    //  - net_connect (EventType 20) — no rule → should be ALLOWED
    struct FakeEvent {
        event_type_u32: u32,
        comm: &'static str,
        description: &'static str,
    }
    let events = vec![
        FakeEvent { event_type_u32: 10, comm: "cat", description: "file_open /etc/passwd" },
        FakeEvent { event_type_u32: 1,  comm: "cat", description: "process_exec cat" },
        FakeEvent { event_type_u32: 20, comm: "curl", description: "net_connect 1.1.1.1:443" },
    ];

    let mut forwarded = Vec::new();
    let mut denied = Vec::new();
    let mut alerted = Vec::new();

    for ev in &events {
        let event_type_str = nova_api::policy_server::event_type_to_str(ev.event_type_u32);

        let action = if state.enforcement_enabled {
            state.enforcement_engine.evaluate(event_type_str)
        } else {
            EnforcementAction::Allow
        };

        match action {
            EnforcementAction::Deny => {
                denied.push(ev.description);
            }
            EnforcementAction::Alert => {
                alerted.push(ev.description);
                forwarded.push(ev.description);
            }
            EnforcementAction::Kill => {
                forwarded.push(ev.description);
            }
            EnforcementAction::Allow => {
                forwarded.push(ev.description);
            }
        }
    }

    // 3. Assert: file_open was DENIED (blocked), never forwarded.
    assert_eq!(denied.len(), 1, "expected exactly 1 denied event");
    assert_eq!(denied[0], "file_open /etc/passwd");
    assert!(
        !forwarded.contains(&"file_open /etc/passwd"),
        "file_open /etc/passwd should NOT be forwarded"
    );

    // 4. Assert: process_exec was ALERTED (forwarded + logged).
    assert_eq!(alerted.len(), 1);
    assert_eq!(alerted[0], "process_exec cat");
    assert!(forwarded.contains(&"process_exec cat"));

    // 5. Assert: net_connect was ALLOWED (forwarded silently).
    assert!(forwarded.contains(&"net_connect 1.1.1.1:443"));

    // 6. Assert: only 2 events forwarded (process_exec + net_connect), NOT file_open.
    assert_eq!(forwarded.len(), 2, "only 2 events should be forwarded, file_open was denied");

    // 7. Verify enforcement counters.
    assert_eq!(state.enforcement_engine.decision_count(), 3);
    assert_eq!(state.enforcement_engine.denial_count(), 1);
    assert_eq!(state.enforcement_engine.alert_count(), 1);

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

/// Test that admission control blocks a sandbox requesting too many resources,
/// simulating what happens in create_sandbox when someone tries to run a
/// container that exceeds policy limits.
#[test]
fn test_e2e_admission_blocks_oversized_sandbox() {
    let config = PolicyConfig {
        admission_enabled: true,
        max_vcpus: 2,
        max_memory_mib: 256,
        max_sandboxes: 5,
        allowed_images: vec!["docker.io/library/".to_string()],
        bundle_dir: std::env::temp_dir()
            .join("nova-test-e2e-admission-block")
            .to_string_lossy()
            .to_string(),
        ..PolicyConfig::default()
    };
    let state = PolicyState::from_config(&config);

    // This request should PASS: small nginx container.
    let ok_input = AdmissionInput {
        sandbox_id: "sandbox-1".to_string(),
        image: "docker.io/library/nginx:alpine".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    let result = state.admission_checker.check(&ok_input);
    assert!(result.allowed, "small nginx should be allowed");

    // This should FAIL: too many vCPUs.
    let bad_cpu = AdmissionInput {
        sandbox_id: "sandbox-2".to_string(),
        image: "docker.io/library/nginx:alpine".to_string(),
        vcpus: 8,
        memory_mib: 128,
        uid: 1000,
    };
    let result = state.admission_checker.check(&bad_cpu);
    assert!(!result.allowed, "8 vCPUs should be denied (limit=2)");
    assert!(result.reason.contains("vCPUs"), "reason: {}", result.reason);

    // This should FAIL: too much memory.
    let bad_mem = AdmissionInput {
        sandbox_id: "sandbox-3".to_string(),
        image: "docker.io/library/nginx:alpine".to_string(),
        vcpus: 1,
        memory_mib: 1024,
        uid: 1000,
    };
    let result = state.admission_checker.check(&bad_mem);
    assert!(!result.allowed, "1024MiB should be denied (limit=256)");
    assert!(result.reason.contains("memory"), "reason: {}", result.reason);

    // This should FAIL: image not in allowlist.
    let bad_image = AdmissionInput {
        sandbox_id: "sandbox-4".to_string(),
        image: "evil.io/cryptominer:latest".to_string(),
        vcpus: 1,
        memory_mib: 128,
        uid: 1000,
    };
    let result = state.admission_checker.check(&bad_image);
    assert!(!result.allowed, "evil.io image should be denied");
    assert!(result.reason.contains("allowlist"), "reason: {}", result.reason);

    let _ = std::fs::remove_dir_all(&config.bundle_dir);
}

// ── 8. Config roundtrip ──────────────────────────────────────────────

#[test]
fn test_policy_config_roundtrip() {
    let toml = r#"
[policy]
admission_enabled = true
enforcement_enabled = true
max_vcpus = 16
max_memory_mib = 16384
max_sandboxes = 200
allowed_images = ["gcr.io/"]
bundle_dir = "/tmp/nova-roundtrip"
enforcement_rules = "default"
"#;
    let cfg = DaemonConfig::from_toml(toml).unwrap();
    let state = PolicyState::from_config(&cfg.policy);

    assert!(state.admission_enabled);
    assert!(state.enforcement_enabled);
    assert_eq!(state.enforcement_engine.rules().len(), 3);
    assert_eq!(state.engine.eval_count(), 0);
    assert_eq!(state.engine.denied_count(), 0);
    assert_eq!(state.bundle_mgr.bundle_count(), 0);

    let _ = std::fs::remove_dir_all("/tmp/nova-roundtrip");
}
