# Policy Enforcement

NovaVM integrates [Open Policy Agent (OPA)](https://www.openpolicyagent.org/) for both admission control and runtime enforcement.

## Two Layers of Policy

### 1. Admission Control

Checked when a sandbox is created (`nova run`):

- **vCPU limit** — Max vCPUs per sandbox
- **Memory limit** — Max memory per sandbox
- **Sandbox count** — Max concurrent sandboxes
- **Image allowlist** — Only permitted images can run

```toml
[policy]
admission_enabled = true
max_vcpus = 8
max_memory_mib = 8192
max_sandboxes = 100
allowed_images = ["nginx:alpine", "python:3.11-slim"]
```

If a request violates admission policy, `CreateSandbox` returns an error and the VM is never started.

### 2. Runtime Enforcement

Applied to every eBPF event flowing through the sensor pipeline:

| Action | Behavior |
|---|---|
| `allow` | Event passes through (default) |
| `alert` | Event passes + logged with ALERT level |
| `deny` | Event is blocked (not forwarded to gRPC stream) |
| `kill` | Sandbox is terminated |

### Builtin Rulesets

Three builtin rulesets via `enforcement_rules`:

**`"default"`** — Balanced security:
- Alert on `process_exec`
- Allow `file_open`, `net_connect`

**`"strict"`** — High security:
- Alert on `process_exec`, `file_open`
- Deny `net_connect` (no outbound connections)

**`"none"`** — No enforcement (all events pass through)

### Custom Rules

Add custom rules in `nova.toml`:

```toml
[[policy.rules]]
event_type = "process_exec"
action = "alert"

[[policy.rules]]
event_type = "file_open"
action = "deny"

[[policy.rules]]
event_type = "net_connect"
action = "kill"
```

## OPA Wasm Bundles

For complex policy logic, load compiled OPA Wasm bundles:

### 1. Write Policy (Rego)

```rego
package nova.sandbox

default allow = false

allow {
    input.image == "nginx:alpine"
    input.vcpus <= 4
    input.memory_mib <= 512
}

deny[msg] {
    input.vcpus > 4
    msg := "too many vCPUs requested"
}
```

### 2. Compile to Wasm

```bash
opa build -t wasm -e nova/sandbox/allow -e nova/sandbox/deny policy.rego
# Produces: bundle.tar.gz containing policy.wasm
```

### 3. Load Bundle

```bash
nova policy load bundle.tar.gz --id my-policy
```

### 4. Evaluate

```bash
nova policy eval nova/sandbox/allow '{"image":"nginx:alpine","vcpus":2}'
# Output: allowed=true

nova policy eval nova/sandbox/allow '{"image":"malware:latest","vcpus":16}'
# Output: allowed=false, reason="too many vCPUs requested"
```

## CLI Commands

```bash
# List loaded policy bundles
nova policy list

# Load a Wasm bundle
nova policy load /path/to/bundle.wasm --id my-policy

# Remove a bundle
nova policy remove my-policy

# Evaluate a policy decision
nova policy eval <policy_path> '<json_input>'

# Show policy engine status
nova policy status
```

## Configuration Reference

```toml
[policy]
# Admission control
admission_enabled = true          # Check policy on sandbox creation
max_vcpus = 8                     # Max vCPUs per sandbox
max_memory_mib = 8192             # Max memory per sandbox
max_sandboxes = 100               # Max concurrent sandboxes
allowed_images = []               # Image allowlist (empty = allow all)

# Runtime enforcement
enforcement_enabled = true        # Check policy on eBPF events
enforcement_rules = "default"     # "default" | "strict" | "none"
bundle_dir = "/var/lib/nova/policy/bundles"

# Custom rules (appended after builtin)
# [[policy.rules]]
# event_type = "process_exec"
# action = "alert"
```

## Audit Trail

All events are logged to `events.jsonl` **before** enforcement. This means:
- The JSONL file captures everything (complete audit trail)
- The gRPC `StreamEvents` stream is **post-enforcement** (filtered)
- Denied events appear in JSONL but not in the gRPC stream

This ensures no events are silently lost — you can always reconstruct what happened from the JSONL log.

## Real-World Example

From E2E testing with a real VM running nginx:alpine:

```
Policy: enforcement_rules = "default" (alert on process_exec)

Results:
  34,722 file_open events  -> ALLOW (passed through)
  1,067  process_exec      -> ALERT (logged + forwarded)
  0      events denied
  0      sandboxes killed
```

Guest VM events are tagged with `sandbox_id`, so enforcement can be per-sandbox.
