//! Sandbox admission policies.
//!
//! Evaluates whether a sandbox creation request should be allowed based
//! on configured policies (resource limits, image allowlists, etc.).

use serde::{Deserialize, Serialize};

use crate::engine::{CompiledPolicy, PolicyEngine};

/// Input for sandbox admission evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionInput {
    /// Sandbox ID being created.
    pub sandbox_id: String,
    /// OCI image reference.
    pub image: String,
    /// Requested vCPUs.
    pub vcpus: u32,
    /// Requested memory in MiB.
    pub memory_mib: u32,
    /// Requesting user ID.
    pub uid: u32,
}

/// Result of an admission check.
#[derive(Debug, Clone)]
pub struct AdmissionResult {
    /// Whether the sandbox creation is allowed.
    pub allowed: bool,
    /// Reason for denial (empty if allowed).
    pub reason: String,
}

/// Built-in admission policy checker.
///
/// Provides configurable resource limits and image allowlists without
/// requiring a full OPA Wasm evaluation.
pub struct AdmissionChecker {
    /// Maximum vCPUs per sandbox.
    max_vcpus: u32,
    /// Maximum memory per sandbox (MiB).
    max_memory_mib: u32,
    /// Maximum total sandboxes.
    max_sandboxes: u32,
    /// Allowed image prefixes (empty = allow all).
    allowed_images: Vec<String>,
    /// Current sandbox count.
    current_sandboxes: u32,
}

impl AdmissionChecker {
    /// Create a new admission checker with default limits.
    pub fn new() -> Self {
        Self {
            max_vcpus: 8,
            max_memory_mib: 8192,
            max_sandboxes: 100,
            allowed_images: Vec::new(),
            current_sandboxes: 0,
        }
    }

    /// Set the maximum vCPUs per sandbox.
    pub fn set_max_vcpus(&mut self, max: u32) {
        self.max_vcpus = max;
    }

    /// Set the maximum memory per sandbox (MiB).
    pub fn set_max_memory_mib(&mut self, max: u32) {
        self.max_memory_mib = max;
    }

    /// Set the maximum total sandboxes.
    pub fn set_max_sandboxes(&mut self, max: u32) {
        self.max_sandboxes = max;
    }

    /// Add an allowed image prefix.
    pub fn add_allowed_image(&mut self, prefix: &str) {
        self.allowed_images.push(prefix.to_string());
    }

    /// Update the current sandbox count.
    pub fn set_current_sandboxes(&mut self, count: u32) {
        self.current_sandboxes = count;
    }

    /// Check whether a sandbox creation request should be admitted.
    pub fn check(&self, input: &AdmissionInput) -> AdmissionResult {
        // Check vCPU limit.
        if input.vcpus > self.max_vcpus {
            return AdmissionResult {
                allowed: false,
                reason: format!(
                    "requested {} vCPUs exceeds limit of {}",
                    input.vcpus, self.max_vcpus
                ),
            };
        }

        // Check memory limit.
        if input.memory_mib > self.max_memory_mib {
            return AdmissionResult {
                allowed: false,
                reason: format!(
                    "requested {} MiB memory exceeds limit of {}",
                    input.memory_mib, self.max_memory_mib
                ),
            };
        }

        // Check sandbox count.
        if self.current_sandboxes >= self.max_sandboxes {
            return AdmissionResult {
                allowed: false,
                reason: format!(
                    "sandbox limit reached ({}/{})",
                    self.current_sandboxes, self.max_sandboxes
                ),
            };
        }

        // Check image allowlist.
        if !self.allowed_images.is_empty()
            && !self
                .allowed_images
                .iter()
                .any(|prefix| input.image.starts_with(prefix))
        {
            return AdmissionResult {
                allowed: false,
                reason: format!("image '{}' not in allowlist", input.image),
            };
        }

        AdmissionResult {
            allowed: true,
            reason: String::new(),
        }
    }

    /// Check admission with built-in rules first, then optionally a Wasm policy.
    ///
    /// If built-in checks deny, the Wasm policy is never called.
    pub fn check_with_policy(
        &self,
        input: &AdmissionInput,
        engine: &mut PolicyEngine,
        policy: Option<&CompiledPolicy>,
    ) -> AdmissionResult {
        // Built-in checks first.
        let builtin_result = self.check(input);
        if !builtin_result.allowed {
            return builtin_result;
        }

        // Optional Wasm policy check.
        if let Some(policy) = policy {
            match engine.evaluate_simple(policy, 1) {
                Ok(eval) => {
                    if !eval.allowed {
                        return AdmissionResult {
                            allowed: false,
                            reason: format!("wasm policy '{}' denied", policy.name()),
                        };
                    }
                }
                Err(e) => {
                    return AdmissionResult {
                        allowed: false,
                        reason: format!("wasm policy evaluation failed: {}", e),
                    };
                }
            }
        }

        AdmissionResult {
            allowed: true,
            reason: String::new(),
        }
    }
}

impl Default for AdmissionChecker {
    fn default() -> Self {
        Self::new()
    }
}
