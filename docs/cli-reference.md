# CLI Reference

`nova` is the unified binary for NovaVM. It serves as both the daemon and the CLI client. The CLI communicates with the daemon over its REST API (port 9800) or gRPC (Unix socket).

## Global Options

```
nova [OPTIONS] <COMMAND>

Options:
  --socket <PATH>     Unix socket path [default: /run/nova/nova.sock]
  --format <FORMAT>   Output format: table | json [default: table]
  -h, --help          Print help
```

## Daemon Commands

### `serve` — Start the daemon

```bash
nova serve [OPTIONS]

# Options:
#   --config <PATH>     Config file path [default: /etc/nova/nova.toml]

# Examples:
sudo nova serve --config /etc/nova/nova.toml
sudo RUST_LOG=info nova serve
```

Starts the NovaVM runtime daemon. Listens on:
- **REST API** — HTTP/JSON on port 9800 (configurable via `api_port`)
- **gRPC** — Unix domain socket at `/run/nova/nova.sock`

On first run with embedded assets, auto-extracts kernel, eBPF bytecode, and agent to `/opt/nova/`.

### `setup` — Manage embedded assets

```bash
nova setup [OPTIONS]

# Options:
#   --list     Show embedded assets and their sizes
#   --force    Overwrite existing files during extraction

# Examples:
nova setup --list           # Show what's embedded
sudo nova setup             # Extract assets to /opt/nova/
sudo nova setup --force     # Re-extract, overwriting existing files
```

Extracts kernel, eBPF bytecode, guest agent, and default config from the binary to their system paths. Only available when built with `./scripts/package-assets.sh`.

## Sandbox Commands

### `run` — Create and start a sandbox

```bash
nova run <IMAGE> [OPTIONS]

# Options:
#   --name <ID>         Sandbox name/ID (auto-generated if omitted)
#   --vcpus <N>         Number of vCPUs [default: 1]
#   --memory <MiB>      Memory in MiB [default: 128]
#   --cmd <COMMAND>     Override entrypoint command

# Examples:
nova run nginx:alpine
nova run nginx:alpine --name web --vcpus 2 --memory 256
nova run python:3.11-slim --cmd "python -c 'print(1+1)'"
nova run ghcr.io/owner/image:tag
```

Pulls the image automatically if not cached locally.

### `ps` — List sandboxes

```bash
nova ps [OPTIONS]

# Options:
#   -a, --all    Show all sandboxes (including stopped)

# Example output:
# ID          IMAGE           STATE     CREATED
# web         nginx:alpine    running   2026-03-07T10:30:00Z
# test        alpine:latest   stopped   2026-03-07T10:25:00Z
```

### `stop` — Stop a sandbox

```bash
nova stop <SANDBOX> [OPTIONS]

# Options:
#   -t, --timeout <SECONDS>   Graceful shutdown timeout [default: 10]

nova stop web
nova stop web -t 30
```

### `kill` — Force-kill a sandbox

```bash
nova kill <SANDBOX>

# Equivalent to: nova stop <SANDBOX> -t 0
```

### `rm` — Remove a sandbox

```bash
nova rm <SANDBOX> [OPTIONS]

# Options:
#   -f, --force   Stop the sandbox first if running

nova rm web
nova rm web -f    # stop + remove
```

### `exec` — Execute command in sandbox

```bash
nova exec <SANDBOX> <COMMAND...>

# Examples:
nova exec web cat /etc/os-release
nova exec web ls -la /usr/share/nginx/html
nova exec web curl http://localhost:80
```

Returns exit code and stdout. Uses serial console protocol (no SSH required).

### `logs` — View sandbox console output

```bash
nova logs <SANDBOX> [OPTIONS]

# Options:
#   -f, --follow     Stream continuously
#   -n, --lines <N>  Number of lines [default: 100]

nova logs web
nova logs web -f       # tail -f style
nova logs web -n 50
```

### `shell` — Interactive console

```bash
nova shell <IMAGE> [OPTIONS]

# Options:
#   --vcpus <N>      Number of vCPUs [default: 1]
#   --memory <MiB>   Memory in MiB [default: 256]
#   --cmd <COMMAND>  Override entrypoint

# Examples:
nova shell alpine:latest
nova shell ubuntu:22.04 --vcpus 2 --memory 512
```

