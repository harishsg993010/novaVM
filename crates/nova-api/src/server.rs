//! RuntimeDaemon — gRPC server that wraps SandboxOrchestrator.
//!
//! Listens on a Unix domain socket and exposes the RuntimeService gRPC
//! interface for sandbox lifecycle management. When a kernel path is
//! configured, StartSandbox will boot a real KVM microVM.
//!
//! All configuration is loaded from a [`DaemonConfig`](crate::config::DaemonConfig)
//! TOML file — no env-var knobs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::config::DaemonConfig;
use crate::image_server::ImageDaemonService;
use crate::policy::policy_service_server::PolicyServiceServer;
use crate::policy_server::{PolicyDaemonService, PolicyState};
use crate::registry::RegistryClient;
use crate::runtime::runtime_service_server::{RuntimeService, RuntimeServiceServer};
use crate::runtime::*;
use crate::sandbox::sandbox_image_service_server::SandboxImageServiceServer;
use crate::sensor::sensor_service_server::SensorServiceServer;
use crate::sensor_server::{SensorDaemonService, SensorState};

use nova_runtime::{
    BlobStore, ImageCache, ImageFormat, ImagePuller, RootfsCache, SandboxOrchestrator,
    SandboxConfig as RuntimeSandboxConfig, SandboxKind,
    SandboxState as RuntimeSandboxState,
};
use nova_runtime::sandbox::NetworkConfig as RuntimeNetworkConfig;
use nova_runtime::snapshot_cache::{SnapshotCache, SnapshotEntry};
use nova_runtime::pool::{PoolConfig, PoolVmTemplate, VmPool, WarmStrategy};
use nova_vmm::snapshot::{save_snapshot, restore_snapshot, VmSnapshotConfig};

/// gRPC daemon that wraps the SandboxOrchestrator.
pub struct RuntimeDaemon {
    orchestrator: Arc<Mutex<SandboxOrchestrator>>,
    image_puller: Arc<Mutex<ImagePuller>>,
    socket_path: PathBuf,
    kernel_path: Option<PathBuf>,
    image_dir: PathBuf,
    /// Track running VM threads.
    vm_handles: Arc<Mutex<HashMap<String, VmHandle>>>,
    /// L3 snapshot cache (shared with VM threads via std::sync::Mutex).
    snapshot_cache: Arc<std::sync::Mutex<SnapshotCache>>,
    /// Per-sandbox image digest for L3 cache key computation.
    image_digests: Arc<Mutex<HashMap<String, String>>>,
    /// Per-sandbox command for L3 cache key computation.
    sandbox_commands: Arc<Mutex<HashMap<String, String>>>,
    /// Per-sandbox image name for L4 pool metadata capture.
    image_names: Arc<Mutex<HashMap<String, String>>>,
    /// L4 pre-warmed VM pool + metadata (lazily initialized after first L3 snapshot save).
    vm_pool: Arc<std::sync::Mutex<Option<(VmPool, L4PoolMeta)>>>,
    /// Sensor subsystem state (shared with SensorService).
    sensor_state: Arc<Mutex<SensorState>>,
    /// Broadcast channel for streaming sensor events to gRPC clients.
    event_tx: tokio::sync::broadcast::Sender<crate::sensor::SensorEvent>,
    /// Sensor configuration (from TOML).
    sensor_config: Arc<crate::config::SensorConfig>,
    /// Guest sensor configuration (from TOML).
    guest_sensor_config: Arc<crate::config::GuestSensorConfig>,
    /// Dynamic source sender — lets us add GuestEventSource at runtime.
    source_tx: crossbeam_channel::Sender<Box<dyn nova_eye::source::SensorSource>>,
    /// Dynamic source receiver — taken by the pipeline thread on start.
    source_rx: Arc<std::sync::Mutex<Option<crossbeam_channel::Receiver<Box<dyn nova_eye::source::SensorSource>>>>>,
    /// TAP device name for guest networking.
    tap_device: Option<String>,
    /// Policy subsystem state (shared with PolicyService and RuntimeService via std::sync::Mutex).
    policy_state: Arc<std::sync::Mutex<PolicyState>>,
    /// Image cache for tracking pulled images.
    image_cache: Arc<Mutex<ImageCache>>,
    /// REST API TCP port (0 = disabled).
    api_port: u16,
}

/// Handle to a running VM.
pub struct VmHandle {
    pub shutdown: Arc<std::sync::atomic::AtomicBool>,
    pub join_handle: Option<std::thread::JoinHandle<()>>,
    pub console_output: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
    pub console_input: nova_vmm::exit_handler::SerialInput,
}

/// Metadata stored alongside the L4 pool to enable fast-path matching.
#[allow(dead_code)]
struct L4PoolMeta {
    image_name: String,
    image_digest: String,
    config_hash: String,
    vm_config_toml: String,
    memory_mib: u32,
    vcpus: u32,
}

impl RuntimeDaemon {
    /// Create a new RuntimeDaemon from a `DaemonConfig`.
    pub fn with_config(config: DaemonConfig) -> Self {
        let socket_path = config.socket_path();
        let image_dir = config.image_dir();
        let kernel_path = config.kernel_path();
        let tap_device = config.daemon.tap_device.clone();
        let api_port = config.daemon.api_port;

        let sensor_config = Arc::new(config.sensor);
        let guest_sensor_config = Arc::new(sensor_config.guest.clone());
        let policy_state = Arc::new(std::sync::Mutex::new(
            PolicyState::from_config(&config.policy),
        ));

        Self::build(socket_path, image_dir, kernel_path, sensor_config, guest_sensor_config, tap_device, policy_state, api_port)
    }

    /// Backwards-compatible constructor (uses defaults for sensor config).
    pub fn new(socket_path: PathBuf, image_dir: PathBuf) -> Self {
        let kernel_path = std::env::var("NOVA_KERNEL").ok().map(PathBuf::from);
        let sensor_config = Arc::new(crate::config::SensorConfig::default());
        let guest_sensor_config = Arc::new(crate::config::GuestSensorConfig::default());
        let policy_state = Arc::new(std::sync::Mutex::new(
            PolicyState::from_config(&crate::config::PolicyConfig::default()),
        ));
        Self::build(socket_path, image_dir, kernel_path, sensor_config, guest_sensor_config, None, policy_state, 9800)
    }

    fn build(
        socket_path: PathBuf,
        image_dir: PathBuf,
        kernel_path: Option<PathBuf>,
        sensor_config: Arc<crate::config::SensorConfig>,
        guest_sensor_config: Arc<crate::config::GuestSensorConfig>,
        tap_device: Option<String>,
        policy_state: Arc<std::sync::Mutex<PolicyState>>,
        api_port: u16,
    ) -> Self {
        let orchestrator = Arc::new(Mutex::new(SandboxOrchestrator::new()));

        // Set up L1 blob store + L2 rootfs cache.
        let blob_dir = image_dir.join("blobs");
        let rootfs_dir = image_dir.join("rootfs");
        let blob_store = BlobStore::open(&blob_dir)
            .expect("failed to open L1 blob store");
        let rootfs_cache = RootfsCache::open(&rootfs_dir)
            .expect("failed to open L2 rootfs cache");
        let image_puller = Arc::new(Mutex::new(
            ImagePuller::with_caches(&image_dir, ImageFormat::Initramfs, blob_store, rootfs_cache)
                .expect("failed to initialize image puller"),
        ));
        tracing::info!("L1 blob store + L2 rootfs cache enabled");

        // Set up L3 snapshot cache.
        let snapshot_dir = image_dir.join("snapshots");
        let snapshot_cache = Arc::new(std::sync::Mutex::new(
            SnapshotCache::open(&snapshot_dir)
                .expect("failed to open L3 snapshot cache"),
        ));
        tracing::info!("L3 snapshot cache enabled");

        // Set up image cache.
        let image_cache = Arc::new(Mutex::new(
            ImageCache::new(&image_dir)
                .expect("failed to open image cache"),
        ));
        tracing::info!("image cache enabled");

        if let Some(ref k) = kernel_path {
            tracing::info!(kernel = %k.display(), "real VM boot enabled");
        }

        // Sensor subsystem: broadcast channel for event streaming.
        let (event_tx, _) = tokio::sync::broadcast::channel(4096);
        let sensor_state = Arc::new(Mutex::new(SensorState::new()));

        // Dynamic source channel for runtime source registration.
        let (source_tx, source_rx) = crossbeam_channel::bounded(64);

        Self {
            orchestrator,
            image_puller,
            socket_path,
            kernel_path,
            image_dir,
            vm_handles: Arc::new(Mutex::new(HashMap::new())),
            snapshot_cache,
            image_digests: Arc::new(Mutex::new(HashMap::new())),
            sandbox_commands: Arc::new(Mutex::new(HashMap::new())),
            image_names: Arc::new(Mutex::new(HashMap::new())),
            vm_pool: Arc::new(std::sync::Mutex::new(None)),
            sensor_state,
            event_tx,
            sensor_config,
            guest_sensor_config,
            source_tx,
            source_rx: Arc::new(std::sync::Mutex::new(Some(source_rx))),
            tap_device,
            policy_state,
            image_cache,
            api_port,
        }
    }

    /// Returns the socket path.
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Start serving on the configured Unix socket.
    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let _ = tokio::fs::remove_file(&self.socket_path).await;

        let uds = tokio::net::UnixListener::bind(&self.socket_path)?;
        let uds_stream = tokio_stream::wrappers::UnixListenerStream::new(uds);

        tracing::info!(socket = %self.socket_path.display(), "gRPC server listening");

