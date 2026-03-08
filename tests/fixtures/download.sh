#!/usr/bin/env bash
# Download Firecracker CI vmlinux kernel for integration testing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL="vmlinux-5.10"
URL="https://s3.amazonaws.com/spec.ccfc.min/ci-artifacts/kernels/x86_64/vmlinux-5.10.217"

if [ -f "$SCRIPT_DIR/$KERNEL" ]; then
    echo "$KERNEL already downloaded."
    exit 0
fi

echo "Downloading $KERNEL ..."
curl -fsSL -o "$SCRIPT_DIR/$KERNEL" "$URL"
echo "Downloaded $KERNEL ($(du -h "$SCRIPT_DIR/$KERNEL" | cut -f1))."
