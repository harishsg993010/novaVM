//! Built-in policies for common security patterns.
//!
//! These policies are always available without loading external bundles.

use crate::enforcement::{EnforcementAction, EnforcementRule};

/// Create a set of default security enforcement rules.
pub fn default_rules() -> Vec<EnforcementRule> {
    vec![
        // Alert on any process execution (for audit logging).
        EnforcementRule {
            name: "audit_process_exec".to_string(),
            event_type: "process_exec".to_string(),
            action: EnforcementAction::Alert,
            enabled: true,
        },
        // Alert on network connections (for audit logging).
        EnforcementRule {
            name: "audit_net_connect".to_string(),
            event_type: "net_connect".to_string(),
            action: EnforcementAction::Alert,
            enabled: true,
        },
        // Alert on file unlink operations.
        EnforcementRule {
            name: "audit_file_unlink".to_string(),
            event_type: "file_unlink".to_string(),
            action: EnforcementAction::Alert,
            enabled: true,
        },
    ]
}

/// Create strict security rules that deny certain operations.
pub fn strict_rules() -> Vec<EnforcementRule> {
    let mut rules = default_rules();
    rules.extend(vec![
        // Deny DNS queries to prevent data exfiltration.
        EnforcementRule {
            name: "deny_dns".to_string(),
            event_type: "dns_query".to_string(),
            action: EnforcementAction::Deny,
            enabled: false, // Disabled by default.
        },
        // Kill sandbox on suspicious HTTP activity.
        EnforcementRule {
            name: "kill_suspicious_http".to_string(),
            event_type: "http_request".to_string(),
            action: EnforcementAction::Kill,
            enabled: false, // Disabled by default.
        },
    ]);
    rules
}
