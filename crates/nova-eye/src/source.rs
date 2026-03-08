//! Event sources for the sensor pipeline.
//!
//! The [`SensorSource`] trait abstracts event producers. Real eBPF sources
//! need root; [`SimulatedSource`] and [`ReplaySource`] enable testing.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use nova_eye_common::EventHeader;

use crate::error::Result;

/// A source of sensor events.
pub trait SensorSource: Send {
    /// Poll for new events, returning any available (header, raw_bytes) pairs.
    fn poll_events(&mut self) -> Result<Vec<(EventHeader, Vec<u8>)>>;

    /// Human-readable name of this source.
    fn name(&self) -> &str;
}

/// A simulated event source with a pre-loaded event queue.
pub struct SimulatedSource {
    name: String,
    events: Vec<(EventHeader, Vec<u8>)>,
}

impl SimulatedSource {
    /// Create a new simulated source.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            events: Vec::new(),
        }
    }

    /// Add a process exec event.
    pub fn add_process_exec(&mut self, pid: u32, comm: &str) {
        let header = make_header(nova_eye_common::EventType::ProcessExec as u32, pid, comm);
        let raw = header_to_bytes(&header);
        self.events.push((header, raw));
    }

    /// Add a file open event.
    pub fn add_file_open(&mut self, pid: u32, comm: &str) {
        let header = make_header(nova_eye_common::EventType::FileOpen as u32, pid, comm);
        let raw = header_to_bytes(&header);
        self.events.push((header, raw));
    }

    /// Add a net connect event.
    pub fn add_net_connect(&mut self, pid: u32, comm: &str) {
        let header = make_header(nova_eye_common::EventType::NetConnect as u32, pid, comm);
        let raw = header_to_bytes(&header);
        self.events.push((header, raw));
    }

    /// Add a raw event.
    pub fn add_raw(&mut self, header: EventHeader, raw: Vec<u8>) {
        self.events.push((header, raw));
    }
}

impl SensorSource for SimulatedSource {
    fn poll_events(&mut self) -> Result<Vec<(EventHeader, Vec<u8>)>> {
        Ok(self.events.drain(..).collect())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// A source that replays events from a JSON-lines file.
pub struct ReplaySource {
    name: String,
    events: Vec<(EventHeader, Vec<u8>)>,
}

impl ReplaySource {
    /// Create a replay source from a JSON-lines file.
    ///
    /// Each line should be a JSON object with fields: type, pid, tid, uid, comm, ts, raw_len.
    pub fn from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                let event_type = v["type"].as_u64().unwrap_or(0) as u32;
                let pid = v["pid"].as_u64().unwrap_or(0) as u32;
                let tid = v["tid"].as_u64().unwrap_or(pid as u64) as u32;
                let uid = v["uid"].as_u64().unwrap_or(0) as u32;
                let ts = v["ts"].as_u64().unwrap_or(0);
                let comm_str = v["comm"].as_str().unwrap_or("");

                let mut header = EventHeader {
                    event_type,
                    timestamp_ns: ts,
                    pid,
                    tid,
                    uid,
                    gid: 0,
                    comm: [0u8; 16],
                };
                let comm_bytes = comm_str.as_bytes();
                let len = comm_bytes.len().min(16);
                header.comm[..len].copy_from_slice(&comm_bytes[..len]);

                let raw = header_to_bytes(&header);
                events.push((header, raw));
            }
        }

        Ok(Self {
            name: format!("replay:{}", path.display()),
            events,
        })
    }
}

impl SensorSource for ReplaySource {
    fn poll_events(&mut self) -> Result<Vec<(EventHeader, Vec<u8>)>> {
        Ok(self.events.drain(..).collect())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Helper to build an EventHeader.
fn make_header(event_type: u32, pid: u32, comm: &str) -> EventHeader {
    let mut h = EventHeader {
        event_type,
        pid,
        tid: pid,
        uid: 1000,
        gid: 1000,
        comm: [0u8; 16],
        timestamp_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    };
    let bytes = comm.as_bytes();
    let len = bytes.len().min(16);
    h.comm[..len].copy_from_slice(&bytes[..len]);
    h
}

/// Serialize an EventHeader to raw bytes.
fn header_to_bytes(h: &EventHeader) -> Vec<u8> {
    let ptr = h as *const EventHeader as *const u8;
    let len = core::mem::size_of::<EventHeader>();
    unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
}
