//! PolicyService gRPC implementation.
//!
//! Provides the server-side implementation of the PolicyService defined
//! in `policy.proto`. Manages OPA policy evaluation, bundle lifecycle,
//! admission control, and runtime enforcement.

use std::path::PathBuf;
use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::config::PolicyConfig;
use crate::policy::policy_service_server::PolicyService;
use crate::policy::*;

use nova_policy::{
    AdmissionChecker, BundleManager, EnforcementEngine, PolicyEngine,
};

/// Internal state for the policy subsystem.
pub struct PolicyState {
    /// OPA Wasm policy engine.
    pub engine: PolicyEngine,
    /// Policy bundle manager.
    pub bundle_mgr: BundleManager,
    /// Sandbox admission checker.
    pub admission_checker: AdmissionChecker,
    /// Runtime enforcement engine.
    pub enforcement_engine: EnforcementEngine,
    /// Whether admission control is enabled.
    pub admission_enabled: bool,
    /// Whether runtime enforcement is enabled.
    pub enforcement_enabled: bool,
}

impl PolicyState {
    /// Initialize from a `PolicyConfig`.
    pub fn from_config(config: &PolicyConfig) -> Self {
        let engine = PolicyEngine::new().expect("failed to create policy engine");

        let bundle_dir = PathBuf::from(&config.bundle_dir);
        let bundle_mgr =
            BundleManager::new(&bundle_dir).expect("failed to create bundle manager");

        let mut admission_checker = AdmissionChecker::new();
        admission_checker.set_max_vcpus(config.max_vcpus);
        admission_checker.set_max_memory_mib(config.max_memory_mib);
        admission_checker.set_max_sandboxes(config.max_sandboxes);
        for prefix in &config.allowed_images {
            admission_checker.add_allowed_image(prefix);
        }

        let mut enforcement_engine = EnforcementEngine::new();
        match config.enforcement_rules.as_str() {
            "default" => {
                for rule in nova_policy::builtin::default_rules() {
                    enforcement_engine.add_rule(rule);
                }
            }
            "strict" => {
                for rule in nova_policy::builtin::strict_rules() {
                    enforcement_engine.add_rule(rule);
                }
            }
            "none" => {}
            other => {
                tracing::warn!(ruleset = %other, "unknown enforcement ruleset, using none");
            }
        }

        // Append custom rules from TOML config.
        for rule in &config.rules {
            tracing::info!(
                name = %rule.name,
                event_type = %rule.event_type,
                action = %rule.action,
                enabled = rule.enabled,
                "loading custom enforcement rule from config"
            );
            enforcement_engine.add_rule(rule.clone());
        }

        tracing::info!(
            admission = config.admission_enabled,
            enforcement = config.enforcement_enabled,
            rules = enforcement_engine.rules().len(),
            bundles = bundle_mgr.bundle_count(),
            "policy state initialized"
        );

        Self {
            engine,
            bundle_mgr,
            admission_checker,
            enforcement_engine,
            admission_enabled: config.admission_enabled,
            enforcement_enabled: config.enforcement_enabled,
        }
    }
}

/// gRPC PolicyService implementation.
pub struct PolicyDaemonService {
    pub state: Arc<std::sync::Mutex<PolicyState>>,
}

