//! Health monitoring and heartbeat reporting.
//!
//! Tracks agent uptime and command statistics for health checks.

use std::time::Instant;

use crate::protocol::Response;

/// Tracks the health state of the guest agent.
pub struct HealthMonitor {
    /// When the agent started.
    start_time: Instant,
    /// Number of commands executed.
    commands_executed: u64,
}

impl HealthMonitor {
    /// Create a new health monitor.
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            commands_executed: 0,
        }
    }

    /// Record that a command was executed.
    pub fn record_command(&mut self) {
        self.commands_executed += 1;
    }

    /// Returns the uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Returns the number of commands executed.
    pub fn commands_executed(&self) -> u64 {
        self.commands_executed
    }

    /// Build a health check response.
    pub fn health_response(&self) -> Response {
        Response::HealthResult {
            healthy: true,
            uptime_secs: self.uptime_secs(),
            commands_executed: self.commands_executed,
        }
    }
}

impl Default for HealthMonitor {
    fn default() -> Self {
        Self::new()
    }
}
