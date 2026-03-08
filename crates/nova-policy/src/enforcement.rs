//! Runtime policy enforcement.
//!
//! Continuously evaluates policies against runtime events and enforces
//! decisions (allow, deny, alert).

use serde::{Deserialize, Serialize};

/// Action to take when a policy is violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnforcementAction {
    /// Allow the operation.
    Allow,
    /// Deny the operation.
    Deny,
    /// Allow but generate an alert.
    Alert,
    /// Kill the sandbox.
    Kill,
}

impl std::fmt::Display for EnforcementAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allow => write!(f, "allow"),
            Self::Deny => write!(f, "deny"),
            Self::Alert => write!(f, "alert"),
            Self::Kill => write!(f, "kill"),
        }
    }
}

/// A runtime enforcement rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnforcementRule {
    /// Rule name.
    pub name: String,
    /// Event type to match (e.g., "process_exec", "net_connect").
    pub event_type: String,
    /// Action to take when the rule matches.
    pub action: EnforcementAction,
    /// Whether the rule is enabled.
    pub enabled: bool,
}

/// Runtime enforcement engine.
pub struct EnforcementEngine {
    /// Active enforcement rules.
    rules: Vec<EnforcementRule>,
    /// Number of enforcement decisions made.
    decisions: u64,
    /// Number of denied actions.
    denials: u64,
    /// Number of alerts generated.
    alerts: u64,
}

impl EnforcementEngine {
    /// Create a new enforcement engine.
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            decisions: 0,
            denials: 0,
            alerts: 0,
        }
    }

    /// Add an enforcement rule.
    pub fn add_rule(&mut self, rule: EnforcementRule) {
        tracing::info!(name = %rule.name, action = %rule.action, "added enforcement rule");
        self.rules.push(rule);
    }

    /// Evaluate an event against all active rules.
    ///
    /// Returns the most restrictive matching action (Kill > Deny > Alert > Allow).
    pub fn evaluate(&mut self, event_type: &str) -> EnforcementAction {
        let mut result = EnforcementAction::Allow;

        for rule in &self.rules {
            if !rule.enabled {
                continue;
            }
            if rule.event_type == event_type || rule.event_type == "*" {
                // Take the most restrictive action.
                result = most_restrictive(result, rule.action);
            }
        }

        self.decisions += 1;
        match result {
            EnforcementAction::Deny | EnforcementAction::Kill => self.denials += 1,
            EnforcementAction::Alert => self.alerts += 1,
            EnforcementAction::Allow => {}
        }

        result
    }

    /// Returns the number of enforcement decisions.
    pub fn decision_count(&self) -> u64 {
        self.decisions
    }

    /// Returns the number of denials.
    pub fn denial_count(&self) -> u64 {
        self.denials
    }

    /// Returns the number of alerts.
    pub fn alert_count(&self) -> u64 {
        self.alerts
    }

    /// Returns all rules.
    pub fn rules(&self) -> &[EnforcementRule] {
        &self.rules
    }
}

impl Default for EnforcementEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the more restrictive of two actions.
fn most_restrictive(a: EnforcementAction, b: EnforcementAction) -> EnforcementAction {
    fn severity(action: EnforcementAction) -> u8 {
        match action {
            EnforcementAction::Allow => 0,
            EnforcementAction::Alert => 1,
            EnforcementAction::Deny => 2,
            EnforcementAction::Kill => 3,
        }
    }
    if severity(b) > severity(a) {
        b
    } else {
        a
    }
}
