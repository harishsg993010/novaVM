//! Sandbox orchestrator — manages the lifecycle of all sandboxes.

use std::collections::HashMap;

use crate::error::{Result, RuntimeError};
use crate::pool::{PoolConfig, VmPool};
use crate::sandbox::lifecycle::{Sandbox, SandboxConfig, SandboxKind, SandboxState};
use crate::snapshot_cache::SnapshotCache;

/// Manages the collection of all sandboxes.
#[allow(dead_code)]
pub struct SandboxOrchestrator {
    /// Map from sandbox ID to sandbox instance.
    sandboxes: HashMap<String, Sandbox>,
    /// Optional L4 pre-warmed VM pool.
    pool: Option<VmPool>,
    /// Optional L3 snapshot cache.
    snapshot_cache: Option<SnapshotCache>,
}

impl SandboxOrchestrator {
    /// Create a new orchestrator.
    pub fn new() -> Self {
        tracing::debug!("creating sandbox orchestrator");
        Self {
            sandboxes: HashMap::new(),
            pool: None,
            snapshot_cache: None,
        }
    }

    /// Create an orchestrator with a pre-warmed VM pool (L4).
    pub fn with_pool(pool_config: PoolConfig) -> Self {
        let pool = VmPool::new(pool_config);
        Self {
            sandboxes: HashMap::new(),
            pool: Some(pool),
            snapshot_cache: None,
        }
    }

    /// Create an orchestrator with both a snapshot cache (L3) and pool (L4).
    pub fn with_caches(
        pool_config: Option<PoolConfig>,
        snapshot_cache: Option<SnapshotCache>,
    ) -> Self {
        Self {
            sandboxes: HashMap::new(),
            pool: pool_config.map(VmPool::new),
            snapshot_cache,
        }
    }

    /// Get a reference to the pool (if configured).
    pub fn pool(&self) -> Option<&VmPool> {
        self.pool.as_ref()
    }

    /// Create a new sandbox.
    pub fn create(&mut self, id: String, config: SandboxConfig) -> Result<()> {
        if self.sandboxes.contains_key(&id) {
            return Err(RuntimeError::SandboxExists(id));
        }

        let sandbox = Sandbox::new(id.clone(), config);
        self.sandboxes.insert(id, sandbox);
        Ok(())
    }

    /// Start a sandbox.
    pub fn start(&mut self, id: &str) -> Result<()> {
        let sandbox = self
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| RuntimeError::SandboxNotFound(id.to_string()))?;

