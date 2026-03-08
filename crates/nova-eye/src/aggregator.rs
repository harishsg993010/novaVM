//! Event aggregation from eBPF ring buffers.
//!
//! `EventAggregator` reads raw events from eBPF ring buffers (or perf
//! buffers) and dispatches them to an [`EventHandler`] for processing.
//!
//! In production the ring buffer would be backed by `aya::maps::RingBuf`.
//! This module provides the structural skeleton with a simulated event
//! queue so the rest of the pipeline can be tested.

use nova_eye_common::EventHeader;

use crate::error::Result;
use crate::source::SensorSource;

/// Trait for handling events read from the eBPF ring buffer.
///
/// Implementors receive the common [`EventHeader`] plus the full raw
/// byte slice of the event (which includes the header). The handler
/// can inspect `header.event_type` to decide how to decode the rest.
///
/// `source_name` identifies the event source (e.g. `"guest:my-sandbox"`
/// for guest VM events, or `"ebpf:process_exec:..."` for host eBPF).
pub trait EventHandler {
    /// Called for each event read from the ring buffer.
    fn handle_event(&mut self, header: &EventHeader, raw: &[u8], source_name: &str);
}

/// Reads events from eBPF ring buffers and dispatches them to handlers.
///
/// In production this would own an `aya::maps::RingBuf` or
/// `aya::maps::PerfEventArray`. The current implementation maintains a
/// simple in-memory queue for testing purposes.
pub struct EventAggregator {
    /// Simulated event queue (header, raw bytes, source name).
    pending_events: Vec<(EventHeader, Vec<u8>, String)>,
}

impl EventAggregator {
    /// Create a new `EventAggregator` with an empty event queue.
    pub fn new() -> Self {
        tracing::debug!("creating new EventAggregator");
        Self {
            pending_events: Vec::new(),
        }
    }

    /// Push a simulated event into the aggregator for testing.
    ///
    /// In production, events would arrive from the kernel ring buffer
    /// automatically. This method allows test code to inject events.
    /// `source_name` identifies the event source (e.g. `"guest:sandbox-1"`).
    pub fn push_event(&mut self, header: EventHeader, raw: Vec<u8>, source_name: String) {
        self.pending_events.push((header, raw, source_name));
    }

    /// Process all pending events, dispatching each to the given handler.
    ///
    /// In a real implementation this would poll the eBPF ring buffer
    /// (blocking or with a timeout) and decode each event. Here we drain
    /// the simulated event queue.
    pub fn process_events(&mut self, handler: &mut dyn EventHandler) -> Result<()> {
        let events: Vec<_> = self.pending_events.drain(..).collect();

        for (header, raw, source_name) in &events {
            tracing::trace!(
                event_type = header.event_type,
                pid = header.pid,
                "dispatching event"
            );
            handler.handle_event(header, raw, source_name);
        }

        if !events.is_empty() {
            tracing::debug!(count = events.len(), "processed events");
        }

        Ok(())
    }

    /// Poll all sources and push their events into the aggregator.
    ///
    /// Returns the total number of events collected.
    pub fn poll_sources(&mut self, sources: &mut [Box<dyn SensorSource>]) -> Result<usize> {
        let mut total = 0;
        for source in sources.iter_mut() {
            let source_name = source.name().to_string();
            let events = source.poll_events()?;
            total += events.len();
            for (header, raw) in events {
                self.push_event(header, raw, source_name.clone());
            }
        }
        Ok(total)
    }

    /// Returns the number of pending (unprocessed) events.
    pub fn pending_count(&self) -> usize {
        self.pending_events.len()
    }
}

impl Default for EventAggregator {
    fn default() -> Self {
        Self::new()
    }
}
