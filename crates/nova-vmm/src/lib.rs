//! NovaVM VMM — Virtual Machine Monitor library.
//!
//! Re-exports the builder, exit handler, configuration, device manager,
//! snapshot support, and network setup so integration tests and other
//! crates can use them.

#![allow(dead_code)]

pub mod builder;
pub mod config;
pub mod device_mgr;
pub mod exit_handler;
pub mod network;
pub mod snapshot;