        match sandbox.config().kind.clone() {
            SandboxKind::Vm => sandbox.start(),
            SandboxKind::Wasm {
                module_path,
                entry_function,
            } => {
                if sandbox.state() != SandboxState::Created {
                    return Err(RuntimeError::InvalidState {
                        id: id.to_string(),
                        from: sandbox.state().to_string(),
                        to: "running".to_string(),
                    });
                }

                sandbox.set_state(SandboxState::Running);

                let result = Self::run_wasm_sandbox(&module_path, &entry_function);
                match result {
                    Ok((output, values)) => {
                        let sandbox = self.sandboxes.get_mut(id).unwrap();
                        if let Some(out) = output {
                            sandbox.set_wasm_output(out);
                        }
                        if !values.is_empty() {
                            sandbox.set_wasm_result(values);
                        }
                        sandbox.set_state(SandboxState::Stopped);
                        Ok(())
                    }
                    Err(e) => {
                        let sandbox = self.sandboxes.get_mut(id).unwrap();
                        sandbox.set_error();
                        Err(e)
                    }
                }
            }
        }
    }

    /// Run a Wasm sandbox synchronously and return (stdout_output, return_values).
    fn run_wasm_sandbox(
        module_path: &std::path::Path,
        entry_function: &str,
    ) -> Result<(Option<String>, Vec<i64>)> {
        let wasm_bytes = std::fs::read(module_path).map_err(|e| {
            RuntimeError::Wasm(format!(
                "failed to read module '{}': {}",
                module_path.display(),
                e
            ))
        })?;

        let config = nova_wasm::WasmEngineConfig::default();
        let engine = nova_wasm::create_engine(&config)
            .map_err(|e| RuntimeError::Wasm(format!("engine creation failed: {e}")))?;

        let module = wasmtime::Module::new(&engine, &wasm_bytes)
            .map_err(|e| RuntimeError::Wasm(format!("module compilation failed: {e}")))?;

        if entry_function == "_start" {
            // Use WasiContextWithCapture to run and capture stdout.
            let ctx = nova_wasm::WasiContextWithCapture::new(&engine)
                .map_err(|e| RuntimeError::Wasm(format!("WASI context creation failed: {e}")))?;

            let output = ctx
                .run(&module)
                .map_err(|e| RuntimeError::Wasm(format!("execution failed: {e}")))?;

            Ok((Some(output), vec![]))
        } else {
            // Instantiate and call the named export directly.
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = wasmtime::Instance::new(&mut store, &module, &[])
                .map_err(|e| RuntimeError::Wasm(format!("instantiation failed: {e}")))?;

            let func = instance
                .get_func(&mut store, entry_function)
                .ok_or_else(|| {
                    RuntimeError::Wasm(format!("export '{}' not found", entry_function))
                })?;

            let ty = func.ty(&store);
            let param_count = ty.params().len();
            let result_count = ty.results().len();

            // Build default params (zeros).
            let params: Vec<wasmtime::Val> = ty
                .params()
                .map(|p| match p {
                    wasmtime::ValType::I32 => wasmtime::Val::I32(0),
                    wasmtime::ValType::I64 => wasmtime::Val::I64(0),
                    _ => wasmtime::Val::I32(0),
                })
                .collect();

            let mut results = vec![wasmtime::Val::I32(0); result_count];

            func.call(&mut store, &params, &mut results)
                .map_err(|e| RuntimeError::Wasm(format!("call to '{}' failed: {e}", entry_function)))?;

            let values: Vec<i64> = results
                .iter()
                .map(|v| match v {
                    wasmtime::Val::I32(n) => *n as i64,
                    wasmtime::Val::I64(n) => *n,
                    _ => 0,
                })
                .collect();

            tracing::info!(
                entry = entry_function,
                params = param_count,
                results = ?values,
                "wasm function call completed"
            );

            Ok((None, values))
        }
    }

    /// Stop a sandbox.
    pub fn stop(&mut self, id: &str) -> Result<()> {
        let sandbox = self
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| RuntimeError::SandboxNotFound(id.to_string()))?;
        sandbox.stop()
    }

    /// Destroy a sandbox (remove it entirely).
    pub fn destroy(&mut self, id: &str) -> Result<()> {
        let sandbox = self
            .sandboxes
            .get(id)
            .ok_or_else(|| RuntimeError::SandboxNotFound(id.to_string()))?;

        // Cannot destroy a running sandbox — stop it first.
        if sandbox.state() == SandboxState::Running {
            return Err(RuntimeError::InvalidState {
                id: id.to_string(),
                from: "running".to_string(),
                to: "destroyed".to_string(),
            });
        }

        self.sandboxes.remove(id);
        tracing::info!(sandbox_id = id, "destroyed sandbox");
        Ok(())
    }

    /// Get a reference to a sandbox.
    pub fn get(&self, id: &str) -> Result<&Sandbox> {
        self.sandboxes
            .get(id)
            .ok_or_else(|| RuntimeError::SandboxNotFound(id.to_string()))
    }

    /// Get a mutable reference to a sandbox.
    pub fn get_mut(&mut self, id: &str) -> Result<&mut Sandbox> {
        self.sandboxes
            .get_mut(id)
            .ok_or_else(|| RuntimeError::SandboxNotFound(id.to_string()))
    }

    /// List all sandboxes.
    pub fn list(&self) -> Vec<&Sandbox> {
        self.sandboxes.values().collect()
    }

    /// Returns the number of sandboxes.
    pub fn count(&self) -> usize {
        self.sandboxes.len()
    }
}

impl Default for SandboxOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SandboxOrchestrator {
    fn drop(&mut self) {
        if let Some(ref mut pool) = self.pool {
            pool.shutdown();
        }
    }
}
