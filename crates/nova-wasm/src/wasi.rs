//! WASI host implementation.
//!
//! Configures WASI contexts for Wasm module execution, including
//! filesystem access, environment variables, and stdio routing.

use std::collections::HashMap;

use wasmtime::{Engine, Linker, Store};
use wasmtime_wasi::pipe::MemoryOutputPipe;
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{Result, WasmError};

/// Configuration for a WASI execution context.
#[derive(Debug, Clone, Default)]
pub struct WasiConfig {
    /// Command line arguments.
    pub args: Vec<String>,
    /// Environment variables.
    pub env: HashMap<String, String>,
    /// Whether to inherit stdio from the host.
    pub inherit_stdio: bool,
}

/// A WASI execution context wrapping a Wasmtime store.
pub struct WasiContext {
    /// The Wasmtime store with WASI state.
    store: Store<WasiP1Ctx>,
    /// The linker with WASI imports.
    linker: Linker<WasiP1Ctx>,
}

impl WasiContext {
    /// Create a new WASI context with the given configuration.
    pub fn new(engine: &Engine, config: &WasiConfig) -> Result<Self> {
        let mut builder = WasiCtxBuilder::new();

        // Set arguments.
        if !config.args.is_empty() {
            builder.args(&config.args);
        }

        // Set environment variables.
        for (key, value) in &config.env {
            builder.env(key, value);
        }

        // Configure stdio.
        if config.inherit_stdio {
            builder.inherit_stdio();
        }

        let wasi_ctx = builder.build_p1();

        let store = Store::new(engine, wasi_ctx);
        let mut linker = Linker::new(engine);
        wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
            .map_err(|e| WasmError::Wasi(format!("failed to add WASI to linker: {e}")))?;

        tracing::debug!("created WASI context");

        Ok(Self { store, linker })
    }

    /// Get a mutable reference to the store.
    pub fn store_mut(&mut self) -> &mut Store<WasiP1Ctx> {
        &mut self.store
    }

    /// Get a reference to the linker.
    pub fn linker(&self) -> &Linker<WasiP1Ctx> {
        &self.linker
    }

    /// Instantiate and run a Wasm module's `_start` function.
    pub fn run(&mut self, module: &wasmtime::Module) -> Result<()> {
        let instance = self
            .linker
            .instantiate(&mut self.store, module)
            .map_err(|e| WasmError::Instantiation(format!("instantiation failed: {e}")))?;

        let start = instance
            .get_typed_func::<(), ()>(&mut self.store, "_start")
            .map_err(|e| WasmError::Execution(format!("no _start function: {e}")))?;

        start
            .call(&mut self.store, ())
            .map_err(|e| WasmError::Execution(format!("execution failed: {e}")))?;

        Ok(())
    }
}

/// A WASI context that captures stdout output for testing.
pub struct WasiContextWithCapture {
    store: Store<WasiP1Ctx>,
    linker: Linker<WasiP1Ctx>,
    stdout: MemoryOutputPipe,
}

impl WasiContextWithCapture {
    /// Create a new WASI context that captures stdout.
    pub fn new(engine: &Engine) -> Result<Self> {
        let stdout = MemoryOutputPipe::new(4096);

        let mut builder = WasiCtxBuilder::new();
        builder.stdout(stdout.clone());

        let wasi_ctx = builder.build_p1();
        let store = Store::new(engine, wasi_ctx);
        let mut linker = Linker::new(engine);
        wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
            .map_err(|e| WasmError::Wasi(format!("failed to add WASI to linker: {e}")))?;

        Ok(Self {
            store,
            linker,
            stdout,
        })
    }

    /// Run a Wasm module and return captured stdout as a String.
    ///
    /// Consumes self so the store is dropped before reading the pipe,
    /// which is required for `try_into_inner` to succeed (single Arc ref).
    pub fn run(mut self, module: &wasmtime::Module) -> Result<String> {
        let instance = self
            .linker
            .instantiate(&mut self.store, module)
            .map_err(|e| WasmError::Instantiation(format!("instantiation failed: {e}")))?;

        let start = instance
            .get_typed_func::<(), ()>(&mut self.store, "_start")
            .map_err(|e| WasmError::Execution(format!("no _start function: {e}")))?;

        start
            .call(&mut self.store, ())
            .map_err(|e| WasmError::Execution(format!("execution failed: {e}")))?;

        // Drop the store and linker so stdout pipe has a single Arc reference.
        let stdout = self.stdout;
        drop(self.linker);
        drop(self.store);

        let bytes: bytes::Bytes = stdout.try_into_inner().unwrap_or_default().into();
        String::from_utf8(bytes.to_vec())
            .map_err(|e| WasmError::Execution(format!("stdout not valid UTF-8: {e}")))
    }
}
