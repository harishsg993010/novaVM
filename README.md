# NovaVM

A lightweight microVM hypervisor that runs OCI container images inside real KVM virtual machines with eBPF observability and OPA policy enforcement.

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

*Benchmarks on same WSL2 hardware with nginx:alpine. See [benchmark.md](benchmark.md).*

## Quick Start

```bash
# Build
cargo build --release --target x86_64-unknown-linux-gnu

# Package assets (kernel, eBPF, agent) into the binary
sudo ./scripts/package-assets.sh
cargo build --release --target x86_64-unknown-linux-gnu

# Install
sudo cp target/x86_64-unknown-linux-gnu/release/nova /usr/local/bin/

# First run auto-extracts embedded assets
sudo nova setup
sudo nova serve --config /etc/nova/nova.toml
```

In another terminal:

```bash
nova run nginx:alpine --name web
nova exec web curl http://localhost:80
nova ps
nova stop web
```

## Single Binary

NovaVM ships as a single `nova` binary that includes:

- **Daemon** (`nova serve`) — KVM runtime, REST + gRPC APIs, sensor pipeline
- **CLI** (`nova run/ps/exec/...`) — sandbox management commands
- **Embedded assets** (optional) — kernel, eBPF bytecode, guest agent

```bash
nova serve --config /etc/nova/nova.toml   # Start daemon
nova setup --list                          # Show embedded assets
nova run python:3.11-slim                  # Run a sandbox
nova exec <id> uname -a                   # Execute in sandbox
nova policy status                         # Check policy engine
```

## SDKs

| SDK | Install | Dependencies |
|---|---|---|
| [Python](sdk/python/) | `pip install novavm` | None (stdlib only) |
| [TypeScript](sdk/typescript/) | `npm install novavm` | None (built-in fetch) |

```python
from novavm import Sandbox

with Sandbox() as sb:
    sb.run_code("x = 1 + 1")
    print(sb.run_code("x").text)  # "2"
```

```typescript
import { Sandbox } from "novavm";

const sb = await Sandbox.create();
const result = await sb.runCode("1 + 1");
console.log(result.text); // "2"
await sb.destroy();
```

## Windows Support

NovaVM runs on Windows through WSL2. A native Windows CLI (`nova.exe`) manages the daemon in WSL via the REST API:

```powershell
# Build the Windows CLI
cd desktop
cargo build --release

# Setup (checks WSL, KVM, installs config)
nova setup

# Start daemon in WSL
nova start

# Use it like Docker
nova run nginx:alpine --name web
nova exec web cat /etc/os-release
nova ps
nova events -f
nova stop
```

See [Windows guide](docs/windows.md) for full setup instructions.

## Features

- **KVM isolation** — Each sandbox runs in its own VM with a dedicated Linux kernel
- **OCI images** — Pull from Docker Hub, GHCR, Quay.io, any OCI-compliant registry
- **4-level caching** — L1 blob store, L2 rootfs cache, L3 VM snapshots, L4 pre-warmed pool
- **eBPF observability** — Real-time process, file, network events from host and guest kernels
- **OPA policy** — Admission control + runtime enforcement with Wasm bundles
- **REST + gRPC APIs** — HTTP/JSON on port 9800, gRPC on Unix socket
- **69ms warm boot** — Demand-paged snapshot restore with virtio force-activate
- **Zero-dep SDKs** — Python and TypeScript SDKs with no external dependencies
- **Windows support** — Native CLI manages WSL2 backend over HTTP

## Documentation

| Guide | Description |
|---|---|
| [Quick Start](docs/quickstart.md) | Get running in 5 minutes |
| [Installation](docs/installation.md) | Build from source, prerequisites |
| [Configuration](docs/configuration.md) | `nova.toml` reference |
| [CLI Reference](docs/cli-reference.md) | All `nova` commands |
| [Architecture](docs/architecture.md) | Crate map, data flow, internals |
| [Networking](docs/networking.md) | TAP setup, guest networking |
| [Snapshots](docs/snapshots.md) | L3 snapshot save/restore, warm boot |
| [Observability](docs/observability.md) | eBPF probes, events, JSONL logs |
| [Policy](docs/policy.md) | OPA admission + runtime enforcement |
| [Images](docs/images.md) | OCI image pulling, L1/L2 caching |
| [Windows](docs/windows.md) | Windows CLI with WSL2 backend |
| [Troubleshooting](docs/troubleshooting.md) | Common issues + fixes |

## Project Stats

- **Language:** Rust (nightly)
- **Crates:** 15 workspace members
- **Tests:** 343 (unit + integration)
- **Binary:** Single `nova` (~26 MB with embedded assets)
- **License:** LGPL-2.1
