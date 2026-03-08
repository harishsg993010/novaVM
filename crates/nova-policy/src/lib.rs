//! NovaVM OPA policy engine.
//!
//! Provides policy evaluation via compiled OPA Wasm bundles, admission
//! control for sandbox creation, and runtime enforcement of security rules.

pub mod admission;
pub mod builtin;
pub mod bundle;
pub mod enforcement;
pub mod engine;
pub mod error;

pub use admission::{AdmissionChecker, AdmissionInput, AdmissionResult};
pub use bundle::{BundleInfo, BundleManager};
pub use enforcement::{EnforcementAction, EnforcementEngine, EnforcementRule};
pub use engine::{CompiledPolicy, EvalResult, PolicyEngine};
pub use error::{PolicyError, Result};

#[cfg(test)]
mod tests {
    use super::*;

    // -- Admission checker tests -------------------------------------------

    #[test]
    fn test_admission_allows_valid_request() {
        let checker = AdmissionChecker::new();
        let input = AdmissionInput {
            sandbox_id: "test-1".to_string(),
            image: "docker.io/library/nginx:latest".to_string(),
            vcpus: 2,
            memory_mib: 256,
            uid: 1000,
        };

        let result = checker.check(&input);
        assert!(result.allowed);
        assert!(result.reason.is_empty());
    }

    #[test]
    fn test_admission_denies_excessive_vcpus() {
        let mut checker = AdmissionChecker::new();
        checker.set_max_vcpus(4);

        let input = AdmissionInput {
            sandbox_id: "test-2".to_string(),
            image: "nginx:latest".to_string(),
            vcpus: 8,
            memory_mib: 256,
            uid: 1000,
        };

        let result = checker.check(&input);
        assert!(!result.allowed);
        assert!(result.reason.contains("vCPUs"));
    }

    #[test]
    fn test_admission_denies_excessive_memory() {
        let mut checker = AdmissionChecker::new();
        checker.set_max_memory_mib(1024);

        let input = AdmissionInput {
            sandbox_id: "test-3".to_string(),
            image: "nginx:latest".to_string(),
            vcpus: 2,
            memory_mib: 2048,
            uid: 1000,
        };

        let result = checker.check(&input);
        assert!(!result.allowed);
        assert!(result.reason.contains("memory"));
    }

    #[test]
    fn test_admission_denies_at_sandbox_limit() {
        let mut checker = AdmissionChecker::new();
        checker.set_max_sandboxes(5);
        checker.set_current_sandboxes(5);

        let input = AdmissionInput {
            sandbox_id: "test-4".to_string(),
            image: "nginx:latest".to_string(),
            vcpus: 1,
            memory_mib: 128,
            uid: 1000,
        };

        let result = checker.check(&input);
        assert!(!result.allowed);
        assert!(result.reason.contains("limit"));
    }

    #[test]
    fn test_admission_image_allowlist() {
        let mut checker = AdmissionChecker::new();
        checker.add_allowed_image("docker.io/library/");
        checker.add_allowed_image("ghcr.io/novavm/");

        // Allowed image.
        let input = AdmissionInput {
            sandbox_id: "test-5a".to_string(),
            image: "docker.io/library/nginx:latest".to_string(),
            vcpus: 1,
            memory_mib: 128,
            uid: 1000,
        };
        assert!(checker.check(&input).allowed);

        // Denied image.
        let input = AdmissionInput {
            sandbox_id: "test-5b".to_string(),
            image: "evil.io/malware:latest".to_string(),
            vcpus: 1,
            memory_mib: 128,
            uid: 1000,
        };
        let result = checker.check(&input);
        assert!(!result.allowed);
        assert!(result.reason.contains("allowlist"));
    }

    // -- Enforcement engine tests ------------------------------------------

    #[test]
    fn test_enforcement_default_allow() {
        let mut engine = EnforcementEngine::new();
        let action = engine.evaluate("process_exec");
        assert_eq!(action, EnforcementAction::Allow);
        assert_eq!(engine.decision_count(), 1);
    }

    #[test]
    fn test_enforcement_alert_rule() {
        let mut engine = EnforcementEngine::new();
        engine.add_rule(EnforcementRule {
            name: "audit_exec".to_string(),
            event_type: "process_exec".to_string(),
            action: EnforcementAction::Alert,
            enabled: true,
        });

        let action = engine.evaluate("process_exec");
        assert_eq!(action, EnforcementAction::Alert);
        assert_eq!(engine.alert_count(), 1);

        // Non-matching event should allow.
        let action = engine.evaluate("file_open");
        assert_eq!(action, EnforcementAction::Allow);
    }

