# Windows Usage (WSL Backend)

NovaVM runs on Windows through WSL2. A native Windows CLI (`nova.exe`) manages the NovaVM daemon running inside WSL, communicating over the REST API on `http://127.0.0.1:9800`.

```
┌─────────────────────────────────────────────────────────┐
│  Windows                                                │
│                                                         │
│  nova.exe (native Windows CLI)                          │
│      │                                                  │
│      │  HTTP REST API (port 9800)                       │
│      ▼                                                  │
│  ┌─────────────────────────────────────────────────┐    │
│  │  WSL2 (Linux)                                   │    │
│  │                                                 │    │
│  │  nova serve (daemon)                            │    │
│  │      │                                          │    │
│  │      ├── KVM VMs (sandboxes)                    │    │
│  │      ├── eBPF probes                            │    │
│  │      ├── OPA policy engine                      │    │
│  │      └── REST API on 0.0.0.0:9800               │    │
│  └─────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

## Prerequisites

| Requirement | Details |
|---|---|
| **Windows** | Windows 10 21H2+ or Windows 11 |
| **WSL2** | `wsl --install` (Ubuntu recommended) |
| **KVM** | Nested virtualization enabled (see below) |
| **Rust** | Stable toolchain (for building the Windows CLI) |
| **nova binary** | Built and installed inside WSL (see [Installation](installation.md)) |

### Enable Nested Virtualization

KVM inside WSL2 requires nested virtualization. In PowerShell (admin):

```powershell
# Check WSL version (must be WSL 2)
wsl --version

# Enable nested virtualization
Set-VMProcessor -VMName WSL -ExposeVirtualizationExtensions $true

# Restart WSL
wsl --shutdown
```

Verify inside WSL:

```bash
ls /dev/kvm   # Should exist
```

## Install

### 1. Build the Windows CLI

From the `desktop/` directory (on Windows):

```powershell
cd novavm\desktop
cargo build --release
```

The binary is at `target\release\nova.exe` (~771 KB, zero runtime dependencies).

Copy it somewhere on your PATH:

```powershell
copy target\release\nova.exe C:\Users\%USERNAME%\.local\bin\nova.exe
```

### 2. Install NovaVM in WSL

Inside WSL, build and install the Linux daemon:

```bash
cd /mnt/c/path/to/novavm
cargo build --release --target x86_64-unknown-linux-gnu
sudo cp target/x86_64-unknown-linux-gnu/release/nova /usr/local/bin/
sudo nova setup   # Extract kernel, eBPF, config
```

### 3. Run Setup

From Windows (PowerShell, cmd, or terminal):

```
nova setup
```

This checks all prerequisites: WSL availability, KVM support, nova binary in WSL, kernel, config, TAP device. It creates any missing directories or config files automatically.

Example output:

```
[*] Setting up NovaVM in WSL...

  Checking WSL... ok
  Checking KVM... ok
  Checking nova binary... /usr/local/bin/nova
  Checking kernel... /opt/nova/vmlinux
  Creating directories... ok
  Checking config... /etc/nova/nova.toml
  Checking TAP device... creating... tap0 UP

[+] Setup complete!
    Start daemon:  nova start
    Run sandbox:   nova run nginx:alpine --name web
    List running:  nova ps
```

## Quick Start

```
# Start daemon in WSL (background)
nova start

# Check status
nova status

# Run a sandbox
nova run nginx:alpine --name web

# List sandboxes
nova ps

# Execute command inside sandbox
nova exec web cat /etc/os-release

# View console output
nova logs web

# Stop and remove
nova stop-sandbox web
nova rm web

# Stop daemon
nova stop
```

## Command Reference

### Daemon Management

These commands manage the NovaVM daemon running inside WSL.

| Command | Description |
|---|---|
| `nova start` | Start the daemon in WSL (creates TAP, launches `nova serve`) |
| `nova stop` | Stop the daemon (`pkill -f 'nova serve'`) |
| `nova status` | Show WSL, daemon, and sandbox status |
| `nova setup` | Check prerequisites, create config, TAP device |

```
# Start with custom config (path inside WSL)
nova start --config /etc/nova/nova-custom.toml

# Check status
nova status
# Output:
#   WSL:        running
#   Daemon:     running
#   API:        http://127.0.0.1:9800
#   Sandboxes:  2 total, 1 running
#   WSL kernel: 5.15.167.4-microsoft-standard-WSL2
#   Cache:      156M
```

### Sandbox Management

These commands communicate with the daemon over the REST API.

| Command | Description |
|---|---|
| `nova run <IMAGE>` | Create and start a sandbox |
| `nova ps` | List sandboxes |
| `nova exec <ID> <CMD...>` | Execute command in a sandbox |
| `nova stop-sandbox <ID>` | Stop a sandbox |
| `nova rm <ID>` | Remove a sandbox |
| `nova logs <ID>` | View sandbox console output |
| `nova inspect <ID>` | Show sandbox or image details |

```
# Run with options
nova run nginx:alpine --name web --vcpus 2 --memory 512

