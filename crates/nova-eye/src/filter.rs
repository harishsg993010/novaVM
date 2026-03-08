//! Event filtering for the sensor pipeline.

use std::collections::HashSet;

use nova_eye_common::{EventHeader, EventType};

/// Filters events by type and/or PID.
pub struct EventFilter {
    /// Allowed event types (empty = allow all).
    allowed_types: HashSet<u32>,
    /// Allowed PIDs (empty = allow all).
    allowed_pids: HashSet<u32>,
}

impl EventFilter {
    /// Create a new permissive filter (allows everything).
    pub fn new() -> Self {
        Self {
            allowed_types: HashSet::new(),
            allowed_pids: HashSet::new(),
        }
    }

    /// Allow only events of this type.
    pub fn allow_type(&mut self, event_type: EventType) {
        self.allowed_types.insert(event_type as u32);
    }

    /// Allow only events from this PID.
    pub fn allow_pid(&mut self, pid: u32) {
        self.allowed_pids.insert(pid);
    }

    /// Check whether an event header passes the filter.
    pub fn matches(&self, header: &EventHeader) -> bool {
        let type_ok =
            self.allowed_types.is_empty() || self.allowed_types.contains(&header.event_type);
        let pid_ok = self.allowed_pids.is_empty() || self.allowed_pids.contains(&header.pid);
        type_ok && pid_ok
    }
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::new()
    }
}
