#!/bin/bash
# Download OCI image fixtures for Stage 3 tests.
# Requires: skopeo (apt install skopeo)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Downloading alpine:3.19 OCI layout ==="
skopeo copy --override-os linux --override-arch amd64 \
    docker://alpine:3.19 \
    "oci:${SCRIPT_DIR}/alpine-oci:latest"
echo "Done: ${SCRIPT_DIR}/alpine-oci (~3 MB)"

echo ""
echo "=== Downloading nginx:1.25-alpine OCI layout ==="
skopeo copy --override-os linux --override-arch amd64 \
    docker://nginx:1.25-alpine \
    "oci:${SCRIPT_DIR}/nginx-oci:latest"
echo "Done: ${SCRIPT_DIR}/nginx-oci (~70 MB)"

echo ""
echo "All OCI fixtures downloaded."
