#!/bin/bash
# Package NovaVM assets for embedding into the nova binary.
#
# Copies kernel, eBPF bytecode, and guest agent from their system locations
# into crates/novactl/assets/ (gzip-compressed where appropriate).
#
# Usage:
#   sudo ./scripts/package-assets.sh
#   # Then build:
#   cargo build --release -p novactl

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ASSETS_DIR="$REPO_DIR/crates/novactl/assets"

mkdir -p "$ASSETS_DIR"

echo "=== NovaVM Asset Packager ==="
echo "Output: $ASSETS_DIR"
echo

# ── Kernel ──────────────────────────────────────────────────────────
# Prefer the eBPF-capable kernel; fall back to the basic one.
KERNEL_EBPF="/opt/nova/vmlinux-5.10-ebpf"
KERNEL_BASIC="/opt/nova/vmlinux-5.10"

if [ -f "$KERNEL_EBPF" ]; then
    KERNEL="$KERNEL_EBPF"
    echo "  using eBPF-capable kernel ($KERNEL)"
    # Strip debug symbols first (440MB → ~25MB).
    STRIPPED=$(mktemp)
    cp "$KERNEL" "$STRIPPED"
    strip --strip-all "$STRIPPED" 2>/dev/null || true
    echo "  compressing stripped kernel..."
    gzip -9 -c "$STRIPPED" > "$ASSETS_DIR/vmlinux.gz"
    ORIG_SIZE=$(stat -c%s "$KERNEL" 2>/dev/null || stat -f%z "$KERNEL")
    STRIP_SIZE=$(stat -c%s "$STRIPPED" 2>/dev/null || stat -f%z "$STRIPPED")
    GZ_SIZE=$(stat -c%s "$ASSETS_DIR/vmlinux.gz" 2>/dev/null || stat -f%z "$ASSETS_DIR/vmlinux.gz")
    echo "    $ORIG_SIZE → $STRIP_SIZE (stripped) → $GZ_SIZE (gzipped)"
    rm -f "$STRIPPED"
elif [ -f "$KERNEL_BASIC" ]; then
    KERNEL="$KERNEL_BASIC"
    echo "  using basic kernel ($KERNEL) — no eBPF support"
    echo "  compressing kernel..."
    gzip -9 -c "$KERNEL" > "$ASSETS_DIR/vmlinux.gz"
    ORIG_SIZE=$(stat -c%s "$KERNEL" 2>/dev/null || stat -f%z "$KERNEL")
    GZ_SIZE=$(stat -c%s "$ASSETS_DIR/vmlinux.gz" 2>/dev/null || stat -f%z "$ASSETS_DIR/vmlinux.gz")
    echo "    $ORIG_SIZE → $GZ_SIZE bytes"
else
    echo "  SKIP kernel (not found)"
fi

# ── Guest agent ─────────────────────────────────────────────────────
AGENT="/opt/nova/bin/nova-eye-agent"
if [ -f "$AGENT" ]; then
    echo "  compressing agent ($AGENT)..."
    gzip -9 -c "$AGENT" > "$ASSETS_DIR/nova-eye-agent.gz"
    ORIG_SIZE=$(stat -c%s "$AGENT" 2>/dev/null || stat -f%z "$AGENT")
    GZ_SIZE=$(stat -c%s "$ASSETS_DIR/nova-eye-agent.gz" 2>/dev/null || stat -f%z "$ASSETS_DIR/nova-eye-agent.gz")
    echo "    $ORIG_SIZE → $GZ_SIZE bytes"
else
    echo "  SKIP agent (not found at $AGENT)"
fi

# ── eBPF bytecode ───────────────────────────────────────────────────
EBPF_DIR="/opt/nova/ebpf"
for prog in nova-eye-process nova-eye-network nova-eye-file nova-eye-http nova-eye-http-read; do
    SRC="$EBPF_DIR/$prog"
    if [ -f "$SRC" ]; then
        cp "$SRC" "$ASSETS_DIR/$prog"
        SIZE=$(stat -c%s "$SRC" 2>/dev/null || stat -f%z "$SRC")
        echo "  copied $prog ($SIZE bytes)"
    else
        echo "  SKIP $prog (not found)"
    fi
done

echo
echo "=== Done ==="
echo
echo "Assets in $ASSETS_DIR:"
ls -lh "$ASSETS_DIR/"
echo
echo "Now build: cargo build --release -p novactl"
echo "Binary will be at: target/release/nova"
