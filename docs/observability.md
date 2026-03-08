# Observability

NovaVM provides real-time eBPF observability for both host and guest kernels, inspired by [Cilium Tetragon](https://github.com/cilium/tetragon).

## Architecture

```
Host Kernel                         Guest Kernel
    |                                   |
    v                                   v
kprobe: vfs_open                   kprobe: vfs_open
kprobe: tcp_v4_connect             tp: sched_process_exec
tp: sched_process_exec             uprobe: SSL_read
uprobe: SSL_write                       |
    |                                   v
    v                              nova-eye-agent
PerfEventArray                     (runs inside VM)
    |                                   |
    v                              UDP packets
AyaBpfSource                            |
    |                                   v
    +<------------------------ GuestEventSource
    |
    v
SensorPipeline
    |
    +-- Filter (by event type)
    +-- OPA enforcement (allow/alert/deny/kill)
    |
    +-- JsondSink -> /var/run/nova/events.jsonl
    +-- ChannelSink -> gRPC StreamEvents
```

## Event Types

| Event | Hook | What's Captured |
|---|---|---|
| `process_exec` | `sched/sched_process_exec` | PID, comm, filename |
| `file_open` | `vfs_open` | PID, comm, filename, flags |
| `net_connect` | `tcp_v4_connect` | PID, comm, dest IP, dest port |
| `http_write` | `SSL_write` uprobe | PID, comm, data length |
| `http_read` | `SSL_read` uprobe | PID, comm, data length |

## Setup

### 1. Build eBPF Programs

```bash
cd crates/nova-eye-ebpf
cargo +nightly build -Z build-std=core --target bpfel-unknown-none --release

# Install bytecode
sudo mkdir -p /opt/nova/ebpf
sudo cp target/bpfel-unknown-none/release/nova-eye-* /opt/nova/ebpf/
```

### 2. Configure Probes

In `/etc/nova/nova.toml`:

```toml
[sensor]
events_log = "/var/run/nova/events.jsonl"
ebpf_dir = "/opt/nova/ebpf"

[[sensor.probes]]
name = "process_exec"
hook_type = "tracepoint"
target = "sched/sched_process_exec"
bytecode = "nova-eye-process"
enabled = true

[[sensor.probes]]
name = "file_open"
hook_type = "kprobe"
target = "vfs_open"
bytecode = "nova-eye-file"
enabled = true

[[sensor.probes]]
name = "net_connect"
hook_type = "kprobe"
target = "tcp_v4_connect"
bytecode = "nova-eye-network"
enabled = true
```

### 3. Start Daemon

```bash
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml
```

The daemon loads eBPF programs on startup and begins capturing events immediately.

### 4. View Events

```bash
# Tail the JSONL log
tail -f /var/run/nova/events.jsonl | python -m json.tool
```

## JSONL Event Format

Each line in `events.jsonl` is a JSON object:

```json
{
  "event_type": "process_exec",
  "timestamp_ns": 1709827200000000,
  "pid": 1234,
  "tid": 1234,
  "uid": 0,
  "gid": 0,
  "comm": "nginx",
  "raw_len": 52,
  "sandbox_id": "web-server"
}
```

**Host events** have no `sandbox_id` field. **Guest events** include `sandbox_id` identifying which VM they came from.

## Guest eBPF

NovaVM can inject an eBPF agent into the guest VM for guest-side telemetry.

### Setup

```bash
# Build guest agent (static musl binary)
cd crates/nova-eye-agent
cargo build --target x86_64-unknown-linux-musl --release

# Install
sudo mkdir -p /opt/nova/bin
sudo cp target/x86_64-unknown-linux-musl/release/nova-eye-agent /opt/nova/bin/
```

Configure in `nova.toml`:

```toml
[sensor.guest]
enabled = true
agent_path = "/opt/nova/bin/nova-eye-agent"
event_port = 9876
```

### How It Works

1. During initramfs build, the agent binary and eBPF bytecode are injected into the guest's cpio archive
2. The guest init script starts the agent after boot
3. The agent loads eBPF probes inside the guest kernel
4. Events are sent via UDP over virtio-net to the host (port 9876)
5. `GuestEventSource` on the host receives and feeds them into the pipeline
6. Events are tagged with `sandbox_id` in the JSONL output

### Guest Agent Heartbeat

The agent sends periodic heartbeats:
```
NOVA-EYE-HB:polls=1000,events=42,errors=0
```

### eBPF Kernel Requirements

The guest kernel must support eBPF. NovaVM ships a patched 5.10.231 kernel with:
- 10 binary patches (NOP ktime_get WARN_ON, RET trace_event_eval_update, RET ftrace_free_init_mem)
- Cmdline: `clk_ignore_unused` (prevents UART clock gating)
- Disabled: NFS, SUNRPC, ZBUD, ZSWAP, ZPOOL, BPFILTER, VIRTIO_CONSOLE

## gRPC Streaming

Stream events via gRPC (useful for building dashboards):

```bash
# Using grpcurl:
grpcurl -plaintext -d '{}' -unix /run/nova/nova.sock \
  nova.sensor.SensorService/StreamEvents
```

The `SensorService` also exposes:
- `GetStatus` — loaded programs, total/dropped event counts
- `LoadProgram` / `UnloadProgram` — dynamic probe management

## Performance Impact

During benchmarks, the eBPF pipeline captured **74,000 events** with **zero measurable impact** on boot times. The pipeline uses lock-free channels and non-blocking I/O throughout.
