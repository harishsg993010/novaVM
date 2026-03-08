//! # nova-eye-common
//!
//! Shared event types for the NovaVM observability subsystem (nova-eye).
//! All event structs are `#[repr(C)]` for direct sharing between
//! eBPF kernel programs and userspace via ring buffers/perf maps.
//!
//! This crate supports both `no_std` (eBPF kernel side) and `std` (userspace).

#![cfg_attr(not(feature = "std"), no_std)]

pub mod events;

pub use events::*;
