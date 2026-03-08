# Installation

## System Requirements

| Requirement | Details |
|---|---|
| **OS** | Linux (x86_64) with KVM support |
| **KVM** | `/dev/kvm` accessible, `CONFIG_KVM` + `CONFIG_KVM_INTEL` or `CONFIG_KVM_AMD` |
| **WSL2** | Supported with nested virtualization enabled |
| **Memory** | 512MB+ available for guest VMs |
| **Disk** | 1GB+ for image cache |

### Check KVM Support

```bash
# Check if KVM is available
ls -la /dev/kvm

# If not available on WSL2, enable nested virtualization:
# In PowerShell (admin): Set-VMProcessor -VMName WSL -ExposeVirtualizationExtensions $true
```

## Build from Source

### 1. Install Rust Nightly

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup toolchain install nightly-2025-01-15
rustup target add x86_64-unknown-linux-gnu
rustup target add x86_64-unknown-linux-musl  # for guest agent
```

### 2. Build Workspace

```bash
cd novavm

# Build all crates (produces single unified binary)
cargo build --release --target x86_64-unknown-linux-gnu
```

This produces:
- `target/x86_64-unknown-linux-gnu/release/nova` — Unified binary (daemon + CLI)

### 3. Package Embedded Assets (Optional)

To embed the kernel, eBPF bytecode, and guest agent into the binary:

```bash
# Copy and compress assets into crates/novactl/assets/
sudo ./scripts/package-assets.sh

# Rebuild with embedded assets
cargo build --release --target x86_64-unknown-linux-gnu
```

The resulting binary (~26 MB) is fully self-contained.

### 4. Build eBPF Programs (Optional)

Required only if you want host/guest eBPF observability.

```bash
cd crates/nova-eye-ebpf
cargo +nightly build -Z build-std=core --target bpfel-unknown-none --release
```

Produces eBPF bytecode in `target/bpfel-unknown-none/release/`:
- `nova-eye-process` — Process exec tracepoint
- `nova-eye-file` — File open kprobe
- `nova-eye-network` — TCP connect kprobe
- `nova-eye-http` — SSL_write uprobe
- `nova-eye-http-read` — SSL_read uprobe

### 5. Build Guest Agent (Optional)

Required only for guest-side eBPF telemetry.

```bash
cd crates/nova-eye-agent
cargo build --target x86_64-unknown-linux-musl --release
```

Produces: `target/x86_64-unknown-linux-musl/release/nova-eye-agent`

## Install

### Option A: With Embedded Assets

```bash
# Install binary (assets already embedded)
sudo cp target/x86_64-unknown-linux-gnu/release/nova /usr/local/bin/

# Extract embedded assets and generate config
sudo nova setup

# Start daemon
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml
```

### Option B: Manual Install

```bash
# Binary
sudo cp target/x86_64-unknown-linux-gnu/release/nova /usr/local/bin/

# Kernel
sudo mkdir -p /opt/nova
sudo cp tests/fixtures/vmlinux-5.10 /opt/nova/vmlinux

# eBPF bytecode (optional)
sudo mkdir -p /opt/nova/ebpf
sudo cp target/bpfel-unknown-none/release/nova-eye-* /opt/nova/ebpf/

# Guest agent (optional)
sudo mkdir -p /opt/nova/bin
sudo cp target/x86_64-unknown-linux-musl/release/nova-eye-agent /opt/nova/bin/

# Config
sudo mkdir -p /etc/nova
sudo cp config/nova.toml /etc/nova/nova.toml

# Runtime directories
sudo mkdir -p /var/lib/nova/images /var/run/nova /run/nova /var/lib/nova/policy/bundles
```

## Directory Layout

After installation:

```
/usr/local/bin/
    nova                 # Unified binary (daemon + CLI)

/opt/nova/
    vmlinux              # Guest kernel
    ebpf/                # eBPF bytecode (optional)
        nova-eye-process
        nova-eye-file
        nova-eye-network
        nova-eye-http
        nova-eye-http-read
    bin/
        nova-eye-agent   # Guest eBPF agent (optional)

/etc/nova/
    nova.toml            # Daemon configuration

/var/lib/nova/
    images/              # L1 blob cache + L2 rootfs cache
        snapshots/       # L3 VM snapshots
    policy/
        bundles/         # OPA Wasm policy bundles

/run/nova/
    nova.sock            # gRPC Unix domain socket

/var/run/nova/
    events.jsonl         # eBPF event audit log
```

## Windows Installation

NovaVM supports Windows through WSL2 with a native Windows CLI.

### 1. Build the Windows CLI

```powershell
cd novavm\desktop
cargo build --release
# Binary: target\release\nova.exe (~771 KB)

# Add to PATH
copy target\release\nova.exe C:\Users\%USERNAME%\.local\bin\nova.exe
```

### 2. Install NovaVM in WSL

```bash
# Inside WSL:
cd /mnt/c/path/to/novavm
cargo build --release --target x86_64-unknown-linux-gnu
sudo cp target/x86_64-unknown-linux-gnu/release/nova /usr/local/bin/
sudo nova setup
```

### 3. Setup and Start

```powershell
# From Windows:
nova setup    # Check prerequisites, create config
nova start    # Launch daemon in WSL
nova status   # Verify everything is running
```

See [Windows guide](windows.md) for full details, command reference, and troubleshooting.

### Cross-Compiling from Windows

If developing the Linux daemon on Windows with WSL2:

```bash
# From Windows, compile inside WSL:
wsl -e bash -lc "cd /mnt/c/path/to/novavm && cargo build --release"

# Run daemon inside WSL:
wsl -u root -e bash -c "RUST_LOG=info nova serve --config /etc/nova/nova.toml"
```

## Verify Installation

```bash
# Check KVM access
ls /dev/kvm

# Check binary
nova --help

# List embedded assets (if built with package-assets.sh)
nova setup --list

# Start daemon
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml &

# Test
nova ps
```
