# Quick Start

Get NovaVM running in 5 minutes.

## Prerequisites

- Linux with KVM support (bare metal or WSL2 with nested KVM)
- Rust nightly toolchain
- Root access (for KVM and TAP)

> **Windows users:** See the [Windows quick start](windows.md) instead — a native `nova.exe` manages the daemon in WSL2 for you.

## 1. Build

```bash
# Clone
git clone https://github.com/harishsg993010/novaVM.git
cd novavm

# Build the unified binary
cargo build --release --target x86_64-unknown-linux-gnu

# Binary is at:
#   target/x86_64-unknown-linux-gnu/release/nova
```

## 2. Install

```bash
# Copy binary
sudo cp target/x86_64-unknown-linux-gnu/release/nova /usr/local/bin/

# Copy kernel
sudo mkdir -p /opt/nova
sudo cp tests/fixtures/vmlinux-5.10 /opt/nova/vmlinux

# Create directories
sudo mkdir -p /etc/nova /var/lib/nova/images /var/run/nova /run/nova

# Create minimal config
sudo tee /etc/nova/nova.toml > /dev/null << 'EOF'
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
EOF
```

### Alternative: Embedded Assets

If you built with embedded assets (`./scripts/package-assets.sh` before build), first-run setup is automatic:

```bash
sudo nova setup          # Extracts kernel, eBPF, agent to /opt/nova/
```

## 3. Start the Daemon

```bash
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml
```

## 4. Run Your First Sandbox

Open a new terminal:

```bash
# Pull and run nginx
nova run nginx:alpine --name my-nginx

# Check status
nova ps

# Execute a command inside the VM
nova exec my-nginx cat /etc/os-release

# View console output
nova logs my-nginx

# Stop and remove
nova stop my-nginx
nova rm my-nginx
```

## 5. Enable Networking (Optional)

To access services running inside the VM (e.g., nginx on port 80):

```bash
# Setup TAP device
sudo bash scripts/setup-network.sh

# Add tap_device to config
# In /etc/nova/nova.toml under [daemon]:
#   tap_device = "tap0"

# Restart daemon, then:
nova run nginx:alpine --name web

# Access from host
curl http://172.16.0.2:80
```

## 6. Use the SDKs (Optional)

```python
# Python (zero dependencies)
pip install novavm

from novavm import Sandbox
with Sandbox() as sb:
    result = sb.run_code("1 + 1")
    print(result.text)  # "2"
```

```typescript
// TypeScript (zero dependencies, Node >= 18)
npm install novavm

import { Sandbox } from "novavm";
const sb = await Sandbox.create();
const result = await sb.runCode("1 + 1");
console.log(result.text); // "2"
await sb.destroy();
```

## What's Next?

- [Windows guide](windows.md) for using NovaVM on Windows with WSL2
- [Enable eBPF observability](observability.md) for real-time process/file/network events
- [Configure OPA policy](policy.md) to enforce security rules
- [Use snapshots](snapshots.md) for 69ms warm boot times
- [Full CLI reference](cli-reference.md) for all commands
- [Python SDK](../sdk/python/README.md) / [TypeScript SDK](../sdk/typescript/README.md) for programmatic access