        let service = RuntimeDaemonService {
            orchestrator: Arc::clone(&self.orchestrator),
            image_puller: Arc::clone(&self.image_puller),
            kernel_path: self.kernel_path.clone(),
            image_dir: self.image_dir.clone(),
            vm_handles: Arc::clone(&self.vm_handles),
            snapshot_cache: Arc::clone(&self.snapshot_cache),
            image_digests: Arc::clone(&self.image_digests),
            sandbox_commands: Arc::clone(&self.sandbox_commands),
            image_names: Arc::clone(&self.image_names),
            vm_pool: Arc::clone(&self.vm_pool),
            sensor_config: Arc::clone(&self.sensor_config),
            guest_sensor_config: Arc::clone(&self.guest_sensor_config),
            source_tx: self.source_tx.clone(),
            tap_device: self.tap_device.clone(),
            policy_state: Arc::clone(&self.policy_state),
        };

        let sensor_service = SensorDaemonService::new(
            Arc::clone(&self.sensor_state),
            self.event_tx.clone(),
        );

        let policy_service = PolicyDaemonService::new(
            Arc::clone(&self.policy_state),
        );

        let image_service = ImageDaemonService {
            registry_client: Arc::new(RegistryClient::new()),
            image_puller: Arc::clone(&self.image_puller),
            image_cache: Arc::clone(&self.image_cache),
            image_dir: self.image_dir.clone(),
        };

        // Start the background sensor pipeline.
        self.start_sensor_pipeline();

        // Start REST API server on TCP port (if enabled).
        if self.api_port > 0 {
            let rest_state = crate::rest::RestState {
                orchestrator: Arc::clone(&self.orchestrator),
                vm_handles: Arc::clone(&self.vm_handles),
                socket_path: self.socket_path.to_string_lossy().to_string(),
            };
            let rest_app = crate::rest::router(rest_state);
            let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.api_port));
            tracing::info!(port = self.api_port, "REST API server listening");
            tokio::spawn(async move {
                let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                axum::serve(listener, rest_app).await.unwrap();
            });
        }

        tonic::transport::Server::builder()
            .add_service(RuntimeServiceServer::new(service))
            .add_service(SensorServiceServer::new(sensor_service))
            .add_service(PolicyServiceServer::new(policy_service))
            .add_service(SandboxImageServiceServer::new(image_service))
            .serve_with_incoming(uds_stream)
            .await?;

        Ok(())
    }

    /// Start a background thread that runs the sensor pipeline.
    ///
    /// Uses config-driven probe loading (no env vars). When probes are
    /// configured, loads real eBPF sources; otherwise runs in simulated mode.
    /// A dynamic source channel allows `GuestEventSource` instances to be
    /// added at runtime when VMs boot.
    fn start_sensor_pipeline(&self) {
        let event_tx = self.event_tx.clone();
        let sensor_state = Arc::clone(&self.sensor_state);
        let sensor_config = Arc::clone(&self.sensor_config);
        let policy_state = Arc::clone(&self.policy_state);

        // Take the source_rx out of the shared slot (only one pipeline thread).
        let source_rx = self.source_rx.lock().unwrap().take()
            .expect("start_sensor_pipeline called more than once");

        std::thread::spawn(move || {
            use nova_eye::{AyaBpfSource, ChannelSink, JsondSink, SensorPipeline};
            let (ch_tx, ch_rx) = crossbeam_channel::bounded(4096);

            let mut pipeline = SensorPipeline::new();

            // Load eBPF probes from config.
            let has_probes = !sensor_config.probes.is_empty();
            if has_probes {
                let ebpf_dir = &sensor_config.ebpf_dir;

                for probe in &sensor_config.probes {
                    if !probe.enabled {
                        continue;
                    }
                    let bytecode_name = match &probe.bytecode {
                        Some(name) => name.clone(),
                        None => continue,
                    };
                    let bytecode_path = format!("{}/{}", ebpf_dir, bytecode_name);

                    match probe.hook_type.as_str() {
                        "tracepoint" if probe.target.contains("sched_process_exec") => {
                            match AyaBpfSource::load_process_exec(&bytecode_path) {
                                Ok(src) => {
                                    tracing::info!(path = %bytecode_path, "loaded eBPF process_exec");
                                    pipeline.add_source(Box::new(src));
                                }
                                Err(e) => tracing::warn!(err = %e, "failed to load process_exec eBPF"),
                            }
                        }
                        "kprobe" if probe.target == "tcp_v4_connect" => {
                            match AyaBpfSource::load_tcp_connect(&bytecode_path) {
                                Ok(src) => {
                                    tracing::info!(path = %bytecode_path, "loaded eBPF tcp_connect");
                                    pipeline.add_source(Box::new(src));
                                }
                                Err(e) => tracing::warn!(err = %e, "failed to load tcp_connect eBPF"),
                            }
                        }
                        "kprobe" if probe.target == "vfs_open" => {
                            match AyaBpfSource::load_file_monitor(&bytecode_path) {
                                Ok(src) => {
                                    tracing::info!(path = %bytecode_path, "loaded eBPF file_monitor");
                                    pipeline.add_source(Box::new(src));
                                }
                                Err(e) => tracing::warn!(err = %e, "failed to load file_monitor eBPF"),
                            }
                        }
                        "uprobe" if probe.target == "SSL_write" => {
                            let libssl = probe.binary.as_deref().unwrap_or("/usr/lib/libssl.so");
                            match AyaBpfSource::load_ssl_monitor(&bytecode_path, libssl) {
                                Ok(src) => {
                                    tracing::info!(path = %bytecode_path, "loaded eBPF ssl_write");
                                    pipeline.add_source(Box::new(src));
                                }
                                Err(e) => tracing::warn!(err = %e, "failed to load ssl_write eBPF"),
                            }
                        }
                        _ => {
                            tracing::info!(
                                hook = %probe.hook_type,
                                target = %probe.target,
                                "skipping probe (no loader for this hook type)"
                            );
                        }
                    }
                }
                tracing::info!(count = sensor_config.probes.len(), "sensor pipeline: config-driven probe loading");
            } else {
                tracing::info!("sensor pipeline: no probes configured");
            }

            // Add channel sink to bridge to gRPC.
            let sink = ChannelSink::new(ch_tx);
            pipeline.add_sink(Box::new(sink));

            // Add JSOND event log file sink.
            let events_log_path = &sensor_config.events_log;
            match JsondSink::new(std::path::Path::new(events_log_path)) {
                Ok(jsond_sink) => {
                    tracing::info!(path = %events_log_path, "JSOND event log enabled");
                    pipeline.add_sink(Box::new(jsond_sink));
                }
                Err(e) => {
                    tracing::warn!(path = %events_log_path, err = %e, "failed to open JSOND event log, continuing without it");
                }
            }

            tracing::info!("sensor pipeline started");

            // Background loop: poll pipeline, check dynamic sources, forward events.
            loop {
                // Check for dynamically added sources (e.g., GuestEventSource).
                while let Ok(src) = source_rx.try_recv() {
                    tracing::info!(name = src.name(), "dynamic source added to pipeline");
                    pipeline.add_source(src);
                }

                let _ = pipeline.tick();

                // Drain channel and forward to broadcast with enforcement.
                while let Ok((header, _raw, source_name)) = ch_rx.try_recv() {
                    let event_type_str = crate::policy_server::event_type_to_str(header.event_type);

                    // Runtime enforcement check.
                    let enforcement_action = {
                        if let Ok(mut ps) = policy_state.try_lock() {
                            if ps.enforcement_enabled {
                                ps.enforcement_engine.evaluate(event_type_str)
                            } else {
                                nova_policy::EnforcementAction::Allow
                            }
                        } else {
                            nova_policy::EnforcementAction::Allow
                        }
                    };

                    let proto_event_type = common_to_proto_event_type(header.event_type);
                    // Extract sandbox_id from source_name (e.g. "guest:my-sandbox" → "my-sandbox").
                    let sandbox_id = source_name.strip_prefix("guest:")
                        .unwrap_or("")
                        .to_string();
                    let event = crate::sensor::SensorEvent {
                        event_type: proto_event_type,
                        timestamp_ns: header.timestamp_ns,
                        pid: header.pid,
                        tid: header.tid,
                        uid: header.uid,
                        comm: header.comm_str().to_string(),
                        sandbox_id: sandbox_id.clone(),
                        payload: Vec::new(),
                    };

                    match enforcement_action {
                        nova_policy::EnforcementAction::Deny => {
                            tracing::warn!(
                                event_type = %event_type_str,
                                pid = header.pid,
                                "enforcement: DENY — event dropped"
                            );
                        }
                        nova_policy::EnforcementAction::Kill => {
                            tracing::error!(
                                event_type = %event_type_str,
                                pid = header.pid,
                                sandbox = %sandbox_id,
                                "enforcement: KILL"
                            );
                            // Forward the event so clients see it, but log the kill.
                            let _ = event_tx.send(event);
                        }
                        nova_policy::EnforcementAction::Alert => {
                            tracing::info!(
                                event_type = %event_type_str,
                                pid = header.pid,
                                "enforcement: ALERT"
                            );
                            let _ = event_tx.send(event);
                        }
                        nova_policy::EnforcementAction::Allow => {
                            let _ = event_tx.send(event);
                        }
                    }

                    // Update total event count.
                    if let Ok(mut state) = sensor_state.try_lock() {
                        state.total_events += 1;
                    }
                }

                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });
    }
}

