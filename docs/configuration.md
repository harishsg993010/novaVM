# Configuration

NovaVM is configured via a single TOML file. No environment variables are used.

**Default path:** `/etc/nova/nova.toml`
**Override:** `nova serve --config /path/to/nova.toml`

## Full Reference

```toml
# ============================================================
# Daemon Settings
# ============================================================
[daemon]

# gRPC Unix domain socket path
socket = "/run/nova/nova.sock"

# Directory for OCI image cache (L1 blobs, L2 rootfs)
image_dir = "/var/lib/nova/images"

# Path to guest Linux kernel (required for KVM sandboxes)
kernel = "/opt/nova/vmlinux"

# REST API port (HTTP/JSON)
api_port = 9800

# TAP device name for guest networking (optional)
# Requires prior setup: sudo bash scripts/setup-network.sh
# tap_device = "tap0"

# ============================================================
# Sensor / eBPF Observability
# ============================================================
[sensor]

# JSONL event audit log path
events_log = "/var/run/nova/events.jsonl"

# Directory containing compiled eBPF ELF bytecode
ebpf_dir = "/opt/nova/ebpf"

# Guest eBPF agent configuration
[sensor.guest]
enabled = true                              # Inject agent into guest initrd
agent_path = "/opt/nova/bin/nova-eye-agent" # Path to musl-static agent binary
event_port = 9876                           # UDP port for guest->host events

# eBPF probe definitions (array of tables)
# Each probe attaches one eBPF program to a kernel/user hook

[[sensor.probes]]
name = "process_exec"
hook_type = "tracepoint"                    # tracepoint | kprobe | uprobe
target = "sched/sched_process_exec"         # hook target
bytecode = "nova-eye-process"               # filename in ebpf_dir
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

[[sensor.probes]]
name = "http_write"
hook_type = "uprobe"
target = "SSL_write"
bytecode = "nova-eye-http"
binary = "/usr/lib/libssl.so"               # required for uprobes
enabled = true

[[sensor.probes]]
name = "http_read"
hook_type = "uprobe"
target = "SSL_read"
bytecode = "nova-eye-http-read"
binary = "/usr/lib/libssl.so"
enabled = true

# ============================================================
# OPA Policy Engine
# ============================================================
[policy]

# Enable admission control (checked on sandbox creation)
admission_enabled = true

# Enable runtime enforcement (checked on every eBPF event)
enforcement_enabled = true

# Admission limits
max_vcpus = 8                               # Max vCPUs per sandbox
max_memory_mib = 8192                       # Max memory (MiB) per sandbox
max_sandboxes = 100                         # Max concurrent sandboxes

# Image allowlist (empty = allow all)
allowed_images = []
# allowed_images = ["nginx:alpine", "python:3.11-slim"]

# Directory for OPA Wasm policy bundles
bundle_dir = "/var/lib/nova/policy/bundles"

# Builtin enforcement ruleset: "default", "strict", or "none"
enforcement_rules = "default"

# Custom enforcement rules (appended after builtin ruleset)
# [[policy.rules]]
# event_type = "process_exec"
# action = "alert"                          # allow | alert | deny | kill
```

## Minimal Config

The simplest config to get started (no eBPF, no policy, no networking):

```toml
[daemon]
socket = "/run/nova/nova.sock"
image_dir = "/var/lib/nova/images"
kernel = "/opt/nova/vmlinux"
api_port = 9800

[sensor]
events_log = "/var/run/nova/events.jsonl"

[policy]
admission_enabled = false
enforcement_enabled = false
```

## Config Sections Explained

### `[daemon]`

| Field | Required | Default | Description |
|---|---|---|---|
| `socket` | No | `/run/nova/nova.sock` | gRPC Unix socket path |
| `image_dir` | No | `/var/lib/nova/images` | OCI cache directory |
| `kernel` | **Yes** | — | Guest kernel path |
| `api_port` | No | `9800` | REST API port |
| `tap_device` | No | — | TAP device for networking |

### `[sensor]`

| Field | Required | Default | Description |
|---|---|---|---|
| `events_log` | No | `/var/run/nova/events.jsonl` | JSONL audit log |
| `ebpf_dir` | No | `/opt/nova/ebpf` | eBPF bytecode directory |

### `[sensor.guest]`

| Field | Required | Default | Description |
|---|---|---|---|
| `enabled` | No | `false` | Inject agent into guest |
| `agent_path` | No | — | Path to agent binary |
| `event_port` | No | `9876` | UDP port for events |

### `[[sensor.probes]]`

| Field | Required | Description |
|---|---|---|
| `name` | No | Human-readable probe name |
| `hook_type` | **Yes** | `tracepoint`, `kprobe`, or `uprobe` |
| `target` | **Yes** | Hook target (e.g., `vfs_open`) |
| `bytecode` | **Yes** | eBPF binary name in `ebpf_dir` |
| `binary` | For uprobes | Path to userspace binary to probe |
| `enabled` | No | `true` by default |

### `[policy]`

| Field | Required | Default | Description |
|---|---|---|---|
| `admission_enabled` | No | `false` | Check policy on sandbox create |
| `enforcement_enabled` | No | `false` | Enforce policy on events |
| `max_vcpus` | No | `8` | Max vCPUs per sandbox |
| `max_memory_mib` | No | `8192` | Max memory per sandbox |
| `max_sandboxes` | No | `100` | Max concurrent sandboxes |
| `allowed_images` | No | `[]` | Image allowlist (empty = all) |
| `bundle_dir` | No | `/var/lib/nova/policy/bundles` | Wasm bundle directory |
| `enforcement_rules` | No | `"default"` | Builtin ruleset |
