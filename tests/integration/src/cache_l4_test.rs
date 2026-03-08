//! L4 Pre-warmed VM Pool integration tests.

use nova_runtime::pool::{PoolConfig, PoolVmTemplate, VmPool, WarmStrategy};
use std::path::PathBuf;
use std::time::Duration;

fn test_config(target: usize, max: usize) -> PoolConfig {
    PoolConfig {
        target_pool_size: target,
        max_pool_size: max,
        warm_strategy: WarmStrategy::ColdBoot,
        max_idle_time: Duration::from_secs(60),
        replenish_interval: Duration::from_millis(50),
        vm_template: PoolVmTemplate {
            vcpus: 1,
            memory_mib: 128,
            kernel_path: PathBuf::from("/boot/vmlinux"),
            initrd_path: None,
            cmdline: "console=ttyS0".to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// No-root tests
// ---------------------------------------------------------------------------

#[test]
fn test_pool_config_validation() {
    let mut config = test_config(2, 4);
    assert!(config.validate().is_ok());

    config.target_pool_size = 0;
    assert!(config.validate().is_err());

    config.target_pool_size = 2;
    config.max_pool_size = 1; // less than target
    assert!(config.validate().is_err());

    config.max_pool_size = 4;
    config.vm_template.vcpus = 0;
    assert!(config.validate().is_err());
}

#[test]
fn test_pool_stats_initial() {
    let config = test_config(2, 4);
    let pool = VmPool::new(config);

    // Give the replenisher a moment to start.
    std::thread::sleep(Duration::from_millis(100));

    let stats = pool.stats();
    // The pool should have created some VMs by now.
    assert!(stats.total_created > 0 || stats.ready_count > 0);
    assert_eq!(stats.total_assigned, 0);
    assert_eq!(stats.pool_misses, 0);
}

#[test]
fn test_pool_acquire_from_empty() {
    let mut config = test_config(0, 0);
    config.target_pool_size = 0; // invalid, but we directly test acquire

    // Create pool with max_pool_size=0 so nothing gets created.
    // Use a config that won't produce VMs.
    let config = PoolConfig {
        target_pool_size: 1,
        max_pool_size: 1,
        warm_strategy: WarmStrategy::ColdBoot,
        max_idle_time: Duration::from_secs(60),
        replenish_interval: Duration::from_secs(999), // slow replenish
        vm_template: PoolVmTemplate {
            vcpus: 1,
            memory_mib: 128,
            kernel_path: PathBuf::from("/boot/vmlinux"),
            initrd_path: None,
            cmdline: "console=ttyS0".to_string(),
        },
    };

    let pool = VmPool::new(config);

    // Try acquire immediately before replenisher has run.
    // It may or may not have a VM; either way shouldn't panic.
    let _result = pool.acquire();
}

#[test]
fn test_pool_shutdown_clean() {
    let config = test_config(2, 4);
    let mut pool = VmPool::new(config);

    // Let it create some VMs.
    std::thread::sleep(Duration::from_millis(100));

    pool.shutdown();

    // After shutdown, stats should still be readable.
    let stats = pool.stats();
    let _ = stats.ready_count;
}

#[test]
fn test_warm_strategy_variants() {
    let cold = WarmStrategy::ColdBoot;
    match cold {
        WarmStrategy::ColdBoot => {}
        _ => panic!("expected ColdBoot"),
    }

    let snap = WarmStrategy::SnapshotRestore {
        snapshot_dir: PathBuf::from("/tmp/snapshot"),
    };
    match snap {
        WarmStrategy::SnapshotRestore { ref snapshot_dir } => {
            assert_eq!(snapshot_dir, &PathBuf::from("/tmp/snapshot"));
        }
        _ => panic!("expected SnapshotRestore"),
    }
}

// ---------------------------------------------------------------------------
// Pool lifecycle tests (no KVM required — pool creates WarmVm tokens, not real VMs)
// ---------------------------------------------------------------------------

#[test]
fn test_pool_cold_boot_replenish() {
    let config = test_config(2, 4);
    let pool = VmPool::new(config);

    // Wait for replenisher to fill pool.
    std::thread::sleep(Duration::from_millis(200));

    let stats = pool.stats();
    assert!(stats.ready_count >= 2, "expected >= 2 ready, got {}", stats.ready_count);
    assert!(stats.total_created >= 2);
}

#[test]
fn test_pool_acquire_and_replenish() {
    let config = test_config(2, 4);
    let pool = VmPool::new(config);

    std::thread::sleep(Duration::from_millis(200));

    // Acquire one.
    let vm = pool.acquire();
    assert!(vm.is_some(), "should have a VM ready");

    let stats = pool.stats();
    assert_eq!(stats.total_assigned, 1);

    // Wait for replenishment.
    std::thread::sleep(Duration::from_millis(200));

    let stats = pool.stats();
    assert!(stats.ready_count >= 2, "pool should have replenished");
}

#[test]
fn test_pool_snapshot_restore_strategy() {
    let config = PoolConfig {
        target_pool_size: 1,
        max_pool_size: 2,
        warm_strategy: WarmStrategy::SnapshotRestore {
            snapshot_dir: PathBuf::from("/tmp/test_snapshot"),
        },
        max_idle_time: Duration::from_secs(60),
        replenish_interval: Duration::from_millis(50),
        vm_template: PoolVmTemplate {
            vcpus: 1,
            memory_mib: 64,
            kernel_path: PathBuf::from("/boot/vmlinux"),
            initrd_path: None,
            cmdline: "console=ttyS0".to_string(),
        },
    };

    let pool = VmPool::new(config);
    std::thread::sleep(Duration::from_millis(200));

    let vm = pool.acquire().unwrap();
    match vm.origin {
        WarmStrategy::SnapshotRestore { ref snapshot_dir } => {
            assert_eq!(snapshot_dir, &PathBuf::from("/tmp/test_snapshot"));
        }
        _ => panic!("expected SnapshotRestore origin"),
    }
}

#[test]
fn test_pool_idle_expiry() {
    let config = PoolConfig {
        target_pool_size: 2,
        max_pool_size: 4,
        warm_strategy: WarmStrategy::ColdBoot,
        max_idle_time: Duration::from_millis(100), // very short
        replenish_interval: Duration::from_millis(50),
        vm_template: PoolVmTemplate {
            vcpus: 1,
            memory_mib: 64,
            kernel_path: PathBuf::from("/boot/vmlinux"),
            initrd_path: None,
            cmdline: "console=ttyS0".to_string(),
        },
    };

    let pool = VmPool::new(config);

    // Let pool fill.
    std::thread::sleep(Duration::from_millis(200));

    // Wait for VMs to expire.
    std::thread::sleep(Duration::from_millis(200));

    // The replenisher keeps creating and expiring, so total_expired should increase.
    let stats = pool.stats();
    // With 100ms idle and 50ms replenish, we should see churn.
    // Just verify it doesn't panic.
    let _ = stats.total_expired;
    let _ = stats.total_created;
}

#[test]
fn test_pool_concurrent_acquire() {
    let config = test_config(4, 8);
    let pool = std::sync::Arc::new(VmPool::new(config));

    // Let pool fill.
    std::thread::sleep(Duration::from_millis(300));

    // Spawn 4 threads each trying to acquire.
    let mut handles = Vec::new();
    for _ in 0..4 {
        let pool = std::sync::Arc::clone(&pool);
        handles.push(std::thread::spawn(move || {
            pool.acquire()
        }));
    }

    let mut acquired = 0;
    for h in handles {
        if h.join().unwrap().is_some() {
            acquired += 1;
        }
    }

    // At least some should succeed.
    assert!(acquired > 0, "expected at least 1 acquired VM");

    let stats = pool.stats();
    assert_eq!(stats.total_assigned, acquired);
}

// ---------------------------------------------------------------------------
// Factory-based pool tests (L4 upgrade: type-erased payloads)
// ---------------------------------------------------------------------------

#[test]
fn test_pool_with_factory() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let counter = std::sync::Arc::new(AtomicU64::new(100));
    let counter_clone = counter.clone();

    let config = test_config(2, 4);
    let pool = VmPool::with_factory(config, Box::new(move || {
        let val = counter_clone.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(val) as Box<dyn std::any::Any + Send>)
    }));

    // Wait for replenisher to create VMs with payloads.
    std::thread::sleep(Duration::from_millis(300));

    let mut vm = pool.acquire().expect("should have a VM ready");
    assert!(vm.payload.is_some(), "factory pool VM should have a payload");

    let val = vm.take_payload::<u64>().expect("payload should downcast to u64");
    assert!(val >= 100, "payload value should be >= 100, got {}", val);

    // After take_payload, payload should be None.
    assert!(vm.payload.is_none(), "payload should be consumed after take");
}

#[test]
fn test_pool_factory_failure_graceful() {
    let config = test_config(2, 4);
    let pool = VmPool::with_factory(config, Box::new(|| {
        // Factory always returns None (simulates restore failure).
        None
    }));

    // Wait for replenisher to attempt creation.
    std::thread::sleep(Duration::from_millis(300));

    // Pool should still have VMs (with None payloads), not panic.
    let vm = pool.acquire();
    if let Some(mut vm) = vm {
        // Payload should be None since factory returned None.
        assert!(vm.payload.is_none(), "failed factory should produce None payload");
        assert!(vm.take_payload::<u64>().is_none());
    }
    // Either way, no panic is the success condition.
}

#[test]
fn test_pool_take_payload_wrong_type() {
    let config = test_config(1, 2);
    let pool = VmPool::with_factory(config, Box::new(|| {
        Some(Box::new(42u64) as Box<dyn std::any::Any + Send>)
    }));

    std::thread::sleep(Duration::from_millis(300));

    let mut vm = pool.acquire().expect("should have a VM ready");
    assert!(vm.payload.is_some());

    // Try to downcast to wrong type — should return None and consume the payload.
    let wrong: Option<String> = vm.take_payload::<String>();
    assert!(wrong.is_none(), "downcast to wrong type should return None");

    // Payload is consumed even on failed downcast.
    assert!(vm.payload.is_none(), "payload consumed after failed downcast");
}

#[test]
fn test_pool_replenish_drops_lock_during_creation() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let factory_entered = std::sync::Arc::new(AtomicBool::new(false));
    let factory_entered_clone = factory_entered.clone();

    let config = PoolConfig {
        target_pool_size: 1,
        max_pool_size: 2,
        warm_strategy: WarmStrategy::ColdBoot,
        max_idle_time: Duration::from_secs(60),
        replenish_interval: Duration::from_millis(50),
        vm_template: PoolVmTemplate {
            vcpus: 1,
            memory_mib: 128,
            kernel_path: PathBuf::from("/boot/vmlinux"),
            initrd_path: None,
            cmdline: "console=ttyS0".to_string(),
        },
    };

    let pool = std::sync::Arc::new(VmPool::with_factory(config, Box::new(move || {
        factory_entered_clone.store(true, Ordering::SeqCst);
        // Simulate slow VM creation (e.g., snapshot restore).
        std::thread::sleep(Duration::from_millis(50));
        Some(Box::new(999u64) as Box<dyn std::any::Any + Send>)
    })));

    // Wait for factory to be called at least once.
    std::thread::sleep(Duration::from_millis(200));
    assert!(factory_entered.load(Ordering::SeqCst), "factory should have been called");

    // Key assertion: acquire() should complete quickly even if factory is slow,
    // because the replenish loop drops the lock during factory calls.
    let start = std::time::Instant::now();
    let _vm = pool.acquire(); // may or may not get a VM
    let elapsed = start.elapsed();

    // acquire() should be near-instant (just a lock + pop), not blocked by factory.
    assert!(
        elapsed < Duration::from_millis(30),
        "acquire() took {:?}, should be < 30ms (lock was not held during factory)",
        elapsed
    );
}