/// Map nova-eye-common EventType u32 values to proto EventType i32 values.
fn common_to_proto_event_type(event_type: u32) -> i32 {
    match event_type {
        1 => crate::sensor::EventType::ProcessExec as i32,
        2 => crate::sensor::EventType::ProcessExit as i32,
        3 => crate::sensor::EventType::ProcessFork as i32,
        10 => crate::sensor::EventType::FileOpen as i32,
        11 => crate::sensor::EventType::FileWrite as i32,
        12 => crate::sensor::EventType::FileUnlink as i32,
        20 => crate::sensor::EventType::NetConnect as i32,
        21 => crate::sensor::EventType::NetAccept as i32,
        22 => crate::sensor::EventType::NetClose as i32,
        30 => crate::sensor::EventType::HttpRequest as i32,
        31 => crate::sensor::EventType::HttpResponse as i32,
        40 => crate::sensor::EventType::DnsQuery as i32,
        _ => crate::sensor::EventType::Unknown as i32,
    }
}

struct RuntimeDaemonService {
    orchestrator: Arc<Mutex<SandboxOrchestrator>>,
    image_puller: Arc<Mutex<ImagePuller>>,
    kernel_path: Option<PathBuf>,
    image_dir: PathBuf,
    vm_handles: Arc<Mutex<HashMap<String, VmHandle>>>,
    snapshot_cache: Arc<std::sync::Mutex<SnapshotCache>>,
    image_digests: Arc<Mutex<HashMap<String, String>>>,
    sandbox_commands: Arc<Mutex<HashMap<String, String>>>,
    image_names: Arc<Mutex<HashMap<String, String>>>,
    vm_pool: Arc<std::sync::Mutex<Option<(VmPool, L4PoolMeta)>>>,
    sensor_config: Arc<crate::config::SensorConfig>,
    guest_sensor_config: Arc<crate::config::GuestSensorConfig>,
    source_tx: crossbeam_channel::Sender<Box<dyn nova_eye::source::SensorSource>>,
    tap_device: Option<String>,
    policy_state: Arc<std::sync::Mutex<PolicyState>>,
}

fn state_to_proto(state: RuntimeSandboxState) -> i32 {
    match state {
        RuntimeSandboxState::Created => SandboxState::Created as i32,
        RuntimeSandboxState::Running => SandboxState::Running as i32,
        RuntimeSandboxState::Stopped => SandboxState::Stopped as i32,
        RuntimeSandboxState::Error => SandboxState::Error as i32,
    }
}

fn system_time_to_rfc3339(t: std::time::SystemTime) -> String {
    let duration = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let mut y = 1970;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap_year(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = is_leap_year(y);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut m = 0;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            m = i as u64 + 1;
            break;
        }
        remaining -= md;
    }
    (y, m, remaining + 1)
}