    #[test]
    fn test_enforcement_most_restrictive() {
        let mut engine = EnforcementEngine::new();
        engine.add_rule(EnforcementRule {
            name: "alert_all".to_string(),
            event_type: "*".to_string(),
            action: EnforcementAction::Alert,
            enabled: true,
        });
        engine.add_rule(EnforcementRule {
            name: "deny_exec".to_string(),
            event_type: "process_exec".to_string(),
            action: EnforcementAction::Deny,
            enabled: true,
        });

        // Should get Deny (most restrictive of Alert + Deny).
        let action = engine.evaluate("process_exec");
        assert_eq!(action, EnforcementAction::Deny);
        assert_eq!(engine.denial_count(), 1);
    }

    #[test]
    fn test_enforcement_disabled_rule() {
        let mut engine = EnforcementEngine::new();
        engine.add_rule(EnforcementRule {
            name: "deny_all".to_string(),
            event_type: "*".to_string(),
            action: EnforcementAction::Kill,
            enabled: false,
        });

        let action = engine.evaluate("process_exec");
        assert_eq!(action, EnforcementAction::Allow);
    }

    // -- Built-in rules tests ----------------------------------------------

    #[test]
    fn test_builtin_default_rules() {
        let rules = builtin::default_rules();
        assert_eq!(rules.len(), 3);
        assert!(rules.iter().all(|r| r.enabled));
        assert!(rules.iter().all(|r| r.action == EnforcementAction::Alert));
    }

    #[test]
    fn test_builtin_strict_rules() {
        let rules = builtin::strict_rules();
        assert!(rules.len() > 3); // Has more than default.
                                  // Strict extras are disabled by default.
        let extras: Vec<_> = rules.iter().filter(|r| !r.enabled).collect();
        assert_eq!(extras.len(), 2);
    }

    // -- Policy engine tests (with minimal Wasm) ---------------------------

    #[test]
    fn test_policy_engine_creation() {
        let engine = PolicyEngine::new().unwrap();
        assert_eq!(engine.eval_count(), 0);
        assert_eq!(engine.denied_count(), 0);
    }

    #[test]
    fn test_policy_engine_compile_invalid() {
        let engine = PolicyEngine::new().unwrap();
        let result = engine.compile("bad_policy", b"not-valid-wasm");
        assert!(result.is_err());
    }

    #[test]
    fn test_policy_engine_compile_and_eval_minimal() {
        // A minimal valid Wasm module (empty module).
        // (module) in WAT = \x00asm\x01\x00\x00\x00
        let wasm = b"\x00asm\x01\x00\x00\x00";

        let mut engine = PolicyEngine::new().unwrap();
        let policy = engine.compile("empty_policy", wasm).unwrap();

        let input = serde_json::json!({"action": "create"});
        let result = engine.evaluate(&policy, &input).unwrap();

        // Empty module has no "eval" export, defaults to allow.
        assert!(result.allowed);
        assert_eq!(engine.eval_count(), 1);
    }

    // -- Bundle manager tests ----------------------------------------------

    #[test]
    fn test_bundle_manager_lifecycle() {
        let dir = std::env::temp_dir().join("nova-policy-test-bundles");
        let _ = std::fs::remove_dir_all(&dir);

        let engine = PolicyEngine::new().unwrap();
        let mut mgr = BundleManager::new(&dir).unwrap();
        assert_eq!(mgr.bundle_count(), 0);

        // Load a bundle with a minimal Wasm module.
        let wasm = b"\x00asm\x01\x00\x00\x00";
        mgr.load_bundle("test-bundle", wasm, &engine).unwrap();
        assert_eq!(mgr.bundle_count(), 1);

        // Get info.
        let info = mgr.get_info("test-bundle").unwrap();
        assert!(info.digest.starts_with("sha256:"));
        assert_eq!(info.policy_count, 1);

        // List.
        assert_eq!(mgr.list_bundles().len(), 1);

        // Remove.
        mgr.remove_bundle("test-bundle").unwrap();
        assert_eq!(mgr.bundle_count(), 0);

        // Remove non-existent.
        assert!(mgr.remove_bundle("nope").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_bundle_empty_wasm_error() {
        let dir = std::env::temp_dir().join("nova-policy-test-empty");
        let _ = std::fs::remove_dir_all(&dir);

        let engine = PolicyEngine::new().unwrap();
        let mut mgr = BundleManager::new(&dir).unwrap();

        let err = mgr.load_bundle("empty", b"", &engine);
        assert!(err.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
