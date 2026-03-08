# OCI Images

NovaVM pulls and runs standard OCI container images from any Docker-compatible registry.

## Supported Registries

| Registry | Example |
|---|---|
| Docker Hub | `nginx:alpine`, `docker.io/library/python:3.11` |
| GitHub (GHCR) | `ghcr.io/owner/image:tag` |
| Quay.io | `quay.io/org/image:latest` |
| Custom | `registry.example.com/image:tag` |
| Digest refs | `nginx@sha256:abc123...` |

## Pull Images

```bash
# Pull from Docker Hub (short name)
nova pull nginx:alpine

# Pull from GHCR
nova pull ghcr.io/owner/image:v1.0

# Pull by digest
nova pull nginx@sha256:abc123def456...

# List cached images
nova images

# Inspect image metadata
nova inspect nginx:alpine
```

## How Pulling Works

```
nova pull nginx:alpine
    |
    v
[1] Parse image reference
    -> registry: registry-1.docker.io
    -> repository: library/nginx
    -> tag: alpine
    |
[2] Authenticate
    -> GET /v2/ -> 401 -> parse WWW-Authenticate
    -> GET token from auth.docker.io
    |
[3] Fetch manifest
    -> GET /v2/library/nginx/manifests/alpine
    -> If manifest list: resolve to linux/amd64
    -> GET platform-specific manifest
    |
[4] Download layers
    -> For each layer:
        -> Check L1 cache (SHA-256 digest)
        -> If miss: GET /v2/.../blobs/<digest>
        -> Stream download with SHA-256 verification
        -> Atomic write to L1 cache
    |
[5] Build rootfs (L2 cache)
    -> Extract layers in order (tar.gz)
    -> Apply whiteout files (.wh.*)
    -> Store as directory tree
    |
[6] Store OCI layout
    -> oci-layout, index.json, manifest.json
    -> blobs/sha256/ (content-addressable)
```

## Caching

### L1: Blob Store

Content-addressable OCI layer blobs, keyed by SHA-256 digest.

```
/var/lib/nova/images/blobs/sha256/
    abc123...   (layer 1, gzipped tar)
    def456...   (layer 2, gzipped tar)
```

Layers are shared across images — if `nginx:alpine` and `python:alpine` share the same base layer, it's stored once.

### L2: Rootfs Cache

Pre-built filesystem trees, ready to be packed into initramfs.

```
/var/lib/nova/images/rootfs/<image_digest>/
    bin/
    etc/
    usr/
    ...
```

On cache hit, the rootfs is **copied** (not hardlinked — hardlinks cause inode corruption with NTFS/WSL2).

## Image to VM Flow

NovaVM doesn't use container runtimes. Instead, it converts the OCI rootfs into a Linux initramfs:

```
OCI rootfs directory
    |
    v
Pack into cpio archive (initramfs)
    + Inject /init script
    + Inject nova-eye-agent (if eBPF enabled)
    + Inject eBPF bytecode (if probes configured)
    |
    v
Load as initrd into KVM VM
    |
    v
Linux kernel boots, runs /init
    + Mounts procfs, sysfs, devtmpfs
    + Configures networking (if TAP)
    + Starts nova-eye-agent (if present)
    + Execs container entrypoint (e.g., nginx)
```

## Auto-Pull on Run

`nova run` automatically pulls the image if not cached:

```bash
# These are equivalent:
nova pull nginx:alpine && nova run nginx:alpine --name web
nova run nginx:alpine --name web  # pulls automatically if needed
```

## Image Management

```bash
# List all cached images
nova images

# Remove a cached image (frees L1 + L2 cache)
# (via gRPC RemoveImage RPC)

# Inspect image metadata
nova inspect nginx:alpine
```

## Limitations

- Only `linux/amd64` platform is supported
- No multi-arch image building
- No image push (pull-only)
- No private registry auth (token-based public registries only)
- Whiteout file handling is basic
