# Architecture

## Overview

NovaVM is a Rust workspace with 15 crates organized in layers. It ships as a single `nova` binary.

```
                    nova (unified binary)
                     |            |
                  serve          CLI
                     |            |
                    nova-api (daemon + REST + gRPC)
                   /    |    \      \
           nova-runtime |  nova-eye  nova-policy
              /    \    |     |          |
        nova-vmm nova-wasm  aya      OPA engine
         /   |   \
  nova-kvm  nova-boot  nova-virtio
        \    |    /
        nova-mem
```

## Crate Map

| Crate | Purpose | Key Types |
|---|---|---|
| **nova-kvm** | KVM ioctl wrappers | `Vm`, `Vcpu`, `KvmFd` |
| **nova-mem** | Guest memory (mmap) | `GuestMemoryMmap`, `GuestAddress` |
| **nova-boot** | Linux boot protocol | `load_kernel()`, `load_initrd()`, `setup_boot_params()` |
| **nova-virtio** | Virtio MMIO devices | `MmioTransport`, `Net`, `Queue` |
| **nova-vmm** | VM manager | `MicroVm`, `MmioBus`, `snapshot` |
| **nova-runtime** | Sandbox orchestrator | `SandboxRuntime`, `ImageCache`, `VmPool` |
| **nova-eye-common** | Shared event structs | `EventHeader`, `ProcessEvent` (no_std) |
| **nova-eye** | eBPF sensor subsystem | `SensorPipeline`, `AyaBpfSource`, `JsondSink` |
| **nova-eye-ebpf** | eBPF kernel programs | 5 BPF binaries (excluded from workspace) |
| **nova-eye-agent** | Guest eBPF agent | Runs inside VM (excluded, musl target) |
| **nova-api** | Daemon + REST + gRPC | `RuntimeDaemon`, server, registry client |
| **nova-policy** | OPA policy engine | `PolicyEngine`, `BundleManager` |
| **nova-wasm** | Wasmtime executor | `WasmSandbox` |
| **novactl** | Unified binary | `nova` binary (daemon + CLI + embedded assets) |

## Data Flow

### Sandbox Lifecycle

```
nova run nginx:alpine
    |
    v
[1] POST /api/v1/sandboxes (REST) or CreateSandbox RPC (gRPC)
    +-- Admission policy check (OPA)
    +-- Pull image (registry.rs -> Docker Hub)
    +-- L1 cache: store layer blobs (SHA-256 verified)
    +-- L2 cache: build rootfs from layers
    |
[2] Boot VM
    +-- Check L3 cache for snapshot
    |   +-- HIT:  mmap(MAP_PRIVATE) snapshot + force-activate virtio
    |   +-- MISS: build initramfs (cpio) + inject guest agent
    |             boot KVM VM (nova-vmm)
    |             save L3 snapshot after boot
    |
[3] VM Running
    +-- Virtio-net: TAP device for networking
    +-- Serial console: exec commands
    +-- eBPF pipeline: host probes + guest events (UDP)
    +-- OPA enforcement: per-event policy checks
    +-- JSONL sink: audit log to events.jsonl
```

### eBPF Event Pipeline

```
Host kernel                    Guest kernel
    |                              |
    v                              v
eBPF probes (kprobe/tp)      eBPF probes (kprobe/tp)
    |                              |
    v                              v
PerfEventArray                PerfEventArray
    |                              |
    v                              v
AyaBpfSource                  nova-eye-agent
    |                              |
    v                              v
SensorPipeline <-- UDP ---- GuestEventSource
    |
    +-- FilterStage (event type filtering)
    |
    +-- OPA enforcement (allow/alert/deny/kill)
    |
    +-- JsondSink (events.jsonl)
    +-- ChannelSink (gRPC StreamEvents)
```

### 4-Level Cache

```
L1: Blob Store
    Content-addressable OCI layer blobs
    Key: SHA-256 digest
    Storage: /var/lib/nova/images/blobs/sha256/

L2: Rootfs Cache
    Pre-built rootfs directories from OCI layers
    Key: image digest
    Storage: /var/lib/nova/images/rootfs/<digest>/
    Clone: copy (not hardlink — avoids inode corruption)

L3: VM Snapshot
    Full VM state: registers, memory, virtio queues
    Restore: mmap(MAP_PRIVATE) for demand-paged memory
    Storage: /var/lib/nova/images/snapshots/<id>/
    Contains: snapshot.json + memory.bin

L4: VM Pool
    Pre-warmed VM instances (type-erased factory)
    Ready to assign to new sandboxes instantly
    In-memory only (not persisted)
```

## KVM VM Layout

```
Guest Physical Memory:
    0x0000_0000 - 0x0000_0FFF   Real mode IDT/GDT
    0x0000_7000 - 0x0000_7FFF   Zero page (boot params)
    0x0001_0000 - 0x0001_FFFF   Kernel command line
    0x0020_0000 - ...           Kernel image (bzImage)
    ...         - RAM_TOP       Initramfs (cpio)

MMIO Devices (above RAM):
    0x_D000_0000 + 0*0x1000     virtio-net  (IRQ 5)
    0x_D000_0000 + 1*0x1000     virtio-blk  (IRQ 6)  [if present]

Serial I/O Ports:
    0x3F8                       COM1 (console + exec)
```

## APIs

### REST API (port 9800)

| Method | Path | Description |
|---|---|---|
| `GET` | `/healthz` | Health check |
| `GET` | `/api/v1/sandboxes` | List sandboxes |
| `POST` | `/api/v1/sandboxes` | Create sandbox |
| `GET` | `/api/v1/sandboxes/:id` | Get sandbox status |
| `POST` | `/api/v1/sandboxes/:id/exec` | Execute command |
| `POST` | `/api/v1/sandboxes/:id/stop` | Stop sandbox |
| `DELETE` | `/api/v1/sandboxes/:id` | Destroy sandbox |

### gRPC Services (Unix socket)

Four services on a single Unix domain socket:

| Service | Proto | RPCs |
|---|---|---|
| **RuntimeService** | `runtime.proto` | CreateSandbox, StartSandbox, StopSandbox, DestroySandbox, SandboxStatus, ListSandboxes, ExecInSandbox, StreamConsole, SendConsoleInput |
| **SandboxImageService** | `sandbox.proto` | PullImage, ListImages, RemoveImage, InspectImage |
| **PolicyService** | `policy.proto` | Evaluate, LoadBundle, ListBundles, RemoveBundle, GetStatus |
| **SensorService** | `sensor.proto` | StreamEvents, GetStatus, LoadProgram, UnloadProgram |

## Embedded Assets

When built with `./scripts/package-assets.sh`, the `nova` binary embeds:

| Asset | Compressed | Extracted to |
|---|---|---|
| Kernel (vmlinux) | gzip (~6.5 MB) | `/opt/nova/vmlinux` |
| Guest agent | gzip (~483 KB) | `/opt/nova/bin/nova-eye-agent` |
| eBPF bytecode (5 files) | raw (~7.3 KB) | `/opt/nova/ebpf/` |
| Default config | raw (~0.5 KB) | `/etc/nova/nova.toml` |

Total binary size with assets: ~26 MB.

## Build Targets

| Target | Toolchain | Output |
|---|---|---|
| Workspace crates | `x86_64-unknown-linux-gnu` | `nova` unified binary |
| eBPF programs | `bpfel-unknown-none` (nightly, build-std=core) | 5 ELF bytecode files |
| Guest agent | `x86_64-unknown-linux-musl` | Static binary for initrd |
