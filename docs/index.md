# NovaVM Documentation

NovaVM is a lightweight microVM hypervisor that runs OCI container images inside real KVM virtual machines with eBPF observability and OPA policy enforcement.

## Why NovaVM?

| | Docker | Firecracker | NovaVM |
|---|---|---|---|
| **Isolation** | Shared kernel (namespaces) | KVM VM | KVM VM |
| **Cold boot** | ~9.6s | ~3.4s | **~2.0s** |
| **Warm boot** | ~2.0s | ~86ms | **~69ms** |
| **Observability** | None | None | eBPF (host + guest) |
| **Policy** | None | None | OPA admission + runtime |
| **Caching** | Layer cache | External | 4-level (L1-L4) |
| **Image format** | OCI | Raw rootfs | OCI (auto-converts) |

*Benchmarks on same WSL2 hardware with nginx:alpine. See [benchmark.md](../benchmark.md).*

## How It Works

```
nova run nginx:alpine
    |
    v
nova serve (REST + gRPC daemon)
    |
    +-- Pull OCI image from Docker Hub/GHCR/Quay
    +-- Build rootfs from layers (L1/L2 cache)
    +-- Build initramfs with guest agent
    +-- Boot KVM VM with virtio-net
    +-- Load eBPF probes (host + guest)
    +-- Enforce OPA policy (admission + runtime)
    +-- Stream events to JSONL audit log
```

Each sandbox runs in its own KVM virtual machine with a dedicated Linux kernel — not just a namespace on the host kernel.

## Single Binary

NovaVM ships as a single `nova` binary that combines daemon, CLI, and optionally embeds the kernel, eBPF bytecode, and guest agent:

```bash
nova serve   # Start daemon (REST API on :9800, gRPC on Unix socket)
nova setup   # Extract embedded assets to /opt/nova/
nova run     # Create and start a sandbox
nova ps      # List sandboxes
nova exec    # Execute command in a sandbox
```

## Windows Support

A native Windows CLI (`nova.exe`) manages the NovaVM daemon running in WSL2 via the REST API. See [Windows guide](windows.md) for setup instructions.

```powershell
nova setup    # Check prerequisites
nova start    # Launch daemon in WSL
nova run nginx:alpine --name web
nova ps
nova stop
```

## SDKs

| SDK | Install | Transport | Dependencies |
|---|---|---|---|
| [Python](../sdk/python/) | `pip install novavm` | REST API | None (stdlib only) |
| [TypeScript](../sdk/typescript/) | `npm install novavm` | REST API | None (built-in fetch) |

## Documentation

| Guide | Description |
|---|---|
| [Quick Start](quickstart.md) | Get running in 5 minutes |
| [Installation](installation.md) | Build from source, prerequisites |
| [Configuration](configuration.md) | `nova.toml` reference |
| [CLI Reference](cli-reference.md) | All `nova` commands |
| [Architecture](architecture.md) | Crate map, data flow, internals |
| [Networking](networking.md) | TAP setup, guest networking |
| [Snapshots](snapshots.md) | L3 snapshot save/restore, warm boot |
| [Observability](observability.md) | eBPF probes, events, JSONL logs |
| [Policy](policy.md) | OPA admission + runtime enforcement |
| [Images](images.md) | OCI image pulling, L1/L2 caching |
| [Windows](windows.md) | Windows CLI with WSL2 backend |
| [Troubleshooting](troubleshooting.md) | Common issues + fixes |

## Project Stats

- **Language:** Rust (nightly)
- **Crates:** 15 workspace members
- **Tests:** 343 (unit + integration)
- **Binary:** Single `nova` (~26 MB with embedded assets)
- **License:** LGPL-2.1
