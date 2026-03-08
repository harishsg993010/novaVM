//! Pre-warmed instance pool for fast cold starts.
//!
//! Maintains a pool of pre-compiled modules ready for instantiation.
//! This reduces latency for serverless workloads by avoiding
//! compilation on the request path.

use std::collections::VecDeque;

use parking_lot::Mutex;
use wasmtime::{Engine, Module};

use crate::error::{Result, WasmError};

/// A pool entry containing a pre-compiled module.
struct PoolEntry {
    /// The compiled module.
    module: Module,
    /// Module name.
    name: String,
}

/// Pre-warmed module pool for fast instantiation.
pub struct InstancePool {
    /// Engine shared by all modules.
    engine: Engine,
    /// Pool of pre-compiled modules.
    pool: Mutex<VecDeque<PoolEntry>>,
    /// Maximum pool size.
    max_size: usize,
}

impl InstancePool {
    /// Create a new instance pool.
    pub fn new(engine: Engine, max_size: usize) -> Self {
        tracing::info!(max_size, "created instance pool");
        Self {
            engine,
            pool: Mutex::new(VecDeque::with_capacity(max_size)),
            max_size,
        }
    }

    /// Pre-warm the pool by compiling a module.
    pub fn warm(&self, name: &str, wasm_bytes: &[u8]) -> Result<()> {
        let mut pool = self.pool.lock();
        if pool.len() >= self.max_size {
            return Err(WasmError::Cache("pool is full".to_string()));
        }

        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| WasmError::Compilation(format!("failed to compile '{}': {}", name, e)))?;

        pool.push_back(PoolEntry {
            module,
            name: name.to_string(),
        });

        tracing::debug!(name, pool_size = pool.len(), "warmed pool entry");
        Ok(())
    }

    /// Take a module from the pool by name.
    ///
    /// Returns the compiled module if available. The module is removed
    /// from the pool (it should be re-added when no longer needed).
    pub fn take(&self, name: &str) -> Option<Module> {
        let mut pool = self.pool.lock();
        if let Some(pos) = pool.iter().position(|e| e.name == name) {
            let entry = pool.remove(pos).unwrap();
            tracing::debug!(name, pool_size = pool.len(), "took module from pool");
            Some(entry.module)
        } else {
            None
        }
    }

    /// Return a module to the pool.
    pub fn return_module(&self, name: &str, module: Module) {
        let mut pool = self.pool.lock();
        if pool.len() < self.max_size {
            pool.push_back(PoolEntry {
                module,
                name: name.to_string(),
            });
            tracing::debug!(name, pool_size = pool.len(), "returned module to pool");
        }
    }

    /// Returns the current pool size.
    pub fn size(&self) -> usize {
        self.pool.lock().len()
    }

    /// Returns the maximum pool size.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Returns a reference to the engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}
