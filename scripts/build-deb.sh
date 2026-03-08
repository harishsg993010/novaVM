#!/bin/bash
# Build NovaVM .deb package
# Usage: bash scripts/build-deb.sh
# Requires: cargo, x86_64-unknown-linux-gnu target

set -e

VERSION="0.1.0"
PKG="novavm_${VERSION}_amd64"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="/tmp/${PKG}"

echo "=== Building NovaVM v${VERSION} .deb package ==="

# Step 1: Build binaries
echo "[1/5] Building binaries..."
cd "$ROOT"
cargo build --release --target x86_64-unknown-linux-gnu -p nova-api -p novactl

RELEASE_DIR="$ROOT/target/x86_64-unknown-linux-gnu/release"

if [ ! -f "$RELEASE_DIR/nova-daemon" ] || [ ! -f "$RELEASE_DIR/novactl" ]; then
    echo "ERROR: Build failed. Binaries not found."
    exit 1
fi

# Step 2: Create package structure
echo "[2/5] Creating package structure..."
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/DEBIAN"
mkdir -p "$BUILD_DIR/usr/bin"
mkdir -p "$BUILD_DIR/etc/nova"
mkdir -p "$BUILD_DIR/opt/nova"
mkdir -p "$BUILD_DIR/lib/systemd/system"

# Step 3: Copy files
echo "[3/5] Copying files..."

# Binaries
cp "$RELEASE_DIR/nova-daemon" "$BUILD_DIR/usr/bin/"
cp "$RELEASE_DIR/novactl" "$BUILD_DIR/usr/bin/"
chmod 755 "$BUILD_DIR/usr/bin/nova-daemon"
chmod 755 "$BUILD_DIR/usr/bin/novactl"

# Kernel
if [ -f "$ROOT/tests/fixtures/vmlinux-5.10" ]; then
    cp "$ROOT/tests/fixtures/vmlinux-5.10" "$BUILD_DIR/opt/nova/vmlinux-5.10"
else
    echo "WARNING: Kernel not found at tests/fixtures/vmlinux-5.10"
    echo "         Package will not include kernel. Set daemon.kernel in nova.toml manually."
fi

# Config
cp "$ROOT/config/nova.toml" "$BUILD_DIR/etc/nova/nova.toml"

# Systemd service
cp "$ROOT/novavm.service" "$BUILD_DIR/lib/systemd/system/novavm.service"

# Debian control files
cp "$ROOT/debian/control" "$BUILD_DIR/DEBIAN/control"
cp "$ROOT/debian/postinst" "$BUILD_DIR/DEBIAN/postinst"
cp "$ROOT/debian/prerm" "$BUILD_DIR/DEBIAN/prerm"
cp "$ROOT/debian/conffiles" "$BUILD_DIR/DEBIAN/conffiles"
chmod 755 "$BUILD_DIR/DEBIAN/postinst"
chmod 755 "$BUILD_DIR/DEBIAN/prerm"

# Calculate installed size (KB)
SIZE=$(du -sk "$BUILD_DIR" | cut -f1)
sed -i "/^Architecture/a Installed-Size: ${SIZE}" "$BUILD_DIR/DEBIAN/control"

# Step 4: Build .deb
echo "[4/5] Building .deb..."
dpkg-deb --build "$BUILD_DIR" "$ROOT/${PKG}.deb"

# Step 5: Verify
echo "[5/5] Verifying..."
dpkg-deb --info "$ROOT/${PKG}.deb"
echo ""
echo "=== Package built: ${PKG}.deb ==="
echo "  Size: $(du -h "$ROOT/${PKG}.deb" | cut -f1)"
echo ""
echo "  Install: sudo dpkg -i ${PKG}.deb"
echo "  Start:   sudo systemctl start novavm"
echo "  Run:     novactl run nginx:alpine"

# Cleanup
rm -rf "$BUILD_DIR"