fn is_leap_year(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Build a TOML config string for nova-vmm from sandbox parameters.
fn build_vm_config(
    kernel_path: &std::path::Path,
    initrd_path: Option<&std::path::Path>,
    memory_mib: u32,
    vcpus: u32,
    cmdline: &str,
    tap_device: Option<&str>,
) -> String {
    // Add rdinit=/init to cmdline if using initramfs.
    let effective_cmdline = if initrd_path.is_some() && !cmdline.contains("rdinit=") {
        format!("{} rdinit=/init", cmdline)
    } else {
        cmdline.to_string()
    };

    let mut toml = format!(
        r#"vcpus = {}
memory_mib = {}

[kernel]
path = "{}"
cmdline = "{}"
boot_method = "elf"
"#,
        vcpus,
        memory_mib,
        kernel_path.display(),
        effective_cmdline,
    );

    if let Some(initrd) = initrd_path {
        toml.push_str(&format!("initrd = \"{}\"\n", initrd.display()));
    }

    if let Some(tap) = tap_device {
        toml.push_str(&format!(
            r#"
[network]
tap = "{}"
mac = "AA:BB:CC:DD:EE:01"
"#,
            tap
        ));
    }

    toml
}

/// Inject an /init script into a cpio file for OCI images.
/// Appends a second cpio archive with /init that configures networking and starts nginx.
fn inject_init_into_cpio(cpio_path: &std::path::Path, entry_script: Option<&[u8]>, kernel_path: Option<&std::path::Path>) -> std::io::Result<()> {
    let init_script = br#"#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
mkdir -p /proc /sys /dev /tmp /var/run 2>/dev/null
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
mount -t tmpfs none /tmp 2>/dev/null
mount -t tmpfs none /var/run 2>/dev/null

echo 'NovaVM: init started'

# Seed entropy for the CRNG (kernel 4.14 needs RNDADDENTROPY ioctl).
if [ -x /sbin/seed-entropy ]; then
    /sbin/seed-entropy
    echo "NovaVM: entropy seeded"
fi

# Configure networking if eth0 exists
if [ -d /sys/class/net/eth0 ]; then
    ip link set eth0 up
    ip addr add 172.16.0.2/30 dev eth0
    ip route add default via 172.16.0.1
    echo 'NovaVM: network configured (172.16.0.2)'
fi

# Mount filesystems needed for eBPF
mount -t debugfs none /sys/kernel/debug 2>/dev/null
mount -t tracefs none /sys/kernel/tracing 2>/dev/null
mkdir -p /sys/fs/bpf 2>/dev/null
mount -t bpf none /sys/fs/bpf 2>/dev/null

# Test UDP connectivity to host
echo 'NovaVM: testing UDP to host'
echo 'NOVA-INIT-UDP-TEST' | busybox nc -u -w1 172.16.0.1 9876 2>/dev/null &
sleep 1

# Start nova-eye eBPF telemetry agent (if injected)
if [ -x /sbin/nova-eye-agent ]; then
    /sbin/nova-eye-agent > /tmp/nova-eye-agent.log 2>&1 &
    sleep 2
    echo 'NovaVM: nova-eye-agent started'
    cat /tmp/nova-eye-agent.log 2>/dev/null

    # Test UDP again after agent starts
    echo 'NOVA-AGENT-UDP-TEST' | busybox nc -u -w1 172.16.0.1 9876 2>/dev/null &
fi

echo 'NovaVM: container ready'

# Run image-specific entry script if injected, otherwise auto-detect.
if [ -x /entry.sh ]; then
    echo 'NovaVM: running /entry.sh'
    /entry.sh
else
    echo 'NovaVM: no /entry.sh, auto-detect mode'
    if [ -x /usr/sbin/nginx ]; then
        mkdir -p /var/log/nginx /var/cache/nginx 2>/dev/null
        printf 'worker_processes 1;\nerror_log /tmp/nginx-error.log info;\npid /tmp/nginx.pid;\nevents { worker_connections 64; }\nhttp {\n  access_log off;\n  server {\n    listen 80;\n    location / {\n      return 200 "NovaVM nginx OK\\n";\n      add_header Content-Type text/plain;\n    }\n  }\n}\n' > /tmp/nginx.conf
        /usr/sbin/nginx -c /tmp/nginx.conf 2>&1
        echo "NovaVM: nginx started (exit=$?)"
    fi
fi

echo 'NovaVM: entry completed'

# Serial exec listener - reads commands from serial console and executes them.
# Uses markers so the host can extract command output.
exec 0</dev/ttyS0
exec 1>/dev/ttyS0
exec 2>/dev/ttyS0
while true; do
    read -r cmd || { sleep 1; continue; }
    [ -z "$cmd" ] && continue
    echo "===NOVA_EXEC_BEGIN==="
    eval "$cmd" 2>&1
    echo "===NOVA_EXEC_END===$?"
done
"#.to_vec();

    let mut extra_cpio = Vec::new();
    nova_boot::initrd::inject_file(&mut extra_cpio, "init", &init_script, 0o100755);

    // Inject image-specific entry script if provided.
    if let Some(entry) = entry_script {
        nova_boot::initrd::inject_file(&mut extra_cpio, "entry.sh", entry, 0o100755);
        tracing::info!(size = entry.len(), "injected /entry.sh into cpio");
    }

    // Inject seed-entropy binary (seeds CRNG via RNDADDENTROPY ioctl).
    // Look in kernel's directory, NOVA_SEED_ENTROPY env, or well-known paths.
    let mut seed_entropy_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("NOVA_SEED_ENTROPY") {
        seed_entropy_paths.push(std::path::PathBuf::from(p));
    }
    // Check sibling of the kernel binary (from config or env).
    if let Some(kp) = kernel_path {
        if let Some(dir) = kp.parent() {
            seed_entropy_paths.push(dir.join("seed-entropy"));
        }
    }
    if let Ok(k) = std::env::var("NOVA_KERNEL") {
        if let Some(dir) = std::path::Path::new(&k).parent() {
            seed_entropy_paths.push(dir.join("seed-entropy"));
        }
    }
    seed_entropy_paths.push(std::path::PathBuf::from("/usr/lib/nova/seed-entropy"));
    let mut seed_entropy_injected = false;
    for path in &seed_entropy_paths {
        if path.exists() {
            if let Ok(data) = std::fs::read(path) {
                // Inject as /sbin/seed-entropy with directory entry.
                nova_boot::initrd::inject_file(&mut extra_cpio, "sbin/seed-entropy", &data, 0o100755);
                seed_entropy_injected = true;
                tracing::info!(path = %path.display(), size = data.len(), "injected seed-entropy into cpio");
                break;
            }
        }
    }
    if !seed_entropy_injected {
        tracing::warn!("seed-entropy binary not found; VM may block on getrandom()");
    }

    // Append to existing cpio (Linux kernel processes concatenated cpios).
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(cpio_path)?;
    std::io::Write::write_all(&mut file, &extra_cpio)?;

    tracing::info!(
        path = %cpio_path.display(),
        init_size = extra_cpio.len(),
        "injected /init and seed-entropy into cpio"
    );

    Ok(())
}

/// Inject the guest eBPF agent binary, bytecode, and probe config into a cpio archive.
///
/// Appends additional cpio entries for:
/// - `/sbin/nova-eye-agent` — the agent binary
/// - `/opt/nova/ebpf/<name>` — each probe's bytecode
/// - `/etc/nova/probes.json` — probe configuration for the agent
fn inject_guest_ebpf(
    cpio_path: &std::path::Path,
    guest_config: &crate::config::GuestSensorConfig,
    sensor_config: &crate::config::SensorConfig,
) -> std::io::Result<()> {
    let mut extra_cpio = Vec::new();

    // Create directory entries needed for the injected files.
    // The kernel CPIO extractor needs explicit directory entries.
    for dir in &["etc/nova", "opt/nova", "opt/nova/ebpf"] {
        nova_boot::initrd::inject_file(&mut extra_cpio, dir, &[], 0o040755);
    }

    // Read and inject the agent binary.
    let agent_path = std::path::Path::new(&guest_config.agent_path);
    if agent_path.exists() {
        let agent_bin = std::fs::read(agent_path)?;
        nova_boot::initrd::inject_file(&mut extra_cpio, "sbin/nova-eye-agent", &agent_bin, 0o100755);
        tracing::info!(
            path = %agent_path.display(),
            size = agent_bin.len(),
            "injected nova-eye-agent into guest cpio"
        );
    } else {
        tracing::warn!(
            path = %agent_path.display(),
            "nova-eye-agent binary not found, skipping injection"
        );
        return Ok(());
    }

    // Inject each enabled probe's bytecode.
    for probe in &sensor_config.probes {
        if !probe.enabled {
            continue;
        }
        let bytecode_name = match &probe.bytecode {
            Some(name) => name,
            None => continue,
        };
        let bytecode_path = format!("{}/{}", sensor_config.ebpf_dir, bytecode_name);
        if let Ok(bytecode) = std::fs::read(&bytecode_path) {
            let guest_path = format!("opt/nova/ebpf/{}", bytecode_name);
            nova_boot::initrd::inject_file(&mut extra_cpio, &guest_path, &bytecode, 0o100644);
            tracing::info!(bytecode = %bytecode_name, size = bytecode.len(), "injected eBPF bytecode");
        } else {
            tracing::warn!(path = %bytecode_path, "eBPF bytecode not found, skipping");
        }
    }

    // Inject probe config as JSON for the agent to read.
    let probes_json = serde_json::to_vec(&sensor_config.probes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    nova_boot::initrd::inject_file(&mut extra_cpio, "etc/nova/probes.json", &probes_json, 0o100644);

    // Append to existing cpio.
    let mut file = std::fs::OpenOptions::new().append(true).open(cpio_path)?;
    std::io::Write::write_all(&mut file, &extra_cpio)?;

    tracing::info!(
        path = %cpio_path.display(),
        extra_size = extra_cpio.len(),
        "injected guest eBPF agent + bytecode into cpio"
    );

    Ok(())
}

/// Run a VM in a background thread with L3 snapshot support.
/// Returns the shutdown flag and join handle.
///
/// On cache HIT: restore VM from snapshot (skips kernel boot + init).
/// On cache MISS: cold boot → save snapshot at "container ready" → continue.
fn spawn_vm_thread(
    vm_config_toml: String,
    sandbox_id: String,
    snapshot_cache: Arc<std::sync::Mutex<SnapshotCache>>,
    image_digest: String,
    command: String,
    image_name: String,
    snapshot_base_dir: PathBuf,
    vm_pool: Arc<std::sync::Mutex<Option<(VmPool, L4PoolMeta)>>>,
) -> (Arc<std::sync::atomic::AtomicBool>, std::thread::JoinHandle<()>, Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>, nova_vmm::exit_handler::SerialInput) {
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let console_output: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>> =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let console_input: nova_vmm::exit_handler::SerialInput =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let console_output_clone = Arc::clone(&console_output);
    let console_input_clone = Arc::clone(&console_input);
    let shutdown_clone = Arc::clone(&shutdown);

    let handle = std::thread::spawn(move || {
        let console_buf = console_output_clone;
        let serial_input = console_input_clone;
        let shutdown_flag = shutdown_clone;
        tracing::info!(sandbox_id = %sandbox_id, "VM thread starting");

        // Extract TAP device name from TOML config for snapshot restore networking.
        let tap_name: Option<String> = nova_vmm::config::VmConfig::from_toml(&vm_config_toml)
            .ok()
            .and_then(|c| c.network.map(|n| n.tap));

        // Compute L3 cache key from image digest + hash(semantic config + command).
        // Hash only vcpus/memory/cmdline + command (NOT file paths which change
        // between L2-hit and L2-miss runs).
        let has_snapshot_support = !image_digest.is_empty();
        let (cache_key, config_hash) = if has_snapshot_support {
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                // Extract semantic config from TOML (skip paths).
                let config = nova_vmm::config::VmConfig::from_toml(&vm_config_toml).ok();
                if let Some(ref c) = config {
                    c.vcpus.hash(&mut hasher);
                    c.memory_mib.hash(&mut hasher);
                    c.kernel.cmdline.hash(&mut hasher);
                } else {
                    vm_config_toml.hash(&mut hasher);
                }
                command.hash(&mut hasher);
                format!("{:016x}", hasher.finish())
            };
            let key = SnapshotCache::make_key(&image_digest, &hash);
            tracing::info!(
                sandbox_id = %sandbox_id,
                cache_key = %key,
                "L3 cache key computed"
            );
            (key, hash)
        } else {
            (String::new(), String::new())
        };

        // ── L4 POOL CHECK: try to acquire a pre-warmed VM first ──
        let pool_vm = {
            let pool_guard = vm_pool.lock().unwrap();
            if let Some((ref pool, _)) = *pool_guard {
                pool.acquire()
            } else {
                None
            }
        };
        if let Some(mut warm_vm) = pool_vm {
            if let Some(vm) = warm_vm.take_payload::<nova_vmm::builder::BuiltVm>() {
                tracing::info!(
                    sandbox_id = %sandbox_id,
                    warm_vm_id = warm_vm.id,
                    "L4 pool HIT, using pre-warmed VM"
                );

                // Open TAP and set up host networking for the claimed pool VM.
                let mut built_vm = vm;
                if let Some(ref tap) = tap_name {
                    built_vm.mmio_bus.open_tap_for_net(tap);
                    let mut net_setup = nova_vmm::network::NetworkSetup::default_for_tap(tap);
                    if let Err(e) = net_setup.setup() {
                        tracing::warn!(error = %e, "host network setup failed (L4 pool VM)");
                    }
                    built_vm.network_setup = Some(net_setup);
                }

                // Jump straight to run loop with the pre-warmed VM.
                let timeout = std::time::Duration::from_secs(3600);
                let max_output = 256 * 1024;
                let capture_result = nova_vmm::exit_handler::run_vcpu_resume_capture_interactive(
                    &built_vm.vcpus[0],
                    &mut built_vm.mmio_bus,
                    timeout,
                    max_output,
                    Arc::clone(&serial_input),
                    Arc::clone(&console_buf),
                    Some(Arc::clone(&shutdown_flag)),
                );

                match capture_result {
                    Ok((output, stop_reason, diag)) => {
                        if let Ok(mut buf) = console_buf.lock() {
                            buf.extend(&output);
                        }
                        let text = String::from_utf8_lossy(&output);
                        tracing::info!(
                            sandbox_id = %sandbox_id,
                            stop_reason = ?stop_reason,
                            total_exits = diag.total_exits,
                            elapsed_ms = diag.elapsed.as_millis() as u64,
                            output_bytes = output.len(),
                            "VM stopped (L4 pool)"
                        );
                        if !text.is_empty() {
                            tracing::info!(sandbox_id = %sandbox_id, "console output:\n{}", text);
                        }
                    }
                    Err(e) => tracing::error!(sandbox_id = %sandbox_id, error = %e, "VM run error (L4 pool)"),
                }

                tracing::info!(sandbox_id = %sandbox_id, "VM thread exiting (L4 pool)");
                return;
            }
        }

        // Check L3 snapshot cache.
        let snapshot_hit = if has_snapshot_support {
            let cache = snapshot_cache.lock().unwrap();
            cache.get(&cache_key).cloned()
        } else {
            None
        };

        let mut built_vm = if let Some(entry) = snapshot_hit {
            // ── L3 CACHE HIT: restore from snapshot ──
            tracing::info!(
                sandbox_id = %sandbox_id,
                key = %cache_key,
                dir = %entry.snapshot_dir.display(),
                "L3 snapshot cache HIT, restoring VM"
            );
            match restore_snapshot(&entry.snapshot_dir, tap_name.as_deref()) {
                Ok(mut vm) => {
                    // Open TAP and set up host networking for the restored VM.
                    if let Some(ref tap) = tap_name {
                        vm.mmio_bus.open_tap_for_net(tap);
                        let mut net_setup = nova_vmm::network::NetworkSetup::default_for_tap(tap);
                        if let Err(e) = net_setup.setup() {
                            tracing::warn!(error = %e, "host network setup failed (L3 restore)");
                        }
                        vm.network_setup = Some(net_setup);
                    }
                    tracing::info!(sandbox_id = %sandbox_id, "VM restored from L3 snapshot");

                    // ── Lazy L4 pool init on L3 HIT (reuse existing snapshot dir) ──
                    {
                        let mut pool_guard = vm_pool.lock().unwrap();
                        if pool_guard.is_none() {
                            let snap_dir_for_factory = entry.snapshot_dir.clone();
                            let config = nova_vmm::config::VmConfig::from_toml(&vm_config_toml).ok();
                            let (vcpus, memory_mib, cmdline) = if let Some(ref c) = config {
                                (c.vcpus, c.memory_mib as u32, c.kernel.cmdline.clone())
                            } else {
                                (1, 256, String::new())
                            };
                            let pool_config = PoolConfig {
                                target_pool_size: 2,
                                max_pool_size: 4,
                                warm_strategy: WarmStrategy::SnapshotRestore {
                                    snapshot_dir: entry.snapshot_dir.clone(),
                                },
                                max_idle_time: std::time::Duration::from_secs(300),
                                replenish_interval: std::time::Duration::from_secs(5),
                                vm_template: PoolVmTemplate {
                                    vcpus,
                                    memory_mib,
                                    kernel_path: PathBuf::new(),
                                    initrd_path: None,
                                    cmdline: cmdline.clone(),
                                },
                            };
                            let tap_for_factory = tap_name.clone();
                            let pool = VmPool::with_factory(pool_config, Box::new(move || {
                                match restore_snapshot(&snap_dir_for_factory, tap_for_factory.as_deref()) {
                                    Ok(vm) => Some(Box::new(vm) as Box<dyn std::any::Any + Send>),
                                    Err(e) => {
                                        tracing::warn!(error = %e, "L4 pool factory: snapshot restore failed");
                                        None
                                    }
                                }
                            }));
                            let meta = L4PoolMeta {
                                image_name: image_name.clone(),
                                image_digest: image_digest.clone(),
                                config_hash: config_hash.clone(),
                                vm_config_toml: vm_config_toml.clone(),
                                memory_mib,
                                vcpus,
                            };
                            *pool_guard = Some((pool, meta));
                            tracing::info!(
                                sandbox_id = %sandbox_id,
                                image = %image_name,
                                "L4 pre-warmed VM pool initialized from L3 HIT (target=2, max=4)"
                            );
                        }
                    }

                    vm
                }
                Err(e) => {
                    tracing::warn!(
                        sandbox_id = %sandbox_id,
                        error = %e,
                        "L3 snapshot restore failed, invalidating and falling back to cold boot"
                    );
                    if let Ok(mut cache) = snapshot_cache.lock() {
                        let _ = cache.invalidate(&cache_key);
                    }
                    // Fall back to cold boot (no snapshot save on fallback).
                    match cold_boot_vm(&vm_config_toml, &sandbox_id) {
                        Some(vm) => vm,
                        None => return,
                    }
                }
            }
        } else if has_snapshot_support {
            // ── L3 CACHE MISS: cold boot + boot-to-marker + save snapshot ──
            tracing::info!(
                sandbox_id = %sandbox_id,
                key = %cache_key,
                "L3 snapshot cache MISS, cold boot with snapshot save"
            );

            let config = match nova_vmm::config::VmConfig::from_toml(&vm_config_toml) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "failed to parse VM config");
                    return;
                }
            };

            let mut built_vm = match nova_vmm::builder::build_vm(&config) {
                Ok(vm) => vm,
                Err(e) => {
                    tracing::error!(error = %e, "failed to build VM");
                    return;
                }
            };

            // Boot to "NovaVM: container ready" marker for snapshot checkpoint.
            let boot_timeout = std::time::Duration::from_secs(120);
            match nova_vmm::exit_handler::run_vcpu_until_match(
                &built_vm.vcpus[0],
                &mut built_vm.mmio_bus,
                boot_timeout,
                "NovaVM: container ready",
            ) {
                Ok((boot_output, stop_reason, diag)) => {
                    let text = String::from_utf8_lossy(&boot_output);
                    tracing::info!(
                        sandbox_id = %sandbox_id,
                        stop_reason = ?stop_reason,
                        elapsed_ms = diag.elapsed.as_millis() as u64,
                        output_bytes = boot_output.len(),
                        "VM booted to 'container ready'"
                    );
                    if !text.is_empty() {
                        tracing::info!(sandbox_id = %sandbox_id, "boot output:\n{}", text);
                    }

                    // Save L3 snapshot at this checkpoint.
                    let snap_dir = snapshot_base_dir.join(
                        cache_key.replace(':', "_").replace('/', "_"),
                    );
                    let snap_config = VmSnapshotConfig {
                        vcpus: config.vcpus,
                        memory_mib: config.memory_mib as u32,
                        kernel_cmdline: config.kernel.cmdline.clone(),
                    };
                    match save_snapshot(&built_vm, &snap_dir, &snap_config) {
                        Ok(_files) => {
                            tracing::info!(
                                sandbox_id = %sandbox_id,
                                dir = %snap_dir.display(),
                                "L3 snapshot saved"
                            );
                            let entry = SnapshotEntry {
                                key: cache_key.clone(),
                                snapshot_dir: snap_dir.clone(),
                                config_hash: config_hash.clone(),
                                image_digest: image_digest.clone(),
                                created_at: std::time::SystemTime::now(),
                                valid: true,
                            };
                            if let Ok(mut cache) = snapshot_cache.lock() {
                                if let Err(e) = cache.insert(entry) {
                                    tracing::warn!(error = %e, "failed to insert snapshot into L3 cache");
                                }
                            }

                            // ── Lazy L4 pool init after first snapshot save ──
                            {
                                let mut pool_guard = vm_pool.lock().unwrap();
                                if pool_guard.is_none() {
                                    let snap_dir_for_factory = snap_dir.clone();
                                    let pool_config = PoolConfig {
                                        target_pool_size: 2,
                                        max_pool_size: 4,
                                        warm_strategy: WarmStrategy::SnapshotRestore {
                                            snapshot_dir: snap_dir.clone(),
                                        },
                                        max_idle_time: std::time::Duration::from_secs(300),
                                        replenish_interval: std::time::Duration::from_secs(5),
                                        vm_template: PoolVmTemplate {
                                            vcpus: config.vcpus,
                                            memory_mib: config.memory_mib as u32,
                                            kernel_path: PathBuf::new(),
                                            initrd_path: None,
                                            cmdline: config.kernel.cmdline.clone(),
                                        },
                                    };
                                    let tap_for_factory = tap_name.clone();
                                    let pool = VmPool::with_factory(pool_config, Box::new(move || {
                                        match restore_snapshot(&snap_dir_for_factory, tap_for_factory.as_deref()) {
                                            Ok(vm) => Some(Box::new(vm) as Box<dyn std::any::Any + Send>),
                                            Err(e) => {
                                                tracing::warn!(error = %e, "L4 pool factory: snapshot restore failed");
                                                None
                                            }
                                        }
                                    }));
                                    let meta = L4PoolMeta {
                                        image_name: image_name.clone(),
                                        image_digest: image_digest.clone(),
                                        config_hash: config_hash.clone(),
                                        vm_config_toml: vm_config_toml.clone(),
                                        memory_mib: config.memory_mib as u32,
                                        vcpus: config.vcpus,
                                    };
                                    *pool_guard = Some((pool, meta));
                                    tracing::info!(
                                        sandbox_id = %sandbox_id,
                                        image = %image_name,
                                        "L4 pre-warmed VM pool initialized (target=2, max=4)"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                sandbox_id = %sandbox_id,
                                error = %e,
                                "failed to save L3 snapshot"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        sandbox_id = %sandbox_id,
                        error = %e,
                        "boot-to-marker failed, VM may not have reached 'container ready'"
                    );
                }
            }

            built_vm
        } else {
            // ── No image digest: plain cold boot without snapshot ──
            tracing::info!(sandbox_id = %sandbox_id, "cold boot (no snapshot support)");
            match cold_boot_vm(&vm_config_toml, &sandbox_id) {
                Some(vm) => vm,
                None => return,
            }
        };

        tracing::info!(sandbox_id = %sandbox_id, "entering VM run loop");

        // Continue running the VM. Use interactive capture with serial input support.
        // Output is written to console_buf in real-time (not buffered).
        let timeout = std::time::Duration::from_secs(3600);
        let max_output = 256 * 1024; // 256KB
        let capture_result = nova_vmm::exit_handler::run_vcpu_resume_capture_interactive(
            &built_vm.vcpus[0],
            &mut built_vm.mmio_bus,
            timeout,
            max_output,
            Arc::clone(&serial_input),
            Arc::clone(&console_buf),
            Some(Arc::clone(&shutdown_flag)),
        );

        match capture_result {
            Ok((output, stop_reason, diag)) => {
                // Output already written to console_buf in real-time.
                let text = String::from_utf8_lossy(&output);
                tracing::info!(
                    sandbox_id = %sandbox_id,
                    stop_reason = ?stop_reason,
                    total_exits = diag.total_exits,
                    elapsed_ms = diag.elapsed.as_millis() as u64,
                    output_bytes = output.len(),
                    "VM stopped"
                );
                if !text.is_empty() {
                    tracing::info!(sandbox_id = %sandbox_id, "console output:\n{}", text);
                }
            }
            Err(e) => tracing::error!(sandbox_id = %sandbox_id, error = %e, "VM run error"),
        }

        tracing::info!(sandbox_id = %sandbox_id, "VM thread exiting");
    });

    (shutdown, handle, console_output, console_input)
}

/// Cold-boot a VM from TOML config. Returns None on error.
fn cold_boot_vm(
    vm_config_toml: &str,
    sandbox_id: &str,
) -> Option<nova_vmm::builder::BuiltVm> {
    let config = match nova_vmm::config::VmConfig::from_toml(vm_config_toml) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(sandbox_id = %sandbox_id, error = %e, "failed to parse VM config");
            return None;
        }
    };
    match nova_vmm::builder::build_vm(&config) {
        Ok(vm) => Some(vm),
        Err(e) => {
            tracing::error!(sandbox_id = %sandbox_id, error = %e, "failed to build VM");
            None
        }
    }
}

