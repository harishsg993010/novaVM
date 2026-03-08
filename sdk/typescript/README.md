# NovaVM TypeScript SDK

Run code in secure KVM microVM sandboxes. Zero runtime dependencies.

## Install

```bash
npm install novavm
```

**Requires:** `nova serve` running on the host (REST API on port 9800). Node.js >= 18.

## Quick Start

```typescript
import { Sandbox } from "novavm";

const sandbox = await Sandbox.create();
await sandbox.runCode("x = 1");
const result = await sandbox.runCode("x += 1; x");
console.log(result.text); // "2"
await sandbox.destroy();
```

## API

### Create a Sandbox

```typescript
// Default: python:3.11-slim
const sb = await Sandbox.create();

// Custom image and resources
const sb = await Sandbox.create({
  image: "python:3.11-slim",
  vcpus: 2,
  memory: 512,
  name: "my-sandbox",
});
```

### Execute Commands

```typescript
const result = await sb.exec("ls -la /");
console.log(result.stdout);
console.log(result.exitCode);
```

### Run Code (Persistent State)

```typescript
// Variables persist between calls
await sb.runCode("import math");
await sb.runCode("x = math.sqrt(144)");
const result = await sb.runCode("x");
console.log(result.text); // "12.0"

// Bare expressions auto-print
const list = await sb.runCode("[i**2 for i in range(5)]");
console.log(list.text); // "[0, 1, 4, 9, 16]"
```

### File Operations

```typescript
await sb.writeFile("/tmp/data.txt", "hello world");
const content = await sb.readFile("/tmp/data.txt");
console.log(content); // "hello world"
```

### Cleanup

```typescript
await sb.stop();    // Stop gracefully
await sb.destroy(); // Stop + remove
```

## Options

| Option    | Default                 | Description              |
|-----------|-------------------------|--------------------------|
| `image`   | `python:3.11-slim`      | OCI image reference      |
| `vcpus`   | `1`                     | Virtual CPUs             |
| `memory`  | `256`                   | Memory in MiB            |
| `name`    | auto-generated          | Sandbox name             |
| `baseUrl` | `http://localhost:9800` | Daemon REST API URL      |
| `timeout` | `30`                    | HTTP timeout (seconds)   |

## Transport

The SDK connects to `nova serve` via its REST API (HTTP/JSON). Uses built-in `fetch` — no gRPC libraries, no protobuf, no external dependencies.
