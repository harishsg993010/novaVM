//! REST API gateway — runs alongside the gRPC server on a TCP port.
//!
//! Provides a JSON/HTTP interface to the same sandbox runtime that the
//! gRPC `RuntimeService` exposes.  Every endpoint maps 1-to-1 to a gRPC RPC.
//!
//! Default port: **9800** (configurable via `daemon.api_port` in nova.toml).

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use nova_runtime::{
    SandboxOrchestrator,
    SandboxState as RuntimeSandboxState,
};

use crate::server::VmHandle;

// ── Shared state ────────────────────────────────────────────────────

/// State shared between the REST API and the gRPC server.
#[derive(Clone)]
pub struct RestState {
    pub orchestrator: Arc<Mutex<SandboxOrchestrator>>,
    pub vm_handles: Arc<Mutex<HashMap<String, VmHandle>>>,
    pub socket_path: String,
}

// ── Request / response types ────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateSandboxReq {
    #[serde(default)]
    pub sandbox_id: String,
    #[serde(default = "default_image")]
    pub image: String,
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_memory")]
    pub memory: u32,
}

fn default_image() -> String { "python:3.11-slim".into() }
fn default_vcpus() -> u32 { 1 }
fn default_memory() -> u32 { 256 }

#[derive(Serialize)]
pub struct CreateSandboxResp {
    pub sandbox_id: String,
}

#[derive(Serialize)]
pub struct SandboxInfo {
    pub sandbox_id: String,
    pub state: String,
    pub pid: i32,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct ExecReq {
    pub command: String,
}

#[derive(Serialize)]
pub struct ExecResp {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Serialize)]
pub struct ErrorResp {
    pub error: String,
}

#[derive(Deserialize)]
pub struct RunCodeReq {
    pub code: String,
}

#[derive(Serialize)]
pub struct RunCodeResp {
    pub text: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Deserialize)]
pub struct WriteFileReq {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct ReadFileReq {
    pub path: String,
}

#[derive(Serialize)]
pub struct ReadFileResp {
    pub content: String,
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: RestState) -> Router {
    Router::new()
        .route("/api/v1/sandboxes", post(create_sandbox))
        .route("/api/v1/sandboxes", get(list_sandboxes))
        .route("/api/v1/sandboxes/{id}", get(get_sandbox))
        .route("/api/v1/sandboxes/{id}", delete(destroy_sandbox))
        .route("/api/v1/sandboxes/{id}/stop", post(stop_sandbox))
        .route("/api/v1/sandboxes/{id}/exec", post(exec_in_sandbox))
        .route("/api/v1/sandboxes/{id}/code", post(run_code))
        .route("/api/v1/sandboxes/{id}/files/write", post(write_file))
        .route("/api/v1/sandboxes/{id}/files/read", post(read_file))
        .route("/health", get(health))
        .with_state(state)
}

// ── Handlers ────────────────────────────────────────────────────────

async fn health() -> &'static str {
    "ok"
}

/// POST /api/v1/sandboxes
///
/// Creates and starts a sandbox by calling the gRPC RuntimeService
/// internally over the Unix socket (REST→gRPC gateway pattern).
async fn create_sandbox(
    State(state): State<RestState>,
    Json(req): Json<CreateSandboxReq>,
) -> impl IntoResponse {
    let sandbox_id = if req.sandbox_id.is_empty() {
        format!("nova-{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() & 0xFFFF_FFFF)
    } else {
        req.sandbox_id
    };

    // Connect to the daemon's own gRPC socket.
    let socket_path = state.socket_path.clone();
    let channel = match tonic::transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
    {
        Ok(ch) => ch,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("gRPC connect failed: {}", e)})),
            );
        }
    };

    use crate::runtime::runtime_service_client::RuntimeServiceClient;
    let mut client = RuntimeServiceClient::new(channel);

    // Step 1: Create.
    let create_req = crate::runtime::CreateSandboxRequest {
        sandbox_id: sandbox_id.clone(),
        image: req.image,
        config: Some(crate::runtime::SandboxConfig {
            vcpus: req.vcpus,
            memory_mib: req.memory,
            kernel: String::new(),
            rootfs: String::new(),
            cmdline: String::new(),
            network: None,
            env: std::collections::HashMap::new(),
        }),
        command: String::new(),
    };
    if let Err(e) = client.create_sandbox(create_req).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("CreateSandbox: {}", e.message())})),
        );
    }

    // Step 2: Start.
    let start_req = crate::runtime::StartSandboxRequest {
        sandbox_id: sandbox_id.clone(),
    };
    if let Err(e) = client.start_sandbox(start_req).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("StartSandbox: {}", e.message())})),
        );
    }

    (StatusCode::CREATED, Json(serde_json::json!({"sandbox_id": sandbox_id})))
}

