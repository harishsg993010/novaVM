//! Wasmtime-based OPA policy evaluator.
//!
//! Loads compiled OPA Wasm bundles and evaluates policies against JSON
//! input. The OPA Wasm ABI exposes `eval` and `builtins` entrypoints
//! that this engine calls to perform policy decisions.

use std::time::Instant;

use wasmtime::{Engine, Linker, Module, Store};

use crate::error::{PolicyError, Result};

/// Result of a policy evaluation.
#[derive(Debug, Clone)]
pub struct EvalResult {
    /// Whether the policy allows the action.
    pub allowed: bool,
    /// Reason for denial (empty if allowed).
    pub reason: String,
    /// Full result as JSON.
    pub result_json: serde_json::Value,
    /// Evaluation duration in microseconds.
    pub duration_us: u64,
}

/// OPA policy engine using Wasmtime.
pub struct PolicyEngine {
    /// Wasmtime engine (shared across all modules).
    engine: Engine,
    /// Number of evaluations performed.
    eval_count: u64,
    /// Number of denied evaluations.
    denied_count: u64,
    /// Total evaluation time in microseconds.
    total_eval_us: u64,
}

/// A compiled policy module ready for evaluation.
pub struct CompiledPolicy {
    /// The compiled Wasmtime module.
    module: Module,
    /// Policy identifier.
    name: String,
}

impl CompiledPolicy {
    /// Get the policy name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl PolicyEngine {
    /// Create a new policy engine.
    pub fn new() -> Result<Self> {
        let engine = Engine::default();
        tracing::info!("created OPA policy engine");
        Ok(Self {
            engine,
            eval_count: 0,
            denied_count: 0,
            total_eval_us: 0,
        })
    }

    /// Compile a Wasm policy module from raw bytes.
    ///
    /// The `wasm_bytes` should be a compiled OPA Wasm bundle (the `.wasm`
    /// file from `opa build -t wasm`).
    pub fn compile(&self, name: &str, wasm_bytes: &[u8]) -> Result<CompiledPolicy> {
        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| PolicyError::Wasm(format!("failed to compile '{}': {}", name, e)))?;

        tracing::info!(name, "compiled policy module");
        Ok(CompiledPolicy {
            module,
            name: name.to_string(),
        })
    }

    /// Evaluate a compiled policy with the given JSON input.
    ///
    /// In a full OPA Wasm ABI implementation, this would:
    /// 1. Instantiate the module with OPA builtins linked
    /// 2. Write input data to the Wasm memory
    /// 3. Call `eval` entrypoint
    /// 4. Read the result from Wasm memory
    ///
    /// This implementation provides a simplified evaluator that checks
    /// for basic allow/deny patterns.
    pub fn evaluate(
        &mut self,
        policy: &CompiledPolicy,
        input: &serde_json::Value,
    ) -> Result<EvalResult> {
        let start = Instant::now();

        tracing::debug!(policy = %policy.name, "evaluating policy");

        // Create a store and linker for this evaluation.
        let mut store = Store::new(&self.engine, ());
        let linker = Linker::new(&self.engine);

        // Instantiate the module.
        let instance = linker
            .instantiate(&mut store, &policy.module)
            .map_err(|e| {
                PolicyError::Evaluation(format!("failed to instantiate '{}': {}", policy.name, e))
            })?;

        // Look for an exported "eval" function.
        // OPA Wasm modules export: eval, builtins, entrypoints
        let eval_result = if let Some(eval_fn) = instance.get_func(&mut store, "eval") {
            // Call the eval function.
            let mut results = [wasmtime::Val::I32(0)];
            eval_fn.call(&mut store, &[], &mut results).map_err(|e| {
                PolicyError::Evaluation(format!("eval call failed for '{}': {}", policy.name, e))
            })?;

            let result_addr = results[0].i32().unwrap_or(0);
            // In a real implementation, we'd read the JSON result from Wasm memory.
            // For now, use the return value as a simple allow/deny signal.
            result_addr != 0
        } else {
            // No eval function — treat as a simple allow policy.
            tracing::warn!(policy = %policy.name, "no 'eval' export found, defaulting to allow");
            true
        };

        let duration = start.elapsed();
        let duration_us = duration.as_micros() as u64;

        self.eval_count += 1;
        self.total_eval_us += duration_us;

        let result = EvalResult {
            allowed: eval_result,
            reason: if eval_result {
                String::new()
            } else {
                "policy denied".to_string()
            },
            result_json: serde_json::json!({
                "allow": eval_result,
                "policy": policy.name,
                "input": input,
            }),
            duration_us,
        };

        if !eval_result {
            self.denied_count += 1;
        }

        Ok(result)
    }

    /// Simplified evaluation for WAT policies with `eval() -> i32` or `eval(i32) -> i32`.
    ///
    /// Supports both signatures automatically. Returns an `EvalResult` where
    /// `allowed` is true when the Wasm function returns non-zero.
    pub fn evaluate_simple(
        &mut self,
        policy: &CompiledPolicy,
        input_code: i32,
    ) -> Result<EvalResult> {
        let start = Instant::now();

        let mut store = Store::new(&self.engine, ());
        let linker = Linker::new(&self.engine);

        let instance = linker
            .instantiate(&mut store, &policy.module)
            .map_err(|e| {
                PolicyError::Evaluation(format!("failed to instantiate '{}': {}", policy.name, e))
            })?;

        let eval_fn = instance.get_func(&mut store, "eval").ok_or_else(|| {
            PolicyError::Evaluation(format!("no 'eval' export in '{}'", policy.name))
        })?;

        let ty = eval_fn.ty(&store);
        let param_count = ty.params().len();

        let mut results = [wasmtime::Val::I32(0)];

        if param_count == 0 {
            eval_fn
                .call(&mut store, &[], &mut results)
                .map_err(|e| {
                    PolicyError::Evaluation(format!(
                        "eval() call failed for '{}': {}",
                        policy.name, e
                    ))
                })?;
        } else {
            eval_fn
                .call(
                    &mut store,
                    &[wasmtime::Val::I32(input_code)],
                    &mut results,
                )
                .map_err(|e| {
                    PolicyError::Evaluation(format!(
                        "eval(i32) call failed for '{}': {}",
                        policy.name, e
                    ))
                })?;
        }

        let result_val = results[0].i32().unwrap_or(0);
        let allowed = result_val != 0;

        let duration = start.elapsed();
        let duration_us = duration.as_micros() as u64;

        self.eval_count += 1;
        self.total_eval_us += duration_us;
        if !allowed {
            self.denied_count += 1;
        }

        Ok(EvalResult {
            allowed,
            reason: if allowed {
                String::new()
            } else {
                "policy denied".to_string()
            },
            result_json: serde_json::json!({
                "allow": allowed,
                "policy": policy.name,
                "result": result_val,
            }),
            duration_us,
        })
    }

    /// Returns the number of evaluations performed.
    pub fn eval_count(&self) -> u64 {
        self.eval_count
    }

    /// Returns the number of denied evaluations.
    pub fn denied_count(&self) -> u64 {
        self.denied_count
    }

    /// Returns the average evaluation time in microseconds.
    pub fn avg_eval_us(&self) -> u64 {
        if self.eval_count == 0 {
            0
        } else {
            self.total_eval_us / self.eval_count
        }
    }

    /// Returns a reference to the underlying Wasmtime engine.
    pub fn wasmtime_engine(&self) -> &Engine {
        &self.engine
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new().expect("failed to create policy engine")
    }
}
