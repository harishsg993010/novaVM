# NovaVM Python SDK

Run code in secure KVM microVM sandboxes. Zero dependencies.

## Install

```bash
pip install novavm
```

**Requires:** `nova serve` running on the host (REST API on port 9800).

## Quick Start

```python
from novavm import Sandbox

# Create a sandbox and run code
with Sandbox() as sandbox:
    sandbox.run_code("x = 1")
    execution = sandbox.run_code("x += 1; x")
    print(execution.text)  # outputs 2
```

## API

### Sandbox

```python
# Default: python:3.11-slim image
sb = Sandbox()

# Custom image and resources
sb = Sandbox(
    image="python:3.11-slim",
    vcpus=2,
    memory=512,
    name="my-sandbox",
)

# Context manager (auto start + destroy)
with Sandbox() as sb:
    ...

# Manual lifecycle
sb = Sandbox()
sb.start()
# ... use sandbox ...
sb.stop()
sb.destroy()
```

### Execute Commands

```python
with Sandbox(image="alpine:latest") as sb:
    result = sb.exec("ls -la /")
    print(result.stdout)
    print(result.exit_code)
```

### Run Code (Persistent State)

```python
with Sandbox() as sb:
    # Variables persist between calls
    sb.run_code("import math")
    sb.run_code("x = math.sqrt(144)")
    result = sb.run_code("x")
    print(result.text)  # 12.0

    # Bare expressions auto-print (like Python REPL)
    result = sb.run_code("[i**2 for i in range(5)]")
    print(result.text)  # [0, 1, 4, 9, 16]
```

### File Operations

```python
with Sandbox() as sb:
    sb.write_file("/tmp/data.txt", "hello world")
    content = sb.read_file("/tmp/data.txt")
    print(content)  # hello world
```

## Configuration

| Parameter  | Default                  | Description              |
|------------|--------------------------|--------------------------|
| `image`    | `python:3.11-slim`       | OCI image reference      |
| `vcpus`    | `1`                      | Virtual CPUs             |
| `memory`   | `256`                    | Memory in MiB            |
| `name`     | auto-generated           | Sandbox name             |
| `base_url` | `http://localhost:9800`  | Daemon REST API URL      |
| `timeout`  | `30`                     | HTTP timeout (seconds)   |

## Transport

The SDK connects to `nova serve` via its REST API (HTTP/JSON). No gRPC libraries, no protobuf, no CLI binary required.