#[tonic::async_trait]
impl RuntimeService for RuntimeDaemonService {
    async fn create_sandbox(
        &self,
        request: Request<CreateSandboxRequest>,
    ) -> Result<Response<CreateSandboxResponse>, Status> {
        let req = request.into_inner();

        let sandbox_id = if req.sandbox_id.is_empty() {
            format!("nova-{:x}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() & 0xFFFF_FFFF)
        } else {
            req.sandbox_id.clone()
        };

        // Determine kernel path (used by both fast path and normal path).
        let kernel = self.kernel_path.clone()
            .unwrap_or_else(|| PathBuf::from("/boot/vmlinux"));

        // ── ADMISSION CONTROL ──
        {
            let config = req.config.as_ref();
            let vcpus = config.map(|c| c.vcpus).unwrap_or(1);
            let memory_mib = config.map(|c| c.memory_mib).unwrap_or(128);

            if let Ok(ps) = self.policy_state.lock() {
                if ps.admission_enabled {
                    let input = nova_policy::AdmissionInput {
                        sandbox_id: sandbox_id.clone(),
                        image: req.image.clone(),
                        vcpus,
                        memory_mib,
                        uid: 1000,
                    };
                    let result = ps.admission_checker.check(&input);
                    if !result.allowed {
                        return Err(Status::permission_denied(
                            format!("admission denied: {}", result.reason),
                        ));
                    }
                }
            }
        }

        // ── L4 FAST PATH: skip OCI pull if pool has a warm VM for this image ──
        if !req.image.is_empty() {
            let l4_fast_path = {
                let pool_guard = self.vm_pool.lock().unwrap();
                if let Some((ref pool, ref meta)) = *pool_guard {
                    meta.image_name == req.image && pool.stats().ready_count > 0
                } else {
                    false
                }
            };
            if l4_fast_path {
                let (image_digest, memory_mib, vcpus) = {
                    let pool_guard = self.vm_pool.lock().unwrap();
                    let meta = &pool_guard.as_ref().unwrap().1;
                    (meta.image_digest.clone(), meta.memory_mib, meta.vcpus)
                };
                tracing::info!(
                    sandbox_id = %sandbox_id,
                    image = %req.image,
                    "L4 fast path: skipping OCI pull, pool has warm VM"
                );
                self.image_digests.lock().await.insert(sandbox_id.clone(), image_digest);
                self.sandbox_commands.lock().await.insert(sandbox_id.clone(), req.command.clone());
                self.image_names.lock().await.insert(sandbox_id.clone(), req.image.clone());
                let config = RuntimeSandboxConfig {
                    vcpus,
                    memory_mib,
                    kernel: kernel.clone(),
                    rootfs: PathBuf::from("L4_POOL"),
                    cmdline: "console=ttyS0 reboot=k panic=1 random.trust_cpu=on tsc=reliable no_timer_check lpj=5000000 preempt=none nosmp noapictimer clk_ignore_unused".to_string(),
                    network: None,
                    kind: SandboxKind::Vm,
                };
                let mut orch = self.orchestrator.lock().await;
                orch.create(sandbox_id.clone(), config)
                    .map_err(|e| Status::already_exists(e.to_string()))?;
                tracing::info!(sandbox_id = %sandbox_id, "sandbox created (L4 fast path)");
                return Ok(Response::new(CreateSandboxResponse { sandbox_id }));
            }
        }

        let mut initrd_path: Option<PathBuf> = None;

        // If an image is specified, try to pull and convert to initramfs.
        if !req.image.is_empty() {
            // Check for OCI layout in fixtures.
            // Extract short name: "docker.io/library/nginx:alpine" → "nginx"
            let image_name = req.image.split(':').next().unwrap_or(&req.image);
            let short_name = image_name.rsplit('/').next().unwrap_or(image_name);
            let fixtures_dir = PathBuf::from("/mnt/c/Users/haris/Desktop/personal/sandbox-reseach/novavm/tests/fixtures");
            let oci_dir = fixtures_dir.join(format!("{}-oci", short_name));

            if oci_dir.exists() {
                tracing::info!(oci_dir = %oci_dir.display(), "found OCI layout, converting to initramfs");
                let mut puller = self.image_puller.lock().await;
                match puller.pull_oci_layout(&oci_dir) {
                    Ok(info) => {
                        tracing::info!(
                            digest = %info.digest,
                            rootfs = %info.rootfs_path.display(),
                            size = info.size_bytes,
                            "OCI image converted to initramfs"
                        );
                        // Store image digest, command, and image name for L3/L4 cache.
                        {
                            let mut digests = self.image_digests.lock().await;
                            digests.insert(sandbox_id.clone(), info.digest.clone());
                        }
                        {
                            let mut cmds = self.sandbox_commands.lock().await;
                            cmds.insert(sandbox_id.clone(), req.command.clone());
                        }
                        {
                            let mut names = self.image_names.lock().await;
                            names.insert(sandbox_id.clone(), req.image.clone());
                        }
                        // Build image-specific entry script based on command or image name.
                        let entry_script: Option<Vec<u8>> = if !req.command.is_empty() {
                            // User-specified command: wrap in a shell script (no exec, so init survives).
                            let script = format!("#!/bin/sh\n{}\n", req.command);
                            Some(script.into_bytes())
                        } else {
                            None // init will auto-detect (nginx, etc.)
                        };
                        // Inject /init script into the cpio for OCI images.
                        if let Err(e) = inject_init_into_cpio(
                            &info.rootfs_path,
                            entry_script.as_deref(),
                            self.kernel_path.as_deref(),
                        ) {
                            tracing::warn!(error = %e, "failed to inject /init into cpio");
                        }

                        // Inject guest eBPF agent + bytecode into the cpio if enabled.
                        if self.guest_sensor_config.enabled {
                            if let Err(e) = inject_guest_ebpf(
                                &info.rootfs_path,
                                &self.guest_sensor_config,
                                &self.sensor_config,
                            ) {
                                tracing::warn!(error = %e, "failed to inject guest eBPF agent");
                            }
                        }

                        initrd_path = Some(info.rootfs_path);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "OCI layout pull failed, falling back to stub image");
                        // Fall back to stub pull.
                        match puller.pull(&req.image) {
                            Ok(info) => {
                                tracing::info!(digest = %info.digest, "stub image pulled");
                            }
                            Err(e2) => {
                                tracing::warn!(error = %e2, "stub pull also failed");
                            }
                        }
                    }
                }
            } else {
                // No local OCI layout — pull from registry.
                let registry_client = crate::registry::RegistryClient::new();
                match crate::registry::ImageReference::parse(&req.image) {
                    Ok(image_ref) => {
                        let pull_oci_dir = self.image_dir.join(format!(
                            "oci-{}",
                            crate::registry::sanitize_image_ref(&req.image)
                        ));
                        tracing::info!(
                            image_ref = %req.image,
                            oci_dir = %pull_oci_dir.display(),
                            "pulling from registry"
                        );
                        match registry_client.pull(&image_ref, &pull_oci_dir, None).await {
                            Ok(reg_digest) => {
                                let mut puller = self.image_puller.lock().await;
                                match puller.pull_oci_layout(&pull_oci_dir) {
                                    Ok(info) => {
                                        tracing::info!(
                                            digest = %reg_digest,
                                            rootfs = %info.rootfs_path.display(),
                                            size = info.size_bytes,
                                            "registry image converted to initramfs"
                                        );
                                        {
                                            let mut digests = self.image_digests.lock().await;
                                            digests.insert(sandbox_id.clone(), reg_digest);
                                        }
                                        {
                                            let mut cmds = self.sandbox_commands.lock().await;
                                            cmds.insert(sandbox_id.clone(), req.command.clone());
                                        }
                                        {
                                            let mut names = self.image_names.lock().await;
                                            names.insert(sandbox_id.clone(), req.image.clone());
                                        }
                                        let entry_script: Option<Vec<u8>> = if !req.command.is_empty() {
                                            let script = format!("#!/bin/sh\n{}\n", req.command);
                                            Some(script.into_bytes())
                                        } else {
                                            None
                                        };
                                        if let Err(e) = inject_init_into_cpio(
                                            &info.rootfs_path,
                                            entry_script.as_deref(),
                                            self.kernel_path.as_deref(),
                                        ) {
                                            tracing::warn!(error = %e, "failed to inject /init into cpio");
                                        }
                                        if self.guest_sensor_config.enabled {
                                            if let Err(e) = inject_guest_ebpf(
                                                &info.rootfs_path,
                                                &self.guest_sensor_config,
                                                &self.sensor_config,
                                            ) {
                                                tracing::warn!(error = %e, "failed to inject guest eBPF agent");
                                            }
                                        }
                                        initrd_path = Some(info.rootfs_path);
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "OCI layout conversion failed after registry pull");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, "registry pull failed, falling back to stub");
                                let puller = self.image_puller.lock().await;
                                match puller.pull(&req.image) {
                                    Ok(info) => {
                                        tracing::info!(digest = %info.digest, "image pulled (stub)");
                                    }
                                    Err(e2) => {
                                        tracing::warn!(error = %e2, "stub pull also failed");
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "invalid image reference, using stub pull");
                        let puller = self.image_puller.lock().await;
                        match puller.pull(&req.image) {
                            Ok(info) => {
                                tracing::info!(digest = %info.digest, "image pulled (stub)");
                            }
                            Err(e2) => {
                                tracing::warn!(error = %e2, "stub pull also failed");
                            }
                        }
                    }
                }
            }
        }

        // Auto-scale memory based on initrd size: kernel needs enough RAM to
        // hold both the initrd in reserved memory AND extract all files into
        // the rootfs tmpfs, plus working memory.  Rule: max(256, initrd_mib * 3).
        let min_memory_mib = if let Some(ref ipath) = initrd_path {
            let initrd_bytes = std::fs::metadata(ipath).map(|m| m.len()).unwrap_or(0);
            let initrd_mib = (initrd_bytes / (1024 * 1024)) as u32 + 1;
            std::cmp::max(256, initrd_mib * 3)
        } else {
            128
        };

        // Build sandbox config.
        let config = if let Some(proto_cfg) = req.config {
            let network = proto_cfg.network.map(|n| {
                RuntimeNetworkConfig {
                    tap_device: n.tap_device,
                    guest_ip: n.guest_ip,
                    host_ip: n.host_ip,
                    mac_address: n.mac_address,
                }
            });
            // Use daemon's kernel_path as fallback when proto config kernel is empty.
            let kernel_for_config = if proto_cfg.kernel.is_empty() {
                kernel.clone()
            } else {
                PathBuf::from(proto_cfg.kernel)
            };
            let rootfs_for_config = if initrd_path.is_some() {
                initrd_path.clone().unwrap()
            } else if !proto_cfg.rootfs.is_empty() {
                PathBuf::from(proto_cfg.rootfs)
            } else {
                PathBuf::from("/var/lib/nova/rootfs.ext4")
            };
            let cmdline = if proto_cfg.cmdline.is_empty() {
                "console=ttyS0 reboot=k panic=1 random.trust_cpu=on tsc=reliable no_timer_check lpj=5000000 preempt=none nosmp noapictimer clk_ignore_unused".to_string()
            } else {
                proto_cfg.cmdline
            };
            let requested_mem = if proto_cfg.memory_mib > 0 { proto_cfg.memory_mib } else { 512 };
            RuntimeSandboxConfig {
                vcpus: if proto_cfg.vcpus > 0 { proto_cfg.vcpus } else { 1 },
                memory_mib: std::cmp::max(requested_mem, min_memory_mib),
                kernel: kernel_for_config,
                rootfs: rootfs_for_config,
                cmdline,
                network,
                kind: SandboxKind::Vm,
            }
        } else {
            RuntimeSandboxConfig {
                vcpus: 1,
                memory_mib: min_memory_mib,
                kernel: kernel.clone(),
                rootfs: initrd_path.clone().unwrap_or_else(|| PathBuf::from("/var/lib/nova/rootfs.ext4")),
                cmdline: "console=ttyS0 reboot=k panic=1 random.trust_cpu=on tsc=reliable no_timer_check lpj=5000000 preempt=none nosmp noapictimer clk_ignore_unused".to_string(),
                network: None,
                kind: SandboxKind::Vm,
            }
        };

        let mut orch = self.orchestrator.lock().await;
        orch.create(sandbox_id.clone(), config)
            .map_err(|e| Status::already_exists(e.to_string()))?;

        tracing::info!(sandbox_id = %sandbox_id, "sandbox created");

        Ok(Response::new(CreateSandboxResponse { sandbox_id }))
    }

    async fn start_sandbox(
        &self,
        request: Request<StartSandboxRequest>,
    ) -> Result<Response<StartSandboxResponse>, Status> {
        let req = request.into_inner();

        // Start the sandbox in the orchestrator (sets state to Running).
        let (sandbox_config, has_kernel) = {
            let mut orch = self.orchestrator.lock().await;
            let sandbox = orch.get(&req.sandbox_id)
                .map_err(|e| Status::not_found(e.to_string()))?;
            let cfg = sandbox.config().clone();
            let kernel_path_set = self.kernel_path.is_some();
            let kernel_exists = cfg.kernel.exists();
            let has_kernel = kernel_path_set && kernel_exists;
            tracing::info!(
                kernel_path_set,
                kernel_exists,
                has_kernel,
                kernel = %cfg.kernel.display(),
                rootfs = %cfg.rootfs.display(),
                "start_sandbox kernel check"
            );
            drop(sandbox);

            orch.start(&req.sandbox_id)
                .map_err(|e| match &e {
                    nova_runtime::RuntimeError::SandboxNotFound(_) => Status::not_found(e.to_string()),
                    nova_runtime::RuntimeError::InvalidState { .. } => Status::failed_precondition(e.to_string()),
                    _ => Status::internal(e.to_string()),
                })?;

            (cfg, has_kernel)
        };

        // If we have a real kernel, boot the VM in a background thread.
        if has_kernel {
            let initrd = if sandbox_config.rootfs.exists()
                && sandbox_config.rootfs.to_string_lossy().ends_with(".cpio")
            {
                Some(sandbox_config.rootfs.as_path())
            } else {
                None
            };

            // Use TAP device from config (falls back to NOVA_TAP env for compat).
            let tap_device = self.tap_device.clone()
                .or_else(|| std::env::var("NOVA_TAP").ok());

            let vm_toml = build_vm_config(
                &sandbox_config.kernel,
                initrd,
                sandbox_config.memory_mib,
                sandbox_config.vcpus,
                &sandbox_config.cmdline,
                tap_device.as_deref(),
            );

            // Retrieve image digest and command for L3 snapshot cache key.
            let image_digest = {
                let digests = self.image_digests.lock().await;
                digests.get(&req.sandbox_id).cloned().unwrap_or_default()
            };
            let command = {
                let cmds = self.sandbox_commands.lock().await;
                cmds.get(&req.sandbox_id).cloned().unwrap_or_default()
            };
            let image_name = {
                let names = self.image_names.lock().await;
                names.get(&req.sandbox_id).cloned().unwrap_or_default()
            };
            let snapshot_base_dir = self.image_dir.join("snapshots");

            tracing::info!(
                sandbox_id = %req.sandbox_id,
                kernel = %sandbox_config.kernel.display(),
                initrd = ?initrd.map(|p| p.display().to_string()),
                image_digest = %image_digest,
                "booting real KVM VM"
            );

            let (shutdown, join_handle, console_output, console_input) = spawn_vm_thread(
                vm_toml,
                req.sandbox_id.clone(),
                Arc::clone(&self.snapshot_cache),
                image_digest,
                command,
                image_name,
                snapshot_base_dir,
                Arc::clone(&self.vm_pool),
            );

            let mut handles = self.vm_handles.lock().await;
            handles.insert(req.sandbox_id.clone(), VmHandle {
                shutdown,
                join_handle: Some(join_handle),
                console_output,
                console_input,
            });

            // Register a GuestEventSource for this VM if guest eBPF is enabled.
            if self.guest_sensor_config.enabled {
                let bind_addr = format!("0.0.0.0:{}", self.guest_sensor_config.event_port);
                match nova_eye::GuestEventSource::new(&bind_addr, &req.sandbox_id) {
                    Ok(src) => {
                        let _ = self.source_tx.send(Box::new(src));
                        tracing::info!(
                            sandbox_id = %req.sandbox_id,
                            bind = %bind_addr,
                            "registered GuestEventSource for VM"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            sandbox_id = %req.sandbox_id,
                            error = %e,
                            "failed to create GuestEventSource"
                        );
                    }
                }
            }
        }

        tracing::info!(sandbox_id = %req.sandbox_id, "sandbox started");

        Ok(Response::new(StartSandboxResponse {}))
    }

    async fn stop_sandbox(
        &self,
        request: Request<StopSandboxRequest>,
    ) -> Result<Response<StopSandboxResponse>, Status> {
        let req = request.into_inner();

        // Signal VM thread to shut down and wait for it without blocking the async runtime.
        {
            let mut handles = self.vm_handles.lock().await;
            if let Some(handle) = handles.remove(&req.sandbox_id) {
                // Set shutdown flag — the vCPU loop checks this every ~10ms.
                handle.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                tracing::info!(sandbox_id = %req.sandbox_id, "shutdown flag set, waiting for VM thread");

                if let Some(jh) = handle.join_handle {
                    // Join on a blocking thread pool to avoid blocking the tokio runtime.
                    let join_result = tokio::task::spawn_blocking(move || {
                        jh.join()
                    });
                    // Wait up to 5 seconds for the VM thread to exit.
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        join_result,
                    ).await {
                        Ok(Ok(Ok(()))) => {
                            tracing::info!(sandbox_id = %req.sandbox_id, "VM thread joined cleanly");
                        }
                        Ok(Ok(Err(_))) => {
                            tracing::warn!(sandbox_id = %req.sandbox_id, "VM thread panicked");
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(sandbox_id = %req.sandbox_id, error = %e, "spawn_blocking join error");
                        }
                        Err(_) => {
                            tracing::warn!(sandbox_id = %req.sandbox_id, "VM thread join timed out after 5s, proceeding");
                        }
                    }
                }
                tracing::info!(sandbox_id = %req.sandbox_id, "VM thread stopped");
            }
        }

        let mut orch = self.orchestrator.lock().await;
        orch.stop(&req.sandbox_id)
            .map_err(|e| match &e {
                nova_runtime::RuntimeError::SandboxNotFound(_) => Status::not_found(e.to_string()),
                nova_runtime::RuntimeError::InvalidState { .. } => Status::failed_precondition(e.to_string()),
                _ => Status::internal(e.to_string()),
            })?;

        tracing::info!(sandbox_id = %req.sandbox_id, "sandbox stopped");

        Ok(Response::new(StopSandboxResponse {}))
    }

    async fn destroy_sandbox(
        &self,
        request: Request<DestroySandboxRequest>,
    ) -> Result<Response<DestroySandboxResponse>, Status> {
        let req = request.into_inner();

        let mut orch = self.orchestrator.lock().await;
        orch.destroy(&req.sandbox_id)
            .map_err(|e| match &e {
                nova_runtime::RuntimeError::SandboxNotFound(_) => Status::not_found(e.to_string()),
                nova_runtime::RuntimeError::InvalidState { .. } => Status::failed_precondition(e.to_string()),
                _ => Status::internal(e.to_string()),
            })?;

        tracing::info!(sandbox_id = %req.sandbox_id, "sandbox destroyed");

        Ok(Response::new(DestroySandboxResponse {}))
    }

    async fn sandbox_status(
        &self,
        request: Request<SandboxStatusRequest>,
    ) -> Result<Response<SandboxStatusResponse>, Status> {
        let req = request.into_inner();
        let orch = self.orchestrator.lock().await;
        let sandbox = orch.get(&req.sandbox_id)
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(SandboxStatusResponse {
            sandbox_id: sandbox.id().to_string(),
            state: state_to_proto(sandbox.state()),
            pid: sandbox.pid().map(|p| p as i32).unwrap_or(0),
            created_at: system_time_to_rfc3339(sandbox.created_at()),
        }))
    }

    async fn list_sandboxes(
        &self,
        _request: Request<ListSandboxesRequest>,
    ) -> Result<Response<ListSandboxesResponse>, Status> {
        let orch = self.orchestrator.lock().await;
        let sandboxes: Vec<SandboxStatusResponse> = orch
            .list()
            .iter()
            .map(|sb| SandboxStatusResponse {
                sandbox_id: sb.id().to_string(),
                state: state_to_proto(sb.state()),
                pid: sb.pid().map(|p| p as i32).unwrap_or(0),
                created_at: system_time_to_rfc3339(sb.created_at()),
            })
            .collect();

        Ok(Response::new(ListSandboxesResponse { sandboxes }))
    }

    async fn exec_in_sandbox(
        &self,
        request: Request<ExecInSandboxRequest>,
    ) -> Result<Response<ExecInSandboxResponse>, Status> {
        let req = request.into_inner();

        // Verify sandbox is running.
        {
            let orch = self.orchestrator.lock().await;
            let sandbox = orch.get(&req.sandbox_id)
                .map_err(|e| Status::not_found(e.to_string()))?;
            if sandbox.state() != RuntimeSandboxState::Running {
                return Err(Status::failed_precondition(format!(
                    "sandbox '{}' is not running (state: {})",
                    req.sandbox_id, sandbox.state()
                )));
            }
        }

        let cmd_str = req.command.join(" ");
        tracing::info!(sandbox_id = %req.sandbox_id, command = %cmd_str, "exec in sandbox");

        // Get console handles from VmHandle.
        let (console_input, console_output) = {
            let handles = self.vm_handles.lock().await;
            match handles.get(&req.sandbox_id) {
                Some(handle) => (
                    Arc::clone(&handle.console_input),
                    Arc::clone(&handle.console_output),
                ),
                None => {
                    return Err(Status::not_found(format!(
                        "no VM handle for sandbox '{}'",
                        req.sandbox_id
                    )));
                }
            }
        };

        // Record current console output length as baseline.
        let baseline = {
            let buf = console_output.lock().unwrap();
            buf.len()
        };

        // Send command to guest via serial input (with newline).
        {
            let mut input = console_input.lock().unwrap();
            let cmd_with_newline = format!("{}\n", cmd_str);
            input.extend(cmd_with_newline.as_bytes());
        }

        // Wait for output markers (up to 30 seconds).
        let begin_marker = "===NOVA_EXEC_BEGIN===";
        let end_marker = "===NOVA_EXEC_END===";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

        let mut stdout_result = Vec::new();
        let mut exit_code = 0i32;

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

            // Look for end marker (which means command completed).
            if let Some(end_pos) = output_text.find(end_marker) {
                // Extract output between begin and end markers.
                if let Some(begin_pos) = output_text.find(begin_marker) {
                    let content_start = begin_pos + begin_marker.len();
                    let content = &output_text[content_start..end_pos];
                    // Trim leading/trailing newlines.
                    let trimmed = content.trim_matches('\n').trim_matches('\r');
                    stdout_result = trimmed.as_bytes().to_vec();
                }

                // Parse exit code from end marker (===NOVA_EXEC_END===<code>).
                let after_end = &output_text[end_pos + end_marker.len()..];
                let code_str = after_end.trim();
                exit_code = code_str.parse::<i32>().unwrap_or(0);
                break;
            }

            if std::time::Instant::now() >= deadline {
                // Timeout — return whatever we have.
                let buf = console_output.lock().unwrap();
                if buf.len() > baseline {
                    stdout_result = buf.iter().skip(baseline).copied().collect();
                }
                exit_code = -1;
                break;
            }
        }

        Ok(Response::new(ExecInSandboxResponse {
            exit_code,
            stdout: stdout_result,
            stderr: Vec::new(),
        }))
    }