# Run with custom entrypoint
nova run python:3.11-slim --name py --cmd "python -c 'print(42)'"

# Execute (supports flags with hyphens)
nova exec web uname -a
nova exec web ls -la /usr/share/nginx/html

# Inspect (tries sandbox first, then image)
nova inspect web
```

### Image Management

| Command | Description |
|---|---|
| `nova pull <IMAGE>` | Pull OCI image from registry |
| `nova images` | List cached images |

```
nova pull nginx:alpine
nova pull ghcr.io/owner/image:tag
nova images
```

### Observability

| Command | Description |
|---|---|
| `nova events` | Show recent eBPF events |
| `nova events -f` | Stream events in real-time |
| `nova events -n 50` | Show last 50 events |

```
# Last 20 events (default)
nova events

# Stream live events (Ctrl+C to stop)
nova events -f

# Output format:
#   [host]            process_exec   pid=1234   comm=nginx
#   [guest:web]       file_open      pid=1      comm=init
```

Events are read from `/var/run/nova/events.jsonl` inside WSL via `wsl -e tail`.

### Global Options

```
# Use a different API endpoint
nova --api http://192.168.1.100:9800 ps

# Default is http://127.0.0.1:9800
```

## How It Works

The Windows CLI (`nova.exe`) uses two communication channels:

1. **REST API** (`std::net::TcpStream`) — For sandbox operations (run, ps, exec, stop, etc.). Connects to `127.0.0.1:9800` where the WSL daemon listens. Raw HTTP/1.1, no TLS (localhost only).

2. **WSL commands** (`wsl.exe`) — For daemon lifecycle (start, stop, setup) and file access (events, logs). Runs `wsl -e bash -c "..."` subprocesses.

The daemon inside WSL binds to `0.0.0.0:9800`, which is accessible from Windows via `127.0.0.1:9800` (WSL2 port forwarding).

## Differences from Linux CLI

| Feature | Linux `nova` | Windows `nova.exe` |
|---|---|---|
| **Daemon** | `nova serve` (runs locally) | `nova start` (launches in WSL) |
| **Transport** | gRPC (Unix socket) + REST | REST only (HTTP) |
| **Stop sandbox** | `nova stop <id>` | `nova stop-sandbox <id>` |
| **Stop daemon** | Ctrl+C / `pkill` | `nova stop` |
| **Events** | `tail -f events.jsonl` | `nova events -f` (via WSL) |
| **sudo** | User runs sudo directly | CLI handles sudo (prompts if needed) |

Note: `nova stop` on Windows stops the daemon (not a sandbox). Use `nova stop-sandbox <id>` to stop a sandbox.

## Troubleshooting

### "connection failed (127.0.0.1:9800)"

The daemon is not running or WSL port forwarding is broken.

```
# Check daemon status
nova status

# Start daemon
nova start

# If still failing, restart WSL
wsl --shutdown
nova start
```

### "wsl exec failed"

WSL is not installed or not running.

```powershell
# Check WSL
wsl --version

# Install WSL
wsl --install

# List distributions
wsl --list --verbose
```

### KVM not available

```powershell
# Enable nested virtualization (PowerShell admin)
Set-VMProcessor -VMName WSL -ExposeVirtualizationExtensions $true
wsl --shutdown

# Verify inside WSL
wsl -e ls /dev/kvm
```

### IPv6 connection issues

Windows may resolve `localhost` to `::1` (IPv6) while the daemon only binds IPv4. The CLI defaults to `127.0.0.1:9800` to avoid this. If you override `--api`, use `127.0.0.1` instead of `localhost`.

### Git Bash path expansion

When using Git Bash, paths starting with `/` get expanded (e.g., `/etc/os-release` becomes `C:/Program Files/Git/etc/os-release`). This affects `nova exec` commands:

```bash
# In Git Bash — this may fail:
nova exec web cat /etc/os-release

# Workaround — use PowerShell or cmd instead
# Or prefix with //:
nova exec web cat //etc/os-release
```

### sudo password prompt

The Windows CLI runs `sudo -n` first (passwordless). If that fails, it prompts for your WSL user password. To avoid the prompt, configure passwordless sudo in WSL:

```bash
# Inside WSL:
echo "$USER ALL=(ALL) NOPASSWD: ALL" | sudo tee /etc/sudoers.d/$USER
```

### "Access is denied" when rebuilding

If `nova.exe` is still running, the compiler cannot overwrite it:

```powershell
# Kill running instance
taskkill /F /IM nova.exe

# Rebuild
cargo build --release
```

### Daemon fails to start

```
# Check daemon log
wsl -e tail -20 /tmp/nova-daemon.log

# Common issues:
# - Port 9800 already in use
# - Missing /etc/nova/nova.toml
# - Missing /opt/nova/vmlinux kernel
# Run setup to fix:
nova setup
```