/// GET /api/v1/sandboxes
async fn list_sandboxes(
    State(state): State<RestState>,
) -> impl IntoResponse {
    let orch = state.orchestrator.lock().await;
    let sandboxes: Vec<SandboxInfo> = orch
        .list()
        .iter()
        .map(|sb| SandboxInfo {
            sandbox_id: sb.id().to_string(),
            state: format!("{}", sb.state()),
            pid: 0,
            created_at: String::new(),
        })
        .collect();
    Json(sandboxes)
}

/// GET /api/v1/sandboxes/:id
async fn get_sandbox(
    State(state): State<RestState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let orch = state.orchestrator.lock().await;
    match orch.get(&id) {
        Ok(sb) => {
            let info = SandboxInfo {
                sandbox_id: sb.id().to_string(),
                state: format!("{}", sb.state()),
                pid: 0,
                created_at: String::new(),
            };
            (StatusCode::OK, Json(serde_json::to_value(info).unwrap()))
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/v1/sandboxes/:id/stop
async fn stop_sandbox(
    State(state): State<RestState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Set the shutdown flag on the VM handle.
    let handles = state.vm_handles.lock().await;
    match handles.get(&id) {
        Some(handle) => {
            handle.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            drop(handles);
            // Update orchestrator state.
            let mut orch = state.orchestrator.lock().await;
            let _ = orch.stop(&id);
            (StatusCode::OK, Json(serde_json::json!({"sandbox_id": id, "action": "stopped"})))
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("sandbox '{}' not found", id)})),
        ),
    }
}

/// DELETE /api/v1/sandboxes/:id
async fn destroy_sandbox(
    State(state): State<RestState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Signal shutdown.
    {
        let handles = state.vm_handles.lock().await;
        if let Some(handle) = handles.get(&id) {
            handle.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    // Remove from orchestrator.
    let mut orch = state.orchestrator.lock().await;
    let _ = orch.stop(&id);
    let _ = orch.destroy(&id);
    drop(orch);
    // Remove VM handle.
    let mut handles = state.vm_handles.lock().await;
    handles.remove(&id);
    (StatusCode::OK, Json(serde_json::json!({"sandbox_id": id, "action": "destroyed"})))
}

/// POST /api/v1/sandboxes/:id/exec
async fn exec_in_sandbox(
    State(state): State<RestState>,
    Path(id): Path<String>,
    Json(req): Json<ExecReq>,
) -> impl IntoResponse {
    // Verify sandbox is running.
    {
        let orch = state.orchestrator.lock().await;
        match orch.get(&id) {
            Ok(sb) => {
                if sb.state() != RuntimeSandboxState::Running {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::to_value(ErrorResp {
                            error: format!("sandbox '{}' is not running", id),
                        }).unwrap()),
                    );
                }
            }
            Err(e) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::to_value(ErrorResp {
                        error: e.to_string(),
                    }).unwrap()),
                );
            }
        }
    }

    // Get console handles.
    let (console_input, console_output) = {
        let handles = state.vm_handles.lock().await;
        match handles.get(&id) {
            Some(handle) => (
                Arc::clone(&handle.console_input),
                Arc::clone(&handle.console_output),
            ),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::to_value(ErrorResp {
                        error: format!("no VM handle for sandbox '{}'", id),
                    }).unwrap()),
                );
            }
        }
    };

    // Record current console output length as baseline.
    let baseline = {
        let buf = console_output.lock().unwrap();
        buf.len()
    };

    // Send command via serial.
    {
        let mut input = console_input.lock().unwrap();
        let cmd_with_newline = format!("{}\n", req.command);
        input.extend(cmd_with_newline.as_bytes());
    }

    // Wait for markers (up to 30s).
    let begin_marker = "===NOVA_EXEC_BEGIN===";
    let end_marker = "===NOVA_EXEC_END===";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

    let mut stdout_result = String::new();
    let mut exit_code;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let output_text = {
            let buf = console_output.lock().unwrap();
            if buf.len() > baseline {
                let new_bytes: Vec<u8> = buf.iter().skip(baseline).copied().collect();
                String::from_utf8_lossy(&new_bytes).to_string()
            } else {
                String::new()
            }
        };

        if let Some(end_pos) = output_text.find(end_marker) {
            if let Some(begin_pos) = output_text.find(begin_marker) {
                let content_start = begin_pos + begin_marker.len();
                let content = &output_text[content_start..end_pos];
                stdout_result = content.trim_matches('\n').trim_matches('\r').to_string();
            }
            let after_end = &output_text[end_pos + end_marker.len()..];
            exit_code = after_end.trim().parse::<i32>().unwrap_or(0);
            break;
        }

        if std::time::Instant::now() >= deadline {
            exit_code = -1;
            break;
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(ExecResp {
            stdout: stdout_result,
            stderr: String::new(),
            exit_code,
        }).unwrap()),
    )
}