    type StreamConsoleStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ConsoleOutput, Status>> + Send>,
    >;

    async fn stream_console(
        &self,
        request: Request<StreamConsoleRequest>,
    ) -> Result<Response<Self::StreamConsoleStream>, Status> {
        let req = request.into_inner();

        // Verify sandbox exists and is running.
        {
            let orch = self.orchestrator.lock().await;
            let sandbox = orch.get(&req.sandbox_id)
                .map_err(|e| Status::not_found(e.to_string()))?;
            if sandbox.state() != RuntimeSandboxState::Running {
                return Err(Status::failed_precondition(format!(
                    "sandbox '{}' is not running",
                    req.sandbox_id
                )));
            }
        }

        // Get console_output from VmHandle.
        let console_buf = {
            let handles = self.vm_handles.lock().await;
            match handles.get(&req.sandbox_id) {
                Some(handle) => Arc::clone(&handle.console_output),
                None => {
                    return Err(Status::not_found(format!(
                        "no VM handle for sandbox '{}'",
                        req.sandbox_id
                    )));
                }
            }
        };

        let orchestrator = Arc::clone(&self.orchestrator);
        let sandbox_id = req.sandbox_id.clone();

        // Poll loop: drain console_output every 100ms, send as ConsoleOutput chunks.
        let stream = async_stream::try_stream! {
            let mut cursor = 0usize;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // Check if sandbox is still running.
                let still_running = {
                    let orch = orchestrator.lock().await;
                    match orch.get(&sandbox_id) {
                        Ok(sb) => sb.state() == RuntimeSandboxState::Running,
                        Err(_) => false,
                    }
                };

                // Drain any new output.
                let data = {
                    let buf = console_buf.lock().unwrap();
                    if buf.len() > cursor {
                        let new_data: Vec<u8> = buf.iter().skip(cursor).copied().collect();
                        cursor = buf.len();
                        Some(new_data)
                    } else {
                        None
                    }
                };

                if let Some(data) = data {
                    if !data.is_empty() {
                        yield ConsoleOutput { data };
                    }
                }

                if !still_running {
                    // Drain final output.
                    let final_data = {
                        let buf = console_buf.lock().unwrap();
                        if buf.len() > cursor {
                            let d: Vec<u8> = buf.iter().skip(cursor).copied().collect();
                            Some(d)
                        } else {
                            None
                        }
                    };
                    if let Some(data) = final_data {
                        if !data.is_empty() {
                            yield ConsoleOutput { data };
                        }
                    }
                    break;
                }
            }
        };

