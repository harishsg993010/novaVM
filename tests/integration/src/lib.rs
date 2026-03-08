//! NovaVM cross-crate integration tests.
//!
//! These tests verify that the various NovaVM subsystems work together
//! correctly across crate boundaries.

#[cfg(test)]
mod bench_test;
#[cfg(test)]
mod boot_test;
#[cfg(test)]
mod networking_test;
#[cfg(test)]
mod policy_test;
#[cfg(test)]
mod real_test;
#[cfg(test)]
mod sensor_test;
#[cfg(test)]
mod wasm_test;
#[cfg(test)]
mod oci_test;
#[cfg(test)]
mod wasm_runtime_test;
#[cfg(test)]
mod sensor_pipeline_test;
#[cfg(test)]
mod policy_eval_test;
#[cfg(test)]
mod e2e_test;
#[cfg(test)]
mod bench_subsystems_test;
#[cfg(test)]
mod e2e_full_test;
#[cfg(test)]
mod cache_l1_test;
#[cfg(test)]
mod cache_l2_test;
#[cfg(test)]
mod cache_l3_test;
#[cfg(test)]
mod cache_l4_test;
#[cfg(test)]
mod ebpf_observe_test;
#[cfg(test)]
mod ebpf_e2e_test;
#[cfg(test)]
mod guest_observe_test;
#[cfg(test)]
mod policy_daemon_test;
#[cfg(test)]
mod registry_pull_test;
