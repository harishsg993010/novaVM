//! Pre-warmed VM pool (L4).
//!
//! Maintains a pool of N pre-booted VMs. `acquire()` returns a ready VM
//! instantly. The pool replenishes automatically via a background thread.

use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How to warm VMs in the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WarmStrategy {
    /// Cold boot from kernel + initrd each time.
    ColdBoot,
    /// Restore from a snapshot directory.
    SnapshotRestore {
        snapshot_dir: PathBuf,
    },
}

/// Template for creating VMs in the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolVmTemplate {
    pub vcpus: u32,
    pub memory_mib: u32,
    pub kernel_path: PathBuf,
    pub initrd_path: Option<PathBuf>,
    pub cmdline: String,
}

/// Configuration for the VM pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Target number of ready VMs to maintain.
    pub target_pool_size: usize,
    /// Maximum pool size (hard cap).
    pub max_pool_size: usize,
    /// How to warm VMs.
    pub warm_strategy: WarmStrategy,
    /// Max time a VM can sit idle before being expired.
    pub max_idle_time: Duration,
    /// How often to check and replenish the pool.
    pub replenish_interval: Duration,
    /// VM template for creating new VMs.
    pub vm_template: PoolVmTemplate,
}

impl PoolConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.target_pool_size == 0 {
            return Err("target_pool_size must be > 0".into());
        }
        if self.max_pool_size < self.target_pool_size {
            return Err("max_pool_size must be >= target_pool_size".into());
        }
        if self.vm_template.vcpus == 0 {
            return Err("vcpus must be > 0".into());
        }
        if self.vm_template.memory_mib == 0 {
            return Err("memory_mib must be > 0".into());
        }
        Ok(())
    }
}

/// A pre-warmed VM ready to use.
pub struct WarmVm {
    /// Unique ID for this warm VM.
    pub id: u64,
    /// When this VM was warmed.
    pub warmed_at: Instant,
    /// How it was created.
    pub origin: WarmStrategy,
    /// Type-erased VM payload (e.g., BuiltVm). None for token-only pools.
    pub payload: Option<Box<dyn Any + Send>>,
}

impl WarmVm {
    /// Extract the payload as a concrete type. Returns `None` if the payload
    /// is absent or the downcast fails.
    pub fn take_payload<T: 'static>(&mut self) -> Option<T> {
        self.payload
            .take()
            .and_then(|b| b.downcast::<T>().ok())
            .map(|b| *b)
    }
}

/// Factory function that creates a type-erased VM payload.
pub type VmFactory = dyn Fn() -> Option<Box<dyn Any + Send>> + Send + Sync;

/// Statistics about the pool.
#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    /// Number of VMs ready for immediate use.
    pub ready_count: usize,
    /// Total VMs assigned to callers.
    pub total_assigned: u64,
    /// Total VMs created (cold boot or restore).
    pub total_created: u64,
    /// Total VMs expired due to idle timeout.
    pub total_expired: u64,
    /// Number of acquire() calls that found an empty pool.
    pub pool_misses: u64,
}

/// Internal shared state for the pool.
struct PoolInner {
    ready: VecDeque<WarmVm>,
    stats: PoolStats,
    shutdown: bool,
    next_id: u64,
    target_size: usize,
}

/// Pre-warmed VM pool with background replenishment.
pub struct VmPool {
    config: PoolConfig,
    inner: Arc<(Mutex<PoolInner>, Condvar)>,
    replenish_handle: Option<JoinHandle<()>>,
    #[allow(dead_code)]
    factory: Option<Arc<VmFactory>>,
}

impl VmPool {
    /// Create a new token-only VM pool and start the background replenisher.
    /// VMs will have `payload: None`. Use `with_factory()` for real payloads.
    pub fn new(config: PoolConfig) -> Self {
        let inner = Arc::new((
            Mutex::new(PoolInner {
                ready: VecDeque::new(),
                stats: PoolStats::default(),
                shutdown: false,
                next_id: 1,
                target_size: config.target_pool_size,
            }),
            Condvar::new(),
        ));

        let handle = {
            let inner = Arc::clone(&inner);
            let config = config.clone();
            thread::spawn(move || Self::replenish_loop(inner, config, None))
        };

        Self {
            config,
            inner,
            replenish_handle: Some(handle),
            factory: None,
        }
    }

    /// Create a VM pool with a factory closure that produces real VM payloads.
    /// The factory is called outside the pool lock for each new VM.
    pub fn with_factory(
        config: PoolConfig,
        factory: Box<dyn Fn() -> Option<Box<dyn Any + Send>> + Send + Sync>,
    ) -> Self {
        let factory = Arc::new(factory);
        let inner = Arc::new((
            Mutex::new(PoolInner {
                ready: VecDeque::new(),
                stats: PoolStats::default(),
                shutdown: false,
                next_id: 1,
                target_size: config.target_pool_size,
            }),
            Condvar::new(),
        ));

        let handle = {
            let inner = Arc::clone(&inner);
            let config = config.clone();
            let factory = Arc::clone(&factory);
            thread::spawn(move || Self::replenish_loop(inner, config, Some(factory)))
        };

        Self {
            config,
            inner,
            replenish_handle: Some(handle),
            factory: Some(factory),
        }
    }

