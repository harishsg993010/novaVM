#!/bin/bash
# Setup TAP networking for NovaVM nginx demo.
# Run this with: sudo bash scripts/setup-network.sh

set -e

TAP_DEV="nova-tap0"
HOST_IP="172.16.0.1/24"

echo "=== NovaVM Network Setup ==="

# Clean up any existing TAP
ip link show $TAP_DEV >/dev/null 2>&1 && {
    echo "Removing existing $TAP_DEV..."
    ip link set $TAP_DEV down 2>/dev/null
    ip tuntap del dev $TAP_DEV mode tap 2>/dev/null
}

# Create TAP device
echo "Creating TAP device $TAP_DEV..."
ip tuntap add dev $TAP_DEV mode tap
ip addr add $HOST_IP dev $TAP_DEV
ip link set $TAP_DEV up

# Enable IP forwarding
echo 1 > /proc/sys/net/ipv4/ip_forward

echo ""
echo "TAP device configured:"
ip addr show $TAP_DEV
echo ""
echo "=== Ready! Now start the daemon with: ==="
echo "NOVA_SOCKET=/tmp/nova/run/nova.sock NOVA_IMAGE_DIR=/tmp/nova/images NOVA_KERNEL=\$(pwd)/tests/fixtures/vmlinux-5.10 NOVA_TAP=nova-tap0 RUST_LOG=info ./target/x86_64-unknown-linux-gnu/release/nova-daemon"
