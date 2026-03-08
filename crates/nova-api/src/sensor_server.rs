//! SensorService gRPC implementation.
//!
//! Provides the server-side implementation of the SensorService defined
//! in `sensor.proto`. Manages eBPF program state and streams telemetry
//! events to connected clients via broadcast channels.

use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::sensor::sensor_service_server::SensorService;
use crate::sensor::*;

/// Per-program tracking information.
#[derive(Debug, Clone)]
pub struct ProgramInfo {
    pub name: String,
    pub attached: bool,
    pub attach_type: String,
    pub attach_target: String,
    pub event_count: u64,
}

/// Internal state for the sensor subsystem.
pub struct SensorState {
    pub programs: Vec<ProgramInfo>,
    pub total_events: u64,
    pub dropped_events: u64,
}

impl SensorState {
    pub fn new() -> Self {
        Self {
            programs: Vec::new(),
            total_events: 0,
            dropped_events: 0,
        }
    }
}

/// gRPC SensorService implementation.
pub struct SensorDaemonService {
    pub state: Arc<Mutex<SensorState>>,
    pub event_tx: broadcast::Sender<SensorEvent>,
}

impl SensorDaemonService {
    pub fn new(
        state: Arc<Mutex<SensorState>>,
        event_tx: broadcast::Sender<SensorEvent>,
    ) -> Self {
        Self { state, event_tx }
    }
}

#[tonic::async_trait]
impl SensorService for SensorDaemonService {
    type StreamEventsStream = ReceiverStream<Result<SensorEvent, Status>>;

    async fn stream_events(
        &self,
        request: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let req = request.into_inner();
        let filter_types: Vec<i32> = req.event_types.iter().map(|t| *t as i32).collect();
        let filter_sandbox = req.sandbox_id.clone();

        let mut broadcast_rx = self.event_tx.subscribe();
        let (tx, rx) = mpsc::channel(256);

        // Spawn a forwarder task that filters events and sends to this client.
        tokio::spawn(async move {
            loop {
                match broadcast_rx.recv().await {
                    Ok(event) => {
                        // Apply type filter.
                        if !filter_types.is_empty()
                            && !filter_types.contains(&event.event_type)
                        {
                            continue;
                        }
                        // Apply sandbox filter.
                        if !filter_sandbox.is_empty()
                            && event.sandbox_id != filter_sandbox
                        {
                            continue;
                        }
                        if tx.send(Ok(event)).await.is_err() {
                            break; // Client disconnected.
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "sensor stream client lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn get_status(
        &self,
        _request: Request<GetSensorStatusRequest>,
    ) -> Result<Response<SensorStatus>, Status> {
        let state = self.state.lock().await;

        let programs = state
            .programs
            .iter()
            .map(|p| crate::sensor::ProgramStatus {
                name: p.name.clone(),
                attached: p.attached,
                attach_type: p.attach_type.clone(),
                attach_target: p.attach_target.clone(),
                event_count: p.event_count,
            })
            .collect();

        Ok(Response::new(SensorStatus {
            loaded_programs: state.programs.len() as u32,
            total_events: state.total_events,
            dropped_events: state.dropped_events,
            programs,
        }))
    }

    async fn load_program(
        &self,
        request: Request<LoadProgramRequest>,
    ) -> Result<Response<LoadProgramResponse>, Status> {
        let req = request.into_inner();
        let mut state = self.state.lock().await;

        // Check for duplicate.
        if state.programs.iter().any(|p| p.name == req.name) {
            return Ok(Response::new(LoadProgramResponse {
                success: false,
                error_message: format!("program '{}' already loaded", req.name),
            }));
        }

        state.programs.push(ProgramInfo {
            name: req.name.clone(),
            attached: true,
            attach_type: req.attach_type.clone(),
            attach_target: req.attach_target.clone(),
            event_count: 0,
        });

        tracing::info!(name = %req.name, "sensor program loaded");

        Ok(Response::new(LoadProgramResponse {
            success: true,
            error_message: String::new(),
        }))
    }

    async fn unload_program(
        &self,
        request: Request<UnloadProgramRequest>,
    ) -> Result<Response<UnloadProgramResponse>, Status> {
        let req = request.into_inner();
        let mut state = self.state.lock().await;

        let initial_len = state.programs.len();
        state.programs.retain(|p| p.name != req.name);

        if state.programs.len() == initial_len {
            return Ok(Response::new(UnloadProgramResponse {
                success: false,
                error_message: format!("program '{}' not found", req.name),
            }));
        }

        tracing::info!(name = %req.name, "sensor program unloaded");

        Ok(Response::new(UnloadProgramResponse {
            success: true,
            error_message: String::new(),
        }))
    }
}
