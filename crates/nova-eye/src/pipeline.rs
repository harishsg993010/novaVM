//! Telemetry export pipeline.
//!
//! Defines the [`TelemetrySink`] trait and several built-in implementations
//! for exporting events to different destinations (stdout, file, gRPC,
//! OpenTelemetry).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use nova_eye_common::EventHeader;

use crate::aggregator::EventAggregator;
use crate::error::{EyeError, Result};
use crate::filter::EventFilter;
use crate::source::SensorSource;

/// Trait for exporting processed events to an external sink.
///
/// Each sink receives the parsed [`EventHeader`], the full raw byte
/// slice of the event, and the `source_name` identifying the event source
/// (e.g. `"guest:sandbox-1"` for guest VM events). Sinks are responsible
/// for serializing and transmitting the data.
pub trait TelemetrySink {
    /// Send a single event to this sink.
    ///
    /// `source_name` identifies the event source. For guest VM events it
    /// has the form `"guest:<sandbox_id>"`.
    fn send(&mut self, event: &EventHeader, raw: &[u8], source_name: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// StdoutSink
// ---------------------------------------------------------------------------

/// Prints events to standard output in a human-readable format.
pub struct StdoutSink {
    /// Number of events written so far.
    count: u64,
}

impl StdoutSink {
    /// Create a new `StdoutSink`.
    pub fn new() -> Self {
        Self { count: 0 }
    }

    /// Returns the number of events sent through this sink.
    pub fn event_count(&self) -> u64 {
        self.count
    }
}

impl Default for StdoutSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetrySink for StdoutSink {
    fn send(&mut self, event: &EventHeader, raw: &[u8], _source_name: &str) -> Result<()> {
        println!(
            "[eye] type={} pid={} tid={} uid={} comm={} ts={} raw_len={}",
            event.event_type,
            event.pid,
            event.tid,
            event.uid,
            event.comm_str(),
            event.timestamp_ns,
            raw.len(),
        );
        self.count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FileSink
// ---------------------------------------------------------------------------

/// Writes events to a file as JSON lines (one JSON object per line).
pub struct FileSink {
    /// Path to the output file.
    #[allow(dead_code)]
    path: PathBuf,
    /// Open file handle.
    file: File,
    /// Number of events written so far.
    count: u64,
}

impl FileSink {
    /// Create a new `FileSink` that writes to the given path.
    ///
    /// The file is opened in append mode; it is created if it does not exist.
    pub fn new(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;

        tracing::info!(path = %path.display(), "opened file sink");

        Ok(Self {
            path: path.to_path_buf(),
            file,
            count: 0,
        })
    }

    /// Returns the number of events written to this file.
    pub fn event_count(&self) -> u64 {
        self.count
    }
}

impl TelemetrySink for FileSink {
    fn send(&mut self, event: &EventHeader, raw: &[u8], _source_name: &str) -> Result<()> {
        // Encode as a minimal JSON line. In production, use serde_json.
        let line = format!(
            "{{\"type\":{},\"pid\":{},\"tid\":{},\"uid\":{},\"comm\":\"{}\",\"ts\":{},\"raw_len\":{}}}\n",
            event.event_type,
            event.pid,
            event.tid,
            event.uid,
            event.comm_str(),
            event.timestamp_ns,
            raw.len(),
        );
        self.file
            .write_all(line.as_bytes())
            .map_err(EyeError::IoError)?;
        self.count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GrpcSink
// ---------------------------------------------------------------------------

/// Placeholder sink for exporting events via gRPC.
///
/// A real implementation would use `tonic` to stream events to a
/// remote collector (e.g., a nova-api telemetry endpoint).
#[allow(dead_code)]
pub struct GrpcSink {
    /// The gRPC endpoint URL.
    endpoint: String,
    /// Number of events sent.
    count: u64,
}

impl GrpcSink {
    /// Create a new `GrpcSink` targeting the given endpoint.
    #[allow(dead_code)]
    pub fn new(endpoint: &str) -> Self {
        tracing::info!(endpoint, "creating gRPC sink (placeholder)");
        Self {
            endpoint: endpoint.to_string(),
            count: 0,
        }
    }
}

impl TelemetrySink for GrpcSink {
    fn send(&mut self, event: &EventHeader, raw: &[u8], _source_name: &str) -> Result<()> {
        // Placeholder: log that we would send the event.
        tracing::debug!(
            endpoint = %self.endpoint,
            event_type = event.event_type,
            raw_len = raw.len(),
            "would send event via gRPC"
        );
        self.count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OtelSink
// ---------------------------------------------------------------------------

/// Placeholder sink for exporting events via OpenTelemetry.
///
/// A real implementation would convert events into OpenTelemetry spans
/// or log records and export them using the OTel SDK.
#[allow(dead_code)]
pub struct OtelSink {
    /// The OTLP endpoint URL.
    endpoint: String,
    /// Service name for the OTel resource.
    service_name: String,
    /// Number of events sent.
    count: u64,
}

impl OtelSink {
    /// Create a new `OtelSink` targeting the given OTLP endpoint.
    #[allow(dead_code)]
    pub fn new(endpoint: &str, service_name: &str) -> Self {
        tracing::info!(endpoint, service_name, "creating OTel sink (placeholder)");
        Self {
            endpoint: endpoint.to_string(),
            service_name: service_name.to_string(),
            count: 0,
        }
    }
}

impl TelemetrySink for OtelSink {
    fn send(&mut self, event: &EventHeader, raw: &[u8], _source_name: &str) -> Result<()> {
        // Placeholder: log that we would send the event.
        tracing::debug!(
            endpoint = %self.endpoint,
            service = %self.service_name,
            event_type = event.event_type,
            raw_len = raw.len(),
            "would send event via OpenTelemetry"
        );
        self.count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JsondSink
// ---------------------------------------------------------------------------

/// Writes events as Tetragon-style JSON lines to a log file.
///
/// Each event is written as a single JSON line containing structured
/// event data with type, process info, and timestamp. The file can be
/// tailed (`tail -f /var/run/nova/events.jsonl`) for real-time monitoring.
///
/// The daemon configures the log path via `NOVA_EVENTS_LOG` env var
/// (default: `/var/run/nova/events.jsonl`).
pub struct JsondSink {
    /// Path to the output file.
    path: PathBuf,
    /// Open file handle.
    file: File,
    /// Number of events written so far.
    count: u64,
}

impl JsondSink {
    /// Create a new `JsondSink` that writes to the given path.
    ///
    /// Creates parent directories if they don't exist.
    /// The file is opened in append mode; it is created if it does not exist.
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(EyeError::IoError)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;

        tracing::info!(path = %path.display(), "opened JSOND event log");

        Ok(Self {
            path: path.to_path_buf(),
            file,
            count: 0,
        })
    }

    /// Returns the path to the event log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the number of events written to this file.
    pub fn event_count(&self) -> u64 {
        self.count
    }
}

impl TelemetrySink for JsondSink {
    fn send(&mut self, event: &EventHeader, raw: &[u8], source_name: &str) -> Result<()> {
        let event_type_str = match event.event_type {
            1 => "process_exec",
            2 => "process_exit",
            3 => "process_fork",
            10 => "file_open",
            11 => "file_write",
            12 => "file_unlink",
            20 => "net_connect",
            21 => "net_accept",
            22 => "net_close",
            30 => "http_request",
            31 => "http_response",
            40 => "dns_query",
            _ => "unknown",
        };

        // Extract sandbox_id from source_name if it's a guest source ("guest:<id>").
        let sandbox_id = source_name.strip_prefix("guest:").unwrap_or("");
        let sandbox_field = if sandbox_id.is_empty() {
            String::new()
        } else {
            format!(",\"sandbox_id\":\"{}\"", sandbox_id)
        };

        let line = format!(
            "{{\"event_type\":\"{}\",\"timestamp_ns\":{},\"pid\":{},\"tid\":{},\"uid\":{},\"gid\":{},\"comm\":\"{}\",\"raw_len\":{}{}}}\n",
            event_type_str,
            event.timestamp_ns,
            event.pid,
            event.tid,
            event.uid,
            event.gid,
            event.comm_str(),
            raw.len(),
            sandbox_field,
        );
        self.file
            .write_all(line.as_bytes())
            .map_err(EyeError::IoError)?;
        self.count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ChannelSink
// ---------------------------------------------------------------------------

/// Sink that sends events through a crossbeam channel.
///
/// This bridges the pipeline to the gRPC SensorService without creating
/// circular dependencies. The daemon creates this sink, passes the sender
/// side, and a background task reads the receiver to convert events into
/// proto SensorEvent messages.
///
/// The channel carries `(EventHeader, Vec<u8>, String)` where the third
/// element is the source name (used to extract `sandbox_id` for guest events).
pub struct ChannelSink {
    sender: crossbeam_channel::Sender<(EventHeader, Vec<u8>, String)>,
    count: u64,
}

impl ChannelSink {
    /// Create a new `ChannelSink` backed by the given crossbeam sender.
    pub fn new(sender: crossbeam_channel::Sender<(EventHeader, Vec<u8>, String)>) -> Self {
        Self { sender, count: 0 }
    }

    /// Returns the number of events sent through this sink.
    pub fn event_count(&self) -> u64 {
        self.count
    }
}

impl TelemetrySink for ChannelSink {
    fn send(&mut self, event: &EventHeader, raw: &[u8], source_name: &str) -> Result<()> {
        let _ = self.sender.send((*event, raw.to_vec(), source_name.to_string()));
        self.count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SensorPipeline
// ---------------------------------------------------------------------------

/// End-to-end sensor pipeline: sources -> aggregator -> filter -> sinks.
pub struct SensorPipeline {
    sources: Vec<Box<dyn SensorSource>>,
    aggregator: EventAggregator,
    filter: EventFilter,
    sinks: Vec<Box<dyn TelemetrySink>>,
}

impl SensorPipeline {
    /// Create a new empty pipeline.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            aggregator: EventAggregator::new(),
            filter: EventFilter::new(),
            sinks: Vec::new(),
        }
    }

    /// Add an event source.
    pub fn add_source(&mut self, source: Box<dyn SensorSource>) {
        self.sources.push(source);
    }

    /// Add a telemetry sink.
    pub fn add_sink(&mut self, sink: Box<dyn TelemetrySink>) {
        self.sinks.push(sink);
    }

    /// Set the event filter.
    pub fn set_filter(&mut self, filter: EventFilter) {
        self.filter = filter;
    }

    /// Get a mutable reference to the aggregator.
    pub fn aggregator_mut(&mut self) -> &mut EventAggregator {
        &mut self.aggregator
    }

    /// Run one tick cycle: poll sources -> filter -> dispatch to sinks.
    ///
    /// Returns the number of events dispatched.
    pub fn tick(&mut self) -> Result<usize> {
        // Poll all sources into the aggregator.
        self.aggregator.poll_sources(&mut self.sources)?;

        // Drain events from aggregator, filter, and send to sinks.
        let mut dispatched = 0;
        let mut sink_handler = PipelineSinkHandler {
            filter: &self.filter,
            sinks: &mut self.sinks,
            dispatched: &mut dispatched,
        };
        self.aggregator.process_events(&mut sink_handler)?;
        Ok(dispatched)
    }

    /// Run N tick cycles.
    pub fn run_cycles(&mut self, n: usize) -> Result<usize> {
        let mut total = 0;
        for _ in 0..n {
            total += self.tick()?;
        }
        Ok(total)
    }
}

impl Default for SensorPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal handler that filters events and dispatches to sinks.
struct PipelineSinkHandler<'a> {
    filter: &'a EventFilter,
    sinks: &'a mut Vec<Box<dyn TelemetrySink>>,
    dispatched: &'a mut usize,
}

impl<'a> crate::aggregator::EventHandler for PipelineSinkHandler<'a> {
    fn handle_event(&mut self, header: &EventHeader, raw: &[u8], source_name: &str) {
        if !self.filter.matches(header) {
            return;
        }
        for sink in self.sinks.iter_mut() {
            let _ = sink.send(header, raw, source_name);
        }
        *self.dispatched += 1;
    }
}
