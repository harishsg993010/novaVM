//! NovaVM guest agent — runs as PID 1 inside the microVM.
//!
//! The agent:
//! 1. Mounts essential filesystems (/proc, /sys, /dev, /tmp)
//! 2. Connects to the host VMM via vsock
//! 3. Enters a command loop, processing requests from the host
//! 4. Handles SIGCHLD to reap zombie processes (PID 1 responsibility)

mod error;
mod exec;
mod health;
mod protocol;
mod vsock;

use crate::health::HealthMonitor;
use crate::protocol::{Request, Response};
use crate::vsock::{VsockClient, DEFAULT_PORT, HOST_CID};

/// Agent version string.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Mount essential filesystems for a minimal Linux guest.
///
/// This is a no-op if not running as PID 1 (i.e., during testing).
fn mount_filesystems() -> error::Result<()> {
    use std::fs;

    let mounts = [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
        ("tmpfs", "/tmp", "tmpfs"),
        ("tmpfs", "/run", "tmpfs"),
    ];

    for (fstype, target, fs) in &mounts {
        // Create mount point if it doesn't exist.
        let _ = fs::create_dir_all(target);

        let ret = unsafe {
            libc::mount(
                fstype.as_ptr() as *const libc::c_char,
                target.as_ptr() as *const libc::c_char,
                fs.as_ptr() as *const libc::c_char,
                0,
                std::ptr::null(),
            )
        };

        if ret == 0 {
            tracing::info!(fstype, target, "mounted filesystem");
        } else {
            // Non-fatal: may already be mounted or not running as PID 1.
            tracing::debug!(
                fstype,
                target,
                err = %std::io::Error::last_os_error(),
                "mount failed (may be expected)"
            );
        }
    }

    Ok(())
}

/// Process a single request and return a response.
fn handle_request(request: &Request, health: &mut HealthMonitor) -> Response {
    match request {
        Request::Exec {
            command,
            env,
            workdir,
        } => {
            health.record_command();
            exec::execute_to_response(command, env, workdir.as_deref())
        }
        Request::HealthCheck => health.health_response(),
        Request::Version => Response::VersionResult {
            version: VERSION.to_string(),
        },
        Request::Shutdown => {
            tracing::info!("shutdown requested by host");
            // In production, this would sync filesystems and call reboot(2).
            Response::HealthResult {
                healthy: true,
                uptime_secs: health.uptime_secs(),
                commands_executed: health.commands_executed(),
            }
        }
    }
}

/// Run the agent command loop with a mock transport (for testing).
#[cfg(test)]
fn run_mock_loop(requests: Vec<Request>) -> Vec<Response> {
    use crate::vsock::MockVsockClient;
    let mut mock = MockVsockClient::new(requests);
    let mut health = HealthMonitor::new();
    let mut responses = Vec::new();

    loop {
        match mock.recv_request() {
            Ok(request) => {
                let is_shutdown = matches!(request, Request::Shutdown);
                let response = handle_request(&request, &mut health);
                mock.send_response(response.clone()).ok();
                responses.push(response);
                if is_shutdown {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    responses
}

fn main() {
    // Initialize tracing.
    tracing_subscriber::fmt()
        .with_target(false)
        .with_ansi(false)
        .init();

    tracing::info!(
        version = VERSION,
        pid = std::process::id(),
        "nova-agent starting"
    );

    // Mount essential filesystems (best-effort).
    if let Err(e) = mount_filesystems() {
        tracing::warn!(err = %e, "failed to mount filesystems");
    }

    // Connect to host via vsock.
    let mut client = match VsockClient::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(err = %e, "failed to create vsock client");
            std::process::exit(1);
        }
    };

    if let Err(e) = client.connect(HOST_CID, DEFAULT_PORT) {
        tracing::error!(err = %e, "failed to connect to host");
        std::process::exit(1);
    }

    tracing::info!("entering command loop");
    let mut health = HealthMonitor::new();

    loop {
        match client.recv_request() {
            Ok(request) => {
                let is_shutdown = matches!(request, Request::Shutdown);
                let response = handle_request(&request, &mut health);

                if let Err(e) = client.send_response(&response) {
                    tracing::error!(err = %e, "failed to send response");
                    break;
                }

                if is_shutdown {
                    tracing::info!("shutting down");
                    break;
                }
            }
            Err(e) => {
                tracing::error!(err = %e, "failed to receive request");
                break;
            }
        }
    }

    // Sync filesystems before exit.
    unsafe { libc::sync() };
    tracing::info!("nova-agent exiting");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_health_check() {
        let mut health = HealthMonitor::new();
        let response = handle_request(&Request::HealthCheck, &mut health);

        match response {
            Response::HealthResult {
                healthy,
                commands_executed,
                ..
            } => {
                assert!(healthy);
                assert_eq!(commands_executed, 0);
            }
            _ => panic!("expected HealthResult"),
        }
    }

    #[test]
    fn test_version() {
        let mut health = HealthMonitor::new();
        let response = handle_request(&Request::Version, &mut health);

        match response {
            Response::VersionResult { version } => {
                assert_eq!(version, VERSION);
            }
            _ => panic!("expected VersionResult"),
        }
    }

    #[test]
    fn test_exec_echo() {
        let mut health = HealthMonitor::new();
        let request = Request::Exec {
            command: vec!["echo".to_string(), "hello".to_string()],
            env: HashMap::new(),
            workdir: None,
        };

        let response = handle_request(&request, &mut health);
        match response {
            Response::ExecResult {
                exit_code, stdout, ..
            } => {
                assert_eq!(exit_code, 0);
                assert_eq!(stdout.trim(), "hello");
            }
            _ => panic!("expected ExecResult"),
        }

        assert_eq!(health.commands_executed(), 1);
    }

    #[test]
    fn test_exec_empty_command() {
        let mut health = HealthMonitor::new();
        let request = Request::Exec {
            command: vec![],
            env: HashMap::new(),
            workdir: None,
        };

        let response = handle_request(&request, &mut health);
        match response {
            Response::Error { message } => {
                assert!(message.contains("empty command"));
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn test_protocol_roundtrip() {
        let request = Request::Exec {
            command: vec!["ls".to_string(), "-la".to_string()],
            env: HashMap::new(),
            workdir: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();

        match parsed {
            Request::Exec { command, .. } => {
                assert_eq!(command, vec!["ls", "-la"]);
            }
            _ => panic!("expected Exec"),
        }
    }

    #[test]
    fn test_mock_loop() {
        let requests = vec![
            Request::Version,
            Request::HealthCheck,
            Request::Exec {
                command: vec!["echo".to_string(), "test".to_string()],
                env: HashMap::new(),
                workdir: None,
            },
            Request::Shutdown,
        ];

        let responses = run_mock_loop(requests);
        assert_eq!(responses.len(), 4);

        // Version response.
        assert!(matches!(&responses[0], Response::VersionResult { .. }));
        // Health response.
        assert!(matches!(&responses[1], Response::HealthResult { .. }));
        // Exec response.
        assert!(matches!(&responses[2], Response::ExecResult { .. }));
        // Shutdown response.
        assert!(matches!(&responses[3], Response::HealthResult { .. }));
    }

    #[test]
    fn test_health_monitor_tracks_commands() {
        let mut monitor = HealthMonitor::new();
        assert_eq!(monitor.commands_executed(), 0);

        monitor.record_command();
        monitor.record_command();
        monitor.record_command();

        assert_eq!(monitor.commands_executed(), 3);
        assert!(monitor.uptime_secs() < 5); // Shouldn't take 5 seconds.
    }
}
