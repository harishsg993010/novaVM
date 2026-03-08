//! NovaVM Wasm executor.
//!
//! Provides a serverless Wasm execution environment built on Wasmtime
//! with WASI support, module caching, and a pre-warmed instance pool.

pub mod engine;
pub mod error;
pub mod instance;
pub mod pool;
pub mod wasi;

pub use engine::{create_engine, WasmEngineConfig};
pub use error::{Result, WasmError};
pub use instance::{CompiledModule, ModuleCache};
pub use pool::InstancePool;
pub use wasi::{WasiConfig, WasiContext, WasiContextWithCapture};

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid Wasm module: (module)
    const MINIMAL_WASM: &[u8] = b"\x00asm\x01\x00\x00\x00";

    // -- Engine tests ------------------------------------------------------

    #[test]
    fn test_create_engine_default() {
        let config = WasmEngineConfig::default();
        let engine = create_engine(&config).unwrap();
        // Just verify it doesn't panic.
        let _ = engine;
    }

    #[test]
    fn test_create_engine_no_optimize() {
        let config = WasmEngineConfig {
            optimize: false,
            ..Default::default()
        };
        let engine = create_engine(&config).unwrap();
        let _ = engine;
    }

    // -- Module cache tests ------------------------------------------------

    #[test]
    fn test_module_cache_compile() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let mut cache = ModuleCache::new(engine);
        assert!(cache.is_empty());

        let compiled = cache.compile("test-module", MINIMAL_WASM).unwrap();
        assert_eq!(compiled.name(), "test-module");
        assert!(compiled.digest().starts_with("sha256:"));
        assert_eq!(compiled.bytecode_size(), MINIMAL_WASM.len());

        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_module_cache_dedup() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let mut cache = ModuleCache::new(engine);

        // Compile same bytes twice — should deduplicate.
        cache.compile("module-a", MINIMAL_WASM).unwrap();
        cache.compile("module-b", MINIMAL_WASM).unwrap();

        // Same bytes = same digest = one cache entry.
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_module_cache_evict() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let mut cache = ModuleCache::new(engine);

        let compiled = cache.compile("evictable", MINIMAL_WASM).unwrap();
        let digest = compiled.digest().to_string();

        assert_eq!(cache.len(), 1);
        assert!(cache.evict(&digest));
        assert_eq!(cache.len(), 0);
        assert!(!cache.evict(&digest)); // Already evicted.
    }

    #[test]
    fn test_module_cache_clear() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let mut cache = ModuleCache::new(engine);

        cache.compile("m1", MINIMAL_WASM).unwrap();
        assert_eq!(cache.len(), 1);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_module_compile_invalid() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let mut cache = ModuleCache::new(engine);

        let err = cache.compile("bad", b"not-wasm");
        assert!(err.is_err());
    }

    // -- Instance pool tests -----------------------------------------------

    #[test]
    fn test_pool_warm_and_take() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let pool = InstancePool::new(engine, 10);
        assert_eq!(pool.size(), 0);

        pool.warm("handler", MINIMAL_WASM).unwrap();
        assert_eq!(pool.size(), 1);

        let module = pool.take("handler");
        assert!(module.is_some());
        assert_eq!(pool.size(), 0);

        // Take again — should be empty.
        assert!(pool.take("handler").is_none());
    }

    #[test]
    fn test_pool_return_module() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let pool = InstancePool::new(engine, 10);

        pool.warm("handler", MINIMAL_WASM).unwrap();
        let module = pool.take("handler").unwrap();

        pool.return_module("handler", module);
        assert_eq!(pool.size(), 1);
    }

    #[test]
    fn test_pool_full() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let pool = InstancePool::new(engine, 2);

        pool.warm("a", MINIMAL_WASM).unwrap();
        pool.warm("b", MINIMAL_WASM).unwrap();

        // Pool is full.
        let err = pool.warm("c", MINIMAL_WASM);
        assert!(err.is_err());
    }

    // -- WASI context tests ------------------------------------------------

    #[test]
    fn test_wasi_context_creation() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let config = WasiConfig {
            args: vec!["test-program".to_string()],
            env: [("FOO".to_string(), "bar".to_string())]
                .into_iter()
                .collect(),
            inherit_stdio: false,
        };

        let ctx = WasiContext::new(&engine, &config);
        assert!(ctx.is_ok());
    }

    #[test]
    fn test_wasi_context_default_config() {
        let engine = create_engine(&WasmEngineConfig::default()).unwrap();
        let config = WasiConfig::default();
        let ctx = WasiContext::new(&engine, &config);
        assert!(ctx.is_ok());
    }
}
