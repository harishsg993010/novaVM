//! Wasm module instantiation and caching.
//!
//! Compiles Wasm modules into [`CompiledModule`]s that can be quickly
//! instantiated multiple times. Provides a digest-based cache to avoid
//! recompilation of identical modules.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use wasmtime::{Engine, Module};

use crate::error::{Result, WasmError};

/// A compiled Wasm module ready for instantiation.
pub struct CompiledModule {
    /// The compiled Wasmtime module.
    module: Module,
    /// Module name/identifier.
    name: String,
    /// SHA-256 digest of the original bytecode.
    digest: String,
    /// Size of the original bytecode.
    bytecode_size: usize,
}

impl CompiledModule {
    /// Get the underlying Wasmtime module.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Get the module name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the bytecode digest.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Get the bytecode size.
    pub fn bytecode_size(&self) -> usize {
        self.bytecode_size
    }
}

/// Cache for compiled Wasm modules.
pub struct ModuleCache {
    /// Engine used for compilation.
    engine: Engine,
    /// Cache: digest -> CompiledModule.
    cache: HashMap<String, CompiledModule>,
}

impl ModuleCache {
    /// Create a new module cache.
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            cache: HashMap::new(),
        }
    }

    /// Compile a Wasm module from bytecode, using cache if available.
    ///
    /// If a module with the same digest is already cached, returns a
    /// reference to the cached module without recompilation.
    pub fn compile(&mut self, name: &str, wasm_bytes: &[u8]) -> Result<&CompiledModule> {
        let digest = compute_digest(wasm_bytes);

        if self.cache.contains_key(&digest) {
            tracing::debug!(name, digest = &digest[..16], "cache hit");
            return Ok(self.cache.get(&digest).unwrap());
        }

        tracing::info!(name, size = wasm_bytes.len(), "compiling module");

        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| WasmError::Compilation(format!("failed to compile '{}': {}", name, e)))?;

        let compiled = CompiledModule {
            module,
            name: name.to_string(),
            digest: digest.clone(),
            bytecode_size: wasm_bytes.len(),
        };

        self.cache.insert(digest.clone(), compiled);
        Ok(self.cache.get(&digest).unwrap())
    }

    /// Remove a module from the cache by digest.
    pub fn evict(&mut self, digest: &str) -> bool {
        self.cache.remove(digest).is_some()
    }

    /// Clear the entire cache.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Returns the number of cached modules.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Returns a reference to the Wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// Compute the SHA-256 digest of bytecode.
fn compute_digest(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}