        Ok(Response::new(Box::pin(stream) as Self::StreamConsoleStream))
    }

    async fn send_console_input(
        &self,
        request: Request<ConsoleInputRequest>,
    ) -> Result<Response<ConsoleInputResponse>, Status> {
        let req = request.into_inner();

        // Verify sandbox is running.
        {
            let orch = self.orchestrator.lock().await;
            let sandbox = orch.get(&req.sandbox_id)
                .map_err(|e| Status::not_found(e.to_string()))?;
            if sandbox.state() != RuntimeSandboxState::Running {
                return Err(Status::failed_precondition(format!(
                    "sandbox '{}' is not running",
                    req.sandbox_id
                )));
            }
        }

        // Get console_input from VmHandle.
        let console_input = {
            let handles = self.vm_handles.lock().await;
            match handles.get(&req.sandbox_id) {
                Some(handle) => Arc::clone(&handle.console_input),
                None => {
                    return Err(Status::not_found(format!(
                        "no VM handle for sandbox '{}'",
                        req.sandbox_id
                    )));
                }
            }
        };

        let bytes_written = req.data.len() as u32;
        {
            let mut input = console_input.lock().unwrap();
            input.extend(&req.data);
        }

        Ok(Response::new(ConsoleInputResponse { bytes_written }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_to_proto() {
        assert_eq!(state_to_proto(RuntimeSandboxState::Created), SandboxState::Created as i32);
        assert_eq!(state_to_proto(RuntimeSandboxState::Running), SandboxState::Running as i32);
        assert_eq!(state_to_proto(RuntimeSandboxState::Stopped), SandboxState::Stopped as i32);
        assert_eq!(state_to_proto(RuntimeSandboxState::Error), SandboxState::Error as i32);
    }

    #[test]
    fn test_system_time_to_rfc3339() {
        let epoch = std::time::UNIX_EPOCH;
        assert_eq!(system_time_to_rfc3339(epoch), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn test_days_to_ymd() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        assert_eq!(days_to_ymd(365), (1971, 1, 1));
    }

    #[test]
    fn test_is_leap_year() {
        assert!(!is_leap_year(1970));
        assert!(is_leap_year(2000));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(1900));
    }
}
