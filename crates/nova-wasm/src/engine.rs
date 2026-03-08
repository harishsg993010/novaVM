//! Wasmtime engine configuration and management.
//!
//! Provides a configured Wasmtime [`Engine`] with appropriate settings
//! for serverless Wasm execution (cranelift compilation, caching, etc.).

use wasmtime::{Config, Engine, OptLevel};

use crate::error::{Result, WasmError};

/// Configuration for the Wasm engine.
#[derive(Debug, Clone)]
pub struct WasmEngineConfig {
    /// Enable Cranelift optimizations.
    pub optimize: bool,
    /// Enable Wasm SIMD support.
    pub simd: bool,
    /// Enable Wasm multi-memory.
    pub multi_memory: bool,
    /// Enable Wasm component model.
    pub component_model: bool,
    /// Maximum Wasm memory pages (64KiB each).
    pub max_memory_pages: u64,
    /// Fuel limit per execution (0 = unlimited).
    pub fuel_limit: u64,
}

impl Default for WasmEngineConfig {
    fn default() -> Self {
        Self {
            optimize: true,
            simd: true,
            multi_memory: true,
            component_model: true,
            max_memory_pages: 65536, // 4 GiB max
            fuel_limit: 0,
        }
    }
}

/// Creates a configured Wasmtime engine.
pub fn create_engine(config: &WasmEngineConfig) -> Result<Engine> {
    let mut wasm_config = Config::new();

    if config.optimize {
        wasm_config.cranelift_opt_level(OptLevel::Speed);
    } else {
        wasm_config.cranelift_opt_level(OptLevel::None);
    }

    wasm_config.wasm_simd(config.simd);
    wasm_config.wasm_multi_memory(config.multi_memory);
    wasm_config.wasm_component_model(config.component_model);

    if config.fuel_limit > 0 {
        wasm_config.consume_fuel(true);
    }

    let engine = Engine::new(&wasm_config)
        .map_err(|e| WasmError::Compilation(format!("failed to create engine: {e}")))?;

    tracing::info!(
        optimize = config.optimize,
        simd = config.simd,
        component_model = config.component_model,
        "created Wasm engine"
    );

    Ok(engine)
}
