//! NovaVM gRPC API definitions.
//!
//! This crate contains the protobuf/gRPC service definitions for all NovaVM
//! subsystems. Code is generated at build time by `tonic-build` from the
//! `.proto` files in the `proto/` directory.

pub mod config;
pub mod image_server;
pub mod policy_server;
pub mod registry;
pub mod rest;
pub mod sensor_server;
pub mod server;

/// Runtime service — sandbox lifecycle management.
pub mod runtime {
    tonic::include_proto!("nova.runtime.v1");
}

/// Sandbox image service — OCI image pull and management.
pub mod sandbox {
    tonic::include_proto!("nova.sandbox.v1");
}

/// Sensor service — eBPF telemetry event streaming.
pub mod sensor {
    tonic::include_proto!("nova.sensor.v1");
}

/// Policy service — OPA policy evaluation and bundle management.
pub mod policy {
    tonic::include_proto!("nova.policy.v1");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_state_values() {
        assert_eq!(runtime::SandboxState::Unknown as i32, 0);
        assert_eq!(runtime::SandboxState::Created as i32, 1);
        assert_eq!(runtime::SandboxState::Running as i32, 2);
        assert_eq!(runtime::SandboxState::Stopped as i32, 3);
        assert_eq!(runtime::SandboxState::Error as i32, 4);
    }

    #[test]
    fn test_event_type_values() {
        assert_eq!(sensor::EventType::Unknown as i32, 0);
        assert_eq!(sensor::EventType::ProcessExec as i32, 1);
        assert_eq!(sensor::EventType::FileOpen as i32, 4);
        assert_eq!(sensor::EventType::NetConnect as i32, 7);
        assert_eq!(sensor::EventType::HttpRequest as i32, 10);
        assert_eq!(sensor::EventType::DnsQuery as i32, 12);
    }

    #[test]
    fn test_create_sandbox_request() {
        let req = runtime::CreateSandboxRequest {
            sandbox_id: "test-sandbox-1".to_string(),
            image: "docker.io/library/nginx:latest".to_string(),
            config: Some(runtime::SandboxConfig {
                vcpus: 2,
                memory_mib: 256,
                kernel: "/boot/vmlinux".to_string(),
                rootfs: "/images/nginx.ext4".to_string(),
                cmdline: "console=ttyS0".to_string(),
                network: Some(runtime::NetworkConfig {
                    tap_device: "tap0".to_string(),
                    guest_ip: "172.16.0.2/24".to_string(),
                    host_ip: "172.16.0.1/24".to_string(),
                    mac_address: "AA:BB:CC:DD:EE:01".to_string(),
                }),
                env: std::collections::HashMap::new(),
            }),
            command: String::new(),
        };
        assert_eq!(req.sandbox_id, "test-sandbox-1");
        assert_eq!(req.config.as_ref().unwrap().vcpus, 2);
    }

    #[test]
    fn test_evaluate_request() {
        let req = policy::EvaluateRequest {
            policy_path: "nova/sandbox/allow".to_string(),
            input: b"{\"image\": \"nginx\"}".to_vec(),
        };
        assert_eq!(req.policy_path, "nova/sandbox/allow");
    }

    #[test]
    fn test_sensor_event() {
        let event = sensor::SensorEvent {
            event_type: sensor::EventType::ProcessExec as i32,
            timestamp_ns: 123_456_789,
            pid: 1234,
            tid: 1234,
            uid: 1000,
            comm: "nginx".to_string(),
            sandbox_id: "sb-1".to_string(),
            payload: b"{}".to_vec(),
        };
        assert_eq!(event.pid, 1234);
        assert_eq!(event.event_type, 1);
    }

    #[test]
    fn test_image_info() {
        let info = sandbox::ImageInfo {
            image_ref: "nginx:latest".to_string(),
            digest: "sha256:abc123".to_string(),
            size_bytes: 1024 * 1024 * 50,
            pulled_at: "2025-01-01T00:00:00Z".to_string(),
            format: "ext4".to_string(),
            rootfs_path: "/images/nginx.ext4".to_string(),
        };
        assert_eq!(info.size_bytes, 52_428_800);
    }
}
