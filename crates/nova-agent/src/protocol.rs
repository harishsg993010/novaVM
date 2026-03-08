//! Wire protocol for host-agent communication.
//!
//! Messages are exchanged as length-prefixed JSON over vsock.
//! The host sends [`Request`] messages, the agent replies with [`Response`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A request sent from the host VMM to the guest agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    /// Execute a command inside the guest.
    #[serde(rename = "exec")]
    Exec {
        /// Command and arguments.
        command: Vec<String>,
        /// Optional environment variables.
        #[serde(default)]
        env: HashMap<String, String>,
        /// Optional working directory.
        #[serde(default)]
        workdir: Option<String>,
    },

    /// Request a health check.
    #[serde(rename = "health")]
    HealthCheck,

    /// Request the agent to shut down the guest.
    #[serde(rename = "shutdown")]
    Shutdown,

    /// Request the agent to report its version.
    #[serde(rename = "version")]
    Version,
}

/// A response sent from the guest agent to the host VMM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// Result of a command execution.
    #[serde(rename = "exec_result")]
    ExecResult {
        /// Exit code of the command.
        exit_code: i32,
        /// Standard output (UTF-8).
        stdout: String,
        /// Standard error (UTF-8).
        stderr: String,
    },

    /// Health check response.
    #[serde(rename = "health_result")]
    HealthResult {
        /// Whether the agent is healthy.
        healthy: bool,
        /// Uptime in seconds.
        uptime_secs: u64,
        /// Number of commands executed.
        commands_executed: u64,
    },

    /// Version response.
    #[serde(rename = "version_result")]
    VersionResult {
        /// Agent version string.
        version: String,
    },

    /// Error response.
    #[serde(rename = "error")]
    Error {
        /// Error message.
        message: String,
    },
}