impl PolicyDaemonService {
    pub fn new(state: Arc<std::sync::Mutex<PolicyState>>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl PolicyService for PolicyDaemonService {
    async fn evaluate(
        &self,
        request: Request<EvaluateRequest>,
    ) -> Result<Response<EvaluateResponse>, Status> {
        let req = request.into_inner();

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("lock poisoned: {}", e)))?;

        let bundle_id = req.policy_path.clone();

        // Split borrow: access bundle_mgr and engine as separate fields.
        let PolicyState {
            ref bundle_mgr,
            ref mut engine,
            ..
        } = *state;

        if let Some(policies) = bundle_mgr.get_policies(&bundle_id) {
            if let Some(policy) = policies.first() {
                let input: serde_json::Value =
                    serde_json::from_slice(&req.input).unwrap_or(serde_json::Value::Null);

                match engine.evaluate(policy, &input) {
                    Ok(result) => {
                        let result_bytes = serde_json::to_vec(&result.result_json)
                            .unwrap_or_default();
                        return Ok(Response::new(EvaluateResponse {
                            allowed: result.allowed,
                            reason: result.reason,
                            result: result_bytes,
                            eval_duration_us: result.duration_us,
                        }));
                    }
                    Err(e) => {
                        return Ok(Response::new(EvaluateResponse {
                            allowed: false,
                            reason: format!("evaluation failed: {}", e),
                            result: Vec::new(),
                            eval_duration_us: 0,
                        }));
                    }
                }
            }
        }

        // No bundle found — default allow.
        Ok(Response::new(EvaluateResponse {
            allowed: true,
            reason: format!("no bundle '{}' loaded, default allow", bundle_id),
            result: Vec::new(),
            eval_duration_us: 0,
        }))
    }

    async fn load_bundle(
        &self,
        request: Request<LoadBundleRequest>,
    ) -> Result<Response<LoadBundleResponse>, Status> {
        let req = request.into_inner();

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("lock poisoned: {}", e)))?;

        // Split borrow: bundle_mgr needs &mut, engine needs &.
        let PolicyState {
            ref mut bundle_mgr,
            ref engine,
            ..
        } = *state;

        match bundle_mgr.load_bundle(&req.bundle_id, &req.bundle_data, engine) {
            Ok(()) => Ok(Response::new(LoadBundleResponse {
                success: true,
                error_message: String::new(),
            })),
            Err(e) => Ok(Response::new(LoadBundleResponse {
                success: false,
                error_message: e.to_string(),
            })),
        }
    }

    async fn list_bundles(
        &self,
        _request: Request<ListBundlesRequest>,
    ) -> Result<Response<ListBundlesResponse>, Status> {
        let state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("lock poisoned: {}", e)))?;

        let bundles = state
            .bundle_mgr
            .list_bundles()
            .into_iter()
            .map(|info| BundleInfo {
                bundle_id: info.bundle_id.clone(),
                policy_count: info.policy_count,
                loaded_at: format_epoch_secs(info.loaded_at),
                digest: info.digest.clone(),
            })
            .collect();

        Ok(Response::new(ListBundlesResponse { bundles }))
    }

    async fn remove_bundle(
        &self,
        request: Request<RemoveBundleRequest>,
    ) -> Result<Response<RemoveBundleResponse>, Status> {
        let req = request.into_inner();

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("lock poisoned: {}", e)))?;

        state
            .bundle_mgr
            .remove_bundle(&req.bundle_id)
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(RemoveBundleResponse {}))
    }

    async fn get_status(
        &self,
        _request: Request<GetPolicyStatusRequest>,
    ) -> Result<Response<PolicyStatus>, Status> {
        let state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("lock poisoned: {}", e)))?;

        Ok(Response::new(PolicyStatus {
            loaded_bundles: state.bundle_mgr.bundle_count() as u32,
            total_evaluations: state.engine.eval_count(),
            denied_evaluations: state.engine.denied_count(),
            avg_eval_duration_us: state.engine.avg_eval_us(),
        }))
    }
}

/// Map EventType u32 to the string names used by EnforcementRule.
pub fn event_type_to_str(event_type: u32) -> &'static str {
    match event_type {
        1 => "process_exec",
        2 => "process_exit",
        3 => "process_fork",
        10 => "file_open",
        11 => "file_write",
        12 => "file_unlink",
        20 => "net_connect",
        21 => "net_accept",
        22 => "net_close",
        30 => "http_request",
        31 => "http_response",
        40 => "dns_query",
        _ => "unknown",
    }
}

/// Format epoch seconds as RFC 3339 (reuses the same approach as server.rs).
fn format_epoch_secs(secs: u64) -> String {
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let mut y: u64 = 1970;
    let mut remaining = days;
    loop {
        let days_in_year = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut m = 0u64;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            m = i as u64 + 1;
            break;
        }
        remaining -= md;
    }

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, remaining + 1, hours, minutes, seconds
    )
}