/// POST /api/v1/sandboxes/:id/code — run Python code with persistent state.
async fn run_code(
    State(state): State<RestState>,
    Path(id): Path<String>,
    Json(req): Json<RunCodeReq>,
) -> impl IntoResponse {
    // Install REPL helper if needed, then write code and execute.
    // Delegate to exec for each step.
    let exec_state = state.clone();

    // Step 1: Write code to file via base64.
    let encoded = base64::encode(req.code.as_bytes());
    let write_cmd = format!("echo {} | base64 -d > /tmp/_novavm_code.py", encoded);

    let write_req = ExecReq { command: write_cmd };
    let write_result = exec_inner(&exec_state, &id, &write_req.command).await;
    if write_result.exit_code != 0 {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::to_value(RunCodeResp {
                text: String::new(),
                stdout: write_result.stdout,
                stderr: write_result.stderr,
                exit_code: write_result.exit_code,
            }).unwrap()),
        );
    }

    // Step 2: Run via python3.
    let result = exec_inner(&exec_state, &id, "python3 -c \"exec(open('/tmp/_novavm_code.py').read())\"").await;

    let text = result.stdout
        .replace("\r\n", "\n")
        .replace('\r', "")
        .trim_end_matches('\n')
        .to_string();

    (
        StatusCode::OK,
        Json(serde_json::to_value(RunCodeResp {
            text,
            stdout: result.stdout.clone(),
            stderr: result.stderr,
            exit_code: result.exit_code,
        }).unwrap()),
    )
}

/// POST /api/v1/sandboxes/:id/files/write
async fn write_file(
    State(state): State<RestState>,
    Path(id): Path<String>,
    Json(req): Json<WriteFileReq>,
) -> impl IntoResponse {
    let encoded = base64::encode(req.content.as_bytes());
    let cmd = format!("echo {} | base64 -d > {}", encoded, req.path);
    let result = exec_inner(&state, &id, &cmd).await;
    if result.exit_code == 0 {
        (StatusCode::OK, Json(serde_json::json!({"ok": true})))
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": result.stderr})))
    }
}

/// POST /api/v1/sandboxes/:id/files/read
async fn read_file(
    State(state): State<RestState>,
    Path(id): Path<String>,
    Json(req): Json<ReadFileReq>,
) -> impl IntoResponse {
    let cmd = format!("cat {}", req.path);
    let result = exec_inner(&state, &id, &cmd).await;
    (
        StatusCode::OK,
        Json(serde_json::to_value(ReadFileResp {
            content: result.stdout,
        }).unwrap()),
    )
}

// ── Internal helpers ────────────────────────────────────────────────

/// Base64 encode helper (simple, no dependency).
mod base64 {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let n = (b0 << 16) | (b1 << 8) | b2;
            result.push(CHARS[((n >> 18) & 63) as usize] as char);
            result.push(CHARS[((n >> 12) & 63) as usize] as char);
            if chunk.len() > 1 {
                result.push(CHARS[((n >> 6) & 63) as usize] as char);
            } else {
                result.push('=');
            }
            if chunk.len() > 2 {
                result.push(CHARS[(n & 63) as usize] as char);
            } else {
                result.push('=');
            }
        }
        result
    }
}

/// Shared exec logic (same as the exec_in_sandbox handler but returns a struct).
async fn exec_inner(state: &RestState, sandbox_id: &str, command: &str) -> ExecResult {
    // Get console handles.
    let (console_input, console_output) = {
        let handles = state.vm_handles.lock().await;
        match handles.get(sandbox_id) {
            Some(handle) => (
                Arc::clone(&handle.console_input),
                Arc::clone(&handle.console_output),
            ),
            None => {
                return ExecResult {
                    stdout: String::new(),
                    stderr: format!("no VM handle for sandbox '{}'", sandbox_id),
                    exit_code: 1,
                };
            }
        }
    };

    let baseline = {
        let buf = console_output.lock().unwrap();
        buf.len()
    };

    {
        let mut input = console_input.lock().unwrap();
        input.extend(format!("{}\n", command).as_bytes());
    }

    let begin_marker = "===NOVA_EXEC_BEGIN===";
    let end_marker = "===NOVA_EXEC_END===";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let output_text = {
            let buf = console_output.lock().unwrap();
            if buf.len() > baseline {
                let new_bytes: Vec<u8> = buf.iter().skip(baseline).copied().collect();
                String::from_utf8_lossy(&new_bytes).to_string()
            } else {
                String::new()
            }
        };

        if let Some(end_pos) = output_text.find(end_marker) {
            let mut stdout = String::new();
            if let Some(begin_pos) = output_text.find(begin_marker) {
                let content_start = begin_pos + begin_marker.len();
                stdout = output_text[content_start..end_pos]
                    .trim_matches('\n')
                    .trim_matches('\r')
                    .to_string();
            }
            let after_end = &output_text[end_pos + end_marker.len()..];
            let exit_code = after_end.trim().parse::<i32>().unwrap_or(0);
            return ExecResult { stdout, stderr: String::new(), exit_code };
        }

        if std::time::Instant::now() >= deadline {
            return ExecResult {
                stdout: String::new(),
                stderr: "command timed out".into(),
                exit_code: -1,
            };
        }
    }
}

/// Internal exec result (not the protobuf one).
struct ExecResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
}