    /// Acquire a pre-warmed VM from the pool.
    /// Returns `None` if the pool is empty.
    pub fn acquire(&self) -> Option<WarmVm> {
        let (lock, cvar) = &*self.inner;
        let mut inner = lock.lock().unwrap();

        if let Some(vm) = inner.ready.pop_front() {
            inner.stats.total_assigned += 1;
            inner.stats.ready_count = inner.ready.len();
            // Signal replenisher to create a replacement.
            cvar.notify_one();
            Some(vm)
        } else {
            inner.stats.pool_misses += 1;
            None
        }
    }

    /// Acquire a VM with a timeout. Waits up to `timeout` for a VM to become available.
    pub fn acquire_timeout(&self, timeout: Duration) -> Option<WarmVm> {
        let (lock, cvar) = &*self.inner;
        let mut inner = lock.lock().unwrap();

        if let Some(vm) = inner.ready.pop_front() {
            inner.stats.total_assigned += 1;
            inner.stats.ready_count = inner.ready.len();
            cvar.notify_one();
            return Some(vm);
        }

        // Wait for a VM to become available.
        let result = cvar
            .wait_timeout_while(inner, timeout, |inner| {
                inner.ready.is_empty() && !inner.shutdown
            })
            .unwrap();

        inner = result.0;
        if let Some(vm) = inner.ready.pop_front() {
            inner.stats.total_assigned += 1;
            inner.stats.ready_count = inner.ready.len();
            cvar.notify_one();
            Some(vm)
        } else {
            inner.stats.pool_misses += 1;
            None
        }
    }

    /// Return an unused VM to the pool.
    pub fn release(&self, vm: WarmVm) {
        let (lock, _cvar) = &*self.inner;
        let mut inner = lock.lock().unwrap();
        if inner.ready.len() < self.config.max_pool_size {
            inner.ready.push_back(vm);
            inner.stats.ready_count = inner.ready.len();
        }
    }

    /// Get current pool statistics.
    pub fn stats(&self) -> PoolStats {
        let (lock, _) = &*self.inner;
        let inner = lock.lock().unwrap();
        inner.stats.clone()
    }

    /// Dynamically resize the target pool size.
    pub fn set_target_size(&self, n: usize) {
        let (lock, cvar) = &*self.inner;
        let mut inner = lock.lock().unwrap();
        inner.target_size = n;
        cvar.notify_one();
    }

    /// Shut down the pool and stop the background thread.
    pub fn shutdown(&mut self) {
        {
            let (lock, cvar) = &*self.inner;
            let mut inner = lock.lock().unwrap();
            inner.shutdown = true;
            cvar.notify_all();
        }

        if let Some(handle) = self.replenish_handle.take() {
            let _ = handle.join();
        }
    }

    /// Background replenisher loop.
    fn replenish_loop(
        inner: Arc<(Mutex<PoolInner>, Condvar)>,
        config: PoolConfig,
        factory: Option<Arc<VmFactory>>,
    ) {
        let (lock, cvar) = &*inner;

        loop {
            let mut state = lock.lock().unwrap();

            // Wait until we need more VMs or shutdown.
            let result = cvar
                .wait_timeout_while(state, config.replenish_interval, |s| {
                    !s.shutdown && s.ready.len() >= s.target_size
                })
                .unwrap();

            state = result.0;

            if state.shutdown {
                break;
            }

            // Expire idle VMs.
            let now = Instant::now();
            let before = state.ready.len();
            state.ready.retain(|vm| {
                now.duration_since(vm.warmed_at) < config.max_idle_time
            });
            let expired = before - state.ready.len();
            state.stats.total_expired += expired as u64;

            // Compute how many VMs we need and allocate IDs under lock.
            let current = state.ready.len();
            let target = state.target_size.min(config.max_pool_size);
            let needed = target.saturating_sub(current);

            let mut ids = Vec::with_capacity(needed);
            for _ in 0..needed {
                ids.push(state.next_id);
                state.next_id += 1;
            }

            state.stats.ready_count = state.ready.len();
            cvar.notify_all();

            // Drop lock before creating VMs (factory may be slow, e.g. ~48ms snapshot restore).
            drop(state);

            // Create VMs outside lock.
            let mut new_vms = Vec::with_capacity(ids.len());
            for id in ids {
                let payload = factory.as_ref().and_then(|f| f());
                new_vms.push(WarmVm {
                    id,
                    warmed_at: Instant::now(),
                    origin: config.warm_strategy.clone(),
                    payload,
                });
            }

            // Re-acquire lock and push new VMs.
            if !new_vms.is_empty() {
                let mut state = lock.lock().unwrap();
                for vm in new_vms {
                    if state.ready.len() < config.max_pool_size {
                        state.ready.push_back(vm);
                        state.stats.total_created += 1;
                    }
                }
                state.stats.ready_count = state.ready.len();
                cvar.notify_all();
            }
        }
    }
}

impl Drop for VmPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}
