//! Wasm executor integration tests.
//!
//! Tests the Wasmtime engine, module caching, WASI context, and
//! instance pool working together.

use std::collections::HashMap;

use nova_wasm::{
    create_engine, InstancePool, ModuleCache, WasiConfig, WasiContext, WasmEngineConfig,
};

/// Test engine creation with default config.
#[test]
fn test_engine_default_config() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config);
    assert!(engine.is_ok(), "engine creation should succeed");
}

/// Test engine creation with custom config.
#[test]
fn test_engine_custom_config() {
    let config = WasmEngineConfig {
        optimize: false,
        simd: true,
        multi_memory: true,
        component_model: true,
        max_memory_pages: 1024,
        fuel_limit: 1_000_000,
    };
    let engine = create_engine(&config);
    assert!(engine.is_ok());
}

/// Test module cache deduplication by SHA-256 digest.
#[test]
fn test_module_cache_dedup() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();
    let mut cache = ModuleCache::new(engine);

    // Minimal valid Wasm module.
    let wasm_bytes = wat_to_wasm_minimal();

    // Compile the same bytes twice — should deduplicate.
    let m1 = cache.compile("module-a", &wasm_bytes);
    assert!(m1.is_ok());
    let digest1 = m1.unwrap().digest().to_string();

    let m2 = cache.compile("module-a", &wasm_bytes);
    assert!(m2.is_ok());
    let digest2 = m2.unwrap().digest().to_string();

    assert_eq!(digest1, digest2, "same bytes should produce same digest");

    // Cache should contain exactly 1 entry (deduplicated).
    assert_eq!(cache.len(), 1);
}

/// Test module cache with different modules.
#[test]
fn test_module_cache_different_modules() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();
    let mut cache = ModuleCache::new(engine);

    // Two different minimal modules.
    let wasm1 = wat_to_wasm_minimal();
    let wasm2 = wat_to_wasm_with_func();

    let d1 = cache.compile("mod-1", &wasm1).unwrap().digest().to_string();
    let d2 = cache.compile("mod-2", &wasm2).unwrap().digest().to_string();

    assert_ne!(d1, d2, "different modules should have different digests");
    assert_eq!(cache.len(), 2);
}

/// Test module cache eviction and clearing.
#[test]
fn test_module_cache_evict_and_clear() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();
    let mut cache = ModuleCache::new(engine);

    let wasm = wat_to_wasm_minimal();
    let digest = cache
        .compile("evict-me", &wasm)
        .unwrap()
        .digest()
        .to_string();
    assert_eq!(cache.len(), 1);

    // Evict by digest.
    assert!(cache.evict(&digest));
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());

    // Re-add and clear.
    cache.compile("re-add", &wasm).unwrap();
    assert_eq!(cache.len(), 1);
    cache.clear();
    assert!(cache.is_empty());
}

/// Test WASI context creation.
#[test]
fn test_wasi_context_creation() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();

    let wasi_config = WasiConfig {
        args: vec!["test-app".to_string()],
        env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
        inherit_stdio: false,
    };

    let ctx = WasiContext::new(&engine, &wasi_config);
    assert!(ctx.is_ok(), "WASI context creation should succeed");
}

/// Test instance pool warm/take/return cycle.
#[test]
fn test_instance_pool_lifecycle() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();
    let pool = InstancePool::new(engine, 5);

    let wasm_bytes = wat_to_wasm_minimal();

    // Warm the pool.
    pool.warm("worker", &wasm_bytes).unwrap();
    pool.warm("worker", &wasm_bytes).unwrap();
    pool.warm("worker", &wasm_bytes).unwrap();
    assert_eq!(pool.size(), 3);

    // Take a module from the pool.
    let taken = pool.take("worker");
    assert!(taken.is_some(), "should be able to take from pool");
    assert_eq!(pool.size(), 2);

    // Return it.
    pool.return_module("worker", taken.unwrap());
    assert_eq!(pool.size(), 3);
}

/// Test instance pool exhaustion.
#[test]
fn test_instance_pool_exhaustion() {
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();
    let pool = InstancePool::new(engine, 2);

    let wasm_bytes = wat_to_wasm_minimal();
    pool.warm("mod", &wasm_bytes).unwrap();
    pool.warm("mod", &wasm_bytes).unwrap();

    // Take all.
    let _m1 = pool.take("mod").unwrap();
    let _m2 = pool.take("mod").unwrap();

    // Pool should be empty.
    assert_eq!(pool.size(), 0);
    assert!(pool.take("mod").is_none(), "empty pool should return None");
}

/// Test engine + cache + pool together as a full pipeline.
#[test]
fn test_full_wasm_pipeline() {
    // 1. Create engine.
    let config = WasmEngineConfig::default();
    let engine = create_engine(&config).unwrap();

    // 2. Cache a module.
    let engine_clone = create_engine(&config).unwrap();
    let mut cache = ModuleCache::new(engine_clone);
    let wasm_bytes = wat_to_wasm_with_func();
    let compiled = cache.compile("pipeline-mod", &wasm_bytes).unwrap();
    assert!(!compiled.digest().is_empty());

    // 3. Warm a pool.
    let pool = InstancePool::new(engine, 10);
    for _ in 0..5 {
        pool.warm("pipeline-mod", &wasm_bytes).unwrap();
    }
    assert_eq!(pool.size(), 5);

    // 4. Take from pool, use, return.
    let instance = pool.take("pipeline-mod").unwrap();
    assert_eq!(pool.size(), 4);
    pool.return_module("pipeline-mod", instance);
    assert_eq!(pool.size(), 5);
}

/// Minimal valid Wasm binary (empty module).
fn wat_to_wasm_minimal() -> Vec<u8> {
    // Wasm magic: \0asm, version 1, no sections.
    vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]
}

/// Wasm binary with a simple function (different from minimal).
fn wat_to_wasm_with_func() -> Vec<u8> {
    // Type section: () -> (), Function section: 1 func, Code section: 1 body (end).
    vec![
        0x00, 0x61, 0x73, 0x6D, // magic
        0x01, 0x00, 0x00, 0x00, // version
        // Type section (id=1)
        0x01, 0x04, // section id=1, size=4
        0x01, // 1 type
        0x60, 0x00, 0x00, // func type: () -> ()
        // Function section (id=3)
        0x03, 0x02, // section id=3, size=2
        0x01, // 1 function
        0x00, // type index 0
        // Code section (id=10)
        0x0A, 0x04, // section id=10, size=4
        0x01, // 1 function body
        0x02, // body size=2
        0x00, // local count=0
        0x0B, // end
    ]
}