Pulls image, boots VM, and attaches stdin/stdout bidirectionally. Ctrl-C to exit.

## Image Commands

### `pull` — Pull OCI image

```bash
nova pull <IMAGE>

# Supported registries:
nova pull nginx:alpine              # Docker Hub
nova pull docker.io/library/nginx   # Docker Hub (explicit)
nova pull ghcr.io/owner/image:tag   # GitHub Container Registry
nova pull quay.io/org/image:latest  # Quay.io
nova pull registry.example.com/image@sha256:abc123  # Digest ref
```

Downloads manifest + layers, verifies SHA-256, converts to rootfs.

### `images` — List cached images

```bash
nova images

# Example output:
# IMAGE              DIGEST          SIZE     FORMAT
# nginx:alpine       sha256:abc123   44 MB    rootfs
# python:3.11-slim   sha256:def456   120 MB   rootfs
```

### `inspect` — Inspect image or sandbox

```bash
nova inspect <TARGET>

# Tries image first, falls back to sandbox
nova inspect nginx:alpine    # image metadata
nova inspect web             # sandbox status
```

## Policy Commands

### `policy list` — List loaded bundles

```bash
nova policy list

# Example output:
# BUNDLE_ID    POLICIES   LOADED_AT              DIGEST
# default      3          2026-03-07T10:00:00Z   sha256:abc123
```

### `policy load` — Load OPA Wasm bundle

```bash
nova policy load <PATH> [OPTIONS]

# Options:
#   --id <BUNDLE_ID>   Bundle identifier (auto-generated if omitted)

nova policy load /path/to/policy.wasm --id my-policy
```

### `policy remove` — Remove a bundle

```bash
nova policy remove <BUNDLE_ID>

nova policy remove my-policy
```

### `policy eval` — Evaluate a policy

```bash
nova policy eval <POLICY_PATH> <INPUT_JSON>

nova policy eval nova/sandbox/allow '{"image":"nginx:alpine","vcpus":2}'
```

### `policy status` — Policy engine status

```bash
nova policy status

# Example output:
# Loaded bundles:    2
# Total evaluations: 1,234
# Denied:            12
# Avg eval time:     0.3ms
```

## JSON Output

Add `--format json` to any command for machine-readable output:

```bash
nova ps --format json
nova images --format json
nova inspect web --format json
nova policy status --format json
```

## REST API

The daemon exposes a REST API on port 9800 (configurable via `api_port` in `nova.toml`):

```bash
# Health check
curl http://localhost:9800/health

# List sandboxes
curl http://localhost:9800/api/v1/sandboxes

# Create sandbox
curl -X POST http://localhost:9800/api/v1/sandboxes \
  -H 'Content-Type: application/json' \
  -d '{"sandbox_id":"web","image":"nginx:alpine","vcpus":1,"memory":128}'

# Execute command
curl -X POST http://localhost:9800/api/v1/sandboxes/web/exec \
  -H 'Content-Type: application/json' \
  -d '{"command":"uname -a"}'

# Stop sandbox
curl -X POST http://localhost:9800/api/v1/sandboxes/web/stop

# Delete sandbox
curl -X DELETE http://localhost:9800/api/v1/sandboxes/web
```

## Socket Path

By default, the CLI connects to `/run/nova/nova.sock` for gRPC. Override with:

```bash
nova --socket /tmp/nova/nova.sock ps
```

The daemon socket path is set in `nova.toml` under `[daemon].socket`.

## Windows CLI (`nova.exe`)

On Windows, a separate native CLI manages the NovaVM daemon running in WSL2 via the REST API. It provides daemon lifecycle commands not found in the Linux CLI:

```powershell
nova setup              # Check prerequisites, create config + TAP
nova start              # Launch daemon in WSL (background)
nova stop               # Stop daemon
nova status             # Show WSL + daemon + sandbox status

nova run nginx:alpine --name web    # Same as Linux
nova ps                             # Same as Linux
nova exec web uname -a              # Same as Linux
nova stop-sandbox web               # Stop a sandbox (not the daemon)
nova rm web                         # Same as Linux
nova events -f                      # Tail events.jsonl via WSL
```

Build from `desktop/`:

```powershell
cd desktop
cargo build --release
# target\release\nova.exe (~771 KB)
```

See [Windows guide](windows.md) for full details.
